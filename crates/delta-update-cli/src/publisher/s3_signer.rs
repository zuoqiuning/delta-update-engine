use std::collections::BTreeMap;

use anyhow::bail;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// AWS Signature Version 4 for the S3 REST API.
///
/// `headers` must use lowercase names and include `host`,
/// `x-amz-content-sha256` and `x-amz-date`; additional signed headers such as
/// `range` are allowed. The returned value is the `Authorization` header.
#[allow(clippy::too_many_arguments)]
pub fn authorization(
    access_key: &str,
    secret_key: &str,
    region: &str,
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    headers: &BTreeMap<String, String>,
    payload_hash: &str,
    amz_date: &str,
) -> anyhow::Result<String> {
    let (signed_headers, canonical_headers) = canonical_headers(headers);
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );
    let date = amz_date.get(..8).unwrap_or_default();
    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );

    let date_key = hmac(format!("AWS4{secret_key}").as_bytes(), date.as_bytes());
    let region_key = hmac(&date_key, region.as_bytes());
    let service_key = hmac(&region_key, b"s3");
    let signing_key = hmac(&service_key, b"aws4_request");
    let signature = hex::encode(hmac(&signing_key, string_to_sign.as_bytes()));

    Ok(format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
    ))
}

fn canonical_headers(headers: &BTreeMap<String, String>) -> (String, String) {
    let mut names = Vec::with_capacity(headers.len());
    let mut lines = String::new();
    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        names.push(name.clone());
        lines.push_str(&format!("{name}:{}\n", value.trim()));
    }
    (names.join(";"), lines)
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `YYYYMMDD'T'HHMMSS'Z'` in UTC.
pub fn amz_date_now() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// SHA-256 of an empty body, used by HEAD/GET requests.
pub fn empty_payload_hash() -> String {
    hex::encode(Sha256::digest([]))
}

pub fn validate_bucket(bucket: &str) -> anyhow::Result<()> {
    if bucket.is_empty()
        || bucket.contains(['/', '\\'])
        || bucket
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        bail!("invalid S3 bucket name: {bucket}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS S3 SigV4 documented example (GET with a Range header).
    #[test]
    fn signature_matches_the_aws_documented_vector() {
        let headers = BTreeMap::from([
            (
                "host".to_string(),
                "examplebucket.s3.amazonaws.com".to_string(),
            ),
            ("range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), empty_payload_hash()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ]);

        let authorization = authorization(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "GET",
            "/test.txt",
            "",
            &headers,
            &empty_payload_hash(),
            "20130524T000000Z",
        )
        .unwrap();

        assert!(authorization.ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ));
        assert!(authorization
            .contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
        assert!(authorization.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    #[test]
    fn rejects_unsafe_bucket_names() {
        assert!(validate_bucket("my-bucket").is_ok());
        assert!(validate_bucket("").is_err());
        assert!(validate_bucket("a/b").is_err());
        assert!(validate_bucket("a b").is_err());
    }
}
