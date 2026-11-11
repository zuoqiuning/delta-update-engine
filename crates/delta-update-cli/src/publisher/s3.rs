use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::blocking::Body;
use sha2::{Digest, Sha256};
use url::Url;

use crate::publisher::cos_signer::encode_path;
use crate::publisher::s3_signer::{
    amz_date_now, authorization, empty_payload_hash, validate_bucket,
};
use crate::publisher::Publisher;

/// S3-compatible publisher (AWS S3, Cloudflare R2, MinIO, S3-compatible OSS).
///
/// Path-style requests are used, so a custom `S3_ENDPOINT` such as
/// `https://<account>.r2.cloudflarestorage.com` or `http://127.0.0.1:9000`
/// works without per-bucket DNS configuration.
pub struct S3Publisher {
    client: reqwest::blocking::Client,
    endpoint: Url,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
    token: Option<String>,
}

impl S3Publisher {
    pub fn new(
        bucket: &str,
        region: String,
        endpoint: Option<&str>,
        access_key: String,
        secret_key: String,
        token: Option<String>,
    ) -> anyhow::Result<Self> {
        validate_bucket(bucket)?;
        let endpoint = endpoint
            .map(str::to_string)
            .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"));
        let endpoint =
            Url::parse(&endpoint).with_context(|| format!("parse S3 endpoint {endpoint}"))?;
        if endpoint.query().is_some() {
            bail!("S3 endpoint must not contain a query string");
        }
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .context("build S3 http client")?;
        Ok(Self {
            client,
            endpoint,
            bucket: bucket.to_string(),
            region,
            access_key,
            secret_key,
            token: token.filter(|value| !value.is_empty()),
        })
    }

    fn object_url(&self, key: &str) -> anyhow::Result<(Url, String)> {
        validate_object_key(key)?;
        let base = self.endpoint.as_str().trim_end_matches('/');
        let canonical_uri = encode_path(&format!(
            "{}/{}/{}",
            self.endpoint.path().trim_end_matches('/'),
            self.bucket,
            key
        ));
        let url = Url::parse(&format!("{base}{canonical_uri}"))
            .with_context(|| format!("build S3 object URL for {key}"))?;
        Ok((url.clone(), host_header(&url)?))
    }

    fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<Body>,
        payload_hash: &str,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let (url, host) = self.object_url(key)?;
        let amz_date = amz_date_now();
        let mut headers = BTreeMap::from([
            ("host".to_string(), host),
            ("x-amz-content-sha256".to_string(), payload_hash.to_string()),
            ("x-amz-date".to_string(), amz_date.clone()),
        ]);
        if let Some(token) = &self.token {
            headers.insert("x-amz-security-token".to_string(), token.clone());
        }
        let authorization = authorization(
            &self.access_key,
            &self.secret_key,
            &self.region,
            method.as_str(),
            url.path(),
            "",
            &headers,
            payload_hash,
            &amz_date,
        )?;

        let mut request = self
            .client
            .request(method, url)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", amz_date)
            .header(reqwest::header::AUTHORIZATION, authorization);
        if let Some(token) = &self.token {
            request = request.header("x-amz-security-token", token);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        request.send().context("S3 request")
    }
}

impl Publisher for S3Publisher {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        let hash = sha256_hex(bytes);
        let response = self.request(
            reqwest::Method::PUT,
            key,
            Some(Body::from(bytes.to_vec())),
            &hash,
        )?;
        ensure_success(response, "PUT", key)
    }

    fn put_file(&self, key: &str, source: &Path) -> anyhow::Result<()> {
        let hash = hash_file(source)?;
        let file =
            std::fs::File::open(source).with_context(|| format!("open {}", source.display()))?;
        let response = self.request(reqwest::Method::PUT, key, Some(Body::from(file)), &hash)?;
        ensure_success(response, "PUT", key)
    }

    fn head(&self, key: &str) -> anyhow::Result<Option<u64>> {
        let response = self.request(reqwest::Method::HEAD, key, None, &empty_payload_hash())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("S3 HEAD {key} returned {}", response.status());
        }
        let length = response
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Ok(Some(length))
    }

    fn get(&self, key: &str) -> anyhow::Result<Vec<u8>> {
        self.get_optional(key)?
            .with_context(|| format!("object not found: {key}"))
    }

    fn get_optional(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut response = self.request(reqwest::Method::GET, key, None, &empty_payload_hash())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("S3 GET {key} returned {}", response.status());
        }
        let mut bytes = Vec::new();
        response
            .read_to_end(&mut bytes)
            .context("read S3 response")?;
        Ok(Some(bytes))
    }
}

fn ensure_success(
    response: reqwest::blocking::Response,
    method: &str,
    key: &str,
) -> anyhow::Result<()> {
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let detail = response.text().unwrap_or_default();
    bail!("S3 {method} {key} returned {status}: {detail}");
}

fn host_header(url: &Url) -> anyhow::Result<String> {
    let host = match url.host() {
        Some(url::Host::Ipv6(address)) => format!("[{address}]"),
        _ => url.host_str().unwrap_or_default().to_string(),
    };
    if host.is_empty() {
        bail!("S3 endpoint has no host");
    }
    Ok(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn validate_object_key(key: &str) -> anyhow::Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.starts_with('\\')
        || key.contains('\\')
        || key
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        bail!("invalid object key: {key}");
    }
    Ok(())
}
