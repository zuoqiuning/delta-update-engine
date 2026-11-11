use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};

use crate::publisher::cos_signer::{
    encode_path, key_time_now, sign_v5, validate_bucket, validate_region,
};
use crate::publisher::Publisher;

pub struct CosPublisher {
    client: reqwest::blocking::Client,
    host: String,
    secret_id: String,
    secret_key: String,
    token: Option<String>,
}

impl CosPublisher {
    pub fn new(
        bucket: &str,
        region: String,
        secret_id: String,
        secret_key: String,
        token: Option<String>,
    ) -> anyhow::Result<Self> {
        validate_bucket(bucket)?;
        validate_region(&region)?;
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()
            .context("build COS http client")?;
        Ok(Self {
            client,
            host: format!("{bucket}.cos.{region}.myqcloud.com"),
            secret_id,
            secret_key,
            token: token.filter(|value| !value.is_empty()),
        })
    }

    fn object_path(&self, key: &str) -> anyhow::Result<String> {
        validate_object_key(key)?;
        Ok(encode_path(&format!("/{key}")))
    }

    fn headers(&self) -> BTreeMap<String, String> {
        let mut headers = BTreeMap::from([("host".to_string(), self.host.clone())]);
        if let Some(token) = &self.token {
            headers.insert("x-cos-security-token".to_string(), token.clone());
        }
        headers
    }

    fn authorization(&self, method: &str, path: &str) -> anyhow::Result<String> {
        sign_v5(
            &self.secret_id,
            &self.secret_key,
            method,
            path,
            &BTreeMap::new(),
            &self.headers(),
            &key_time_now(),
        )
    }

    fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<reqwest::blocking::Body>,
    ) -> anyhow::Result<reqwest::blocking::Response> {
        let path = self.object_path(key)?;
        let url = format!("https://{}{}", self.host, path);
        let authorization = self.authorization(method.as_str(), &path)?;
        let mut request = self
            .client
            .request(method, url)
            .header(reqwest::header::AUTHORIZATION, authorization);
        if let Some(token) = &self.token {
            request = request.header("x-cos-security-token", token);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().context("COS request")?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            bail!("COS returned {status}: {detail}");
        }
        Ok(response)
    }
}

impl Publisher for CosPublisher {
    fn put(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()> {
        self.request(reqwest::Method::PUT, key, Some(bytes.to_vec().into()))?;
        Ok(())
    }

    fn put_file(&self, key: &str, source: &Path) -> anyhow::Result<()> {
        let file =
            std::fs::File::open(source).with_context(|| format!("open {}", source.display()))?;
        self.request(reqwest::Method::PUT, key, Some(file.into()))?;
        Ok(())
    }

    fn head(&self, key: &str) -> anyhow::Result<Option<u64>> {
        let path = self.object_path(key)?;
        let url = format!("https://{}{}", self.host, path);
        let authorization = self.authorization("head", &path)?;
        let mut request = self
            .client
            .head(url)
            .header(reqwest::header::AUTHORIZATION, authorization);
        if let Some(token) = &self.token {
            request = request.header("x-cos-security-token", token);
        }
        let response = request.send().context("COS HEAD")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("COS HEAD returned {}", response.status());
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
        let path = self.object_path(key)?;
        let url = format!("https://{}{}", self.host, path);
        let authorization = self.authorization("get", &path)?;
        let mut request = self
            .client
            .get(url)
            .header(reqwest::header::AUTHORIZATION, authorization);
        if let Some(token) = &self.token {
            request = request.header("x-cos-security-token", token);
        }
        let mut response = request.send().context("COS GET")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            bail!("COS GET returned {}", response.status());
        }
        let mut bytes = Vec::new();
        response
            .read_to_end(&mut bytes)
            .context("read COS response")?;
        Ok(Some(bytes))
    }
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
