//! Online endpoint checks: manifest, signature and object HEAD sizes.

use anyhow::{bail, Context};
use delta_update_engine::limits::Limits;
use delta_update_engine::manifest::parse::parse_manifest;

pub(crate) struct DoctorReport {
    pub(crate) endpoint: String,
    pub(crate) ok: bool,
    pub(crate) version: Option<String>,
    pub(crate) verified: bool,
    pub(crate) errors: Vec<String>,
}

pub(crate) fn check_endpoint(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    keys: Option<&[String]>,
) -> DoctorReport {
    let mut report = DoctorReport {
        endpoint: endpoint.to_string(),
        ok: true,
        version: None,
        verified: false,
        errors: Vec::new(),
    };
    let url = match url::Url::parse(endpoint) {
        Ok(url) => url,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("invalid URL: {error}"));
            return report;
        }
    };
    if !delta_update_engine::config::is_allowed_endpoint(&url, true) {
        report.ok = false;
        report
            .errors
            .push("endpoint must use https (http is allowed only on loopback)".into());
        return report;
    }
    if url.query().is_some() {
        report.ok = false;
        report.errors.push(
            "presigned endpoints are not supported by doctor; use `delta-update verify --bucket <uri>` against the bucket instead"
                .into(),
        );
        return report;
    }

    let manifest = match fetch_limited(client, endpoint, Limits::default().manifest_max_bytes) {
        Ok(bytes) => bytes,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("manifest: {error}"));
            return report;
        }
    };
    let signature_url = format!("{endpoint}.minisig");
    let signature = match fetch_limited(client, &signature_url, 64 * 1024) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("signature: {error}"));
            return report;
        }
    };
    if let Some(keys) = keys {
        if let Err(error) =
            delta_update_engine::crypto::verify_manifest(&manifest, &signature, keys)
        {
            report.ok = false;
            report.errors.push(format!("signature verify: {error}"));
            return report;
        }
        report.verified = true;
    }
    let parsed = match parse_manifest(&manifest, &Limits::default()) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.ok = false;
            report.errors.push(format!("manifest parse: {error}"));
            return report;
        }
    };
    report.version = Some(parsed.version.clone());

    let origin = url.origin().ascii_serialization();
    for (path, entry) in &parsed.files {
        if entry.delta_key.is_some() && entry.full_key.is_none() {
            report.ok = false;
            report
                .errors
                .push(format!("{path}: patch entry is missing full_key"));
        }
        for (kind, key, expected) in [
            ("full", entry.full_key.as_deref(), Some(entry.size)),
            ("delta", entry.delta_key.as_deref(), entry.delta_size),
        ] {
            let Some(key) = key else { continue };
            let object_url = format!(
                "{origin}{}",
                crate::publisher::cos_signer::encode_path(&format!("/{key}"))
            );
            match head_object(client, &object_url) {
                Ok(Some(size)) if expected == Some(size) => {}
                Ok(Some(size)) => {
                    report.ok = false;
                    report.errors.push(format!(
                        "{path} {kind} object has size {size}, expected {expected:?}"
                    ));
                }
                Ok(None) => {
                    report.ok = false;
                    report
                        .errors
                        .push(format!("{path} {kind} object is missing: {key}"));
                }
                Err(error) => {
                    report.ok = false;
                    report.errors.push(format!("{path} {kind} object: {error}"));
                }
            }
        }
    }
    report
}

fn fetch_limited(
    client: &reqwest::blocking::Client,
    url: &str,
    limit: u64,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let response = client.get(url).send().context("GET")?;
    if !response.status().is_success() {
        bail!("GET returned {}", response.status());
    }
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .context("read response")?;
    if bytes.len() as u64 > limit {
        bail!("response exceeds {limit} bytes");
    }
    Ok(bytes)
}

fn head_object(client: &reqwest::blocking::Client, url: &str) -> anyhow::Result<Option<u64>> {
    let response = client.head(url).send().context("HEAD")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        bail!("HEAD returned {}", response.status());
    }
    let length = response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("HEAD response is missing a valid Content-Length"))?;
    Ok(Some(length))
}
