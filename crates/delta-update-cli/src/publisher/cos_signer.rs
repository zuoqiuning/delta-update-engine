use std::collections::BTreeMap;

use anyhow::bail;
use hmac::{Hmac, Mac};
use sha1::{Digest, Sha1};

type HmacSha1 = Hmac<Sha1>;

/// COS REST signature v5.
///
/// Inputs are the request method, the URL-encoded path (starting with `/`),
/// query pairs and headers with lowercase names. The returned value is the
/// complete `Authorization` header.
pub fn sign_v5(
    secret_id: &str,
    secret_key: &str,
    method: &str,
    path: &str,
    query: &BTreeMap<String, String>,
    headers: &BTreeMap<String, String>,
    key_time: &str,
) -> anyhow::Result<String> {
    let (header_list, header_string) = encoded_headers(headers);
    let (param_list, param_string) = encoded_query(query);
    let http_string = format!(
        "{}\n{}\n{}\n{}\n",
        method.to_ascii_lowercase(),
        path,
        param_string,
        header_string
    );
    let signature = signature(secret_key, key_time, &http_string)?;
    Ok(format!(
        "q-sign-algorithm=sha1&q-ak={}&q-sign-time={key_time}&q-key-time={key_time}&q-header-list={header_list}&q-url-param-list={param_list}&q-signature={signature}",
        encode(secret_id)
    ))
}

/// `HMAC-SHA1(secret_key, key_time)`, lowercase hex.
pub fn signing_key(secret_key: &str, key_time: &str) -> String {
    hmac_sha1_hex(secret_key.as_bytes(), key_time.as_bytes())
}

/// `HMAC-SHA1(sign_key, "sha1\n{key_time}\n{sha1(HttpString)}\n")`.
pub fn signature(secret_key: &str, key_time: &str, http_string: &str) -> anyhow::Result<String> {
    let sign_key = signing_key(secret_key, key_time);
    let string_to_sign = format!(
        "sha1\n{key_time}\n{}\n",
        hex::encode(Sha1::digest(http_string.as_bytes()))
    );
    Ok(hmac_sha1_hex(
        sign_key.as_bytes(),
        string_to_sign.as_bytes(),
    ))
}

pub fn encoded_headers(headers: &BTreeMap<String, String>) -> (String, String) {
    let mut list = Vec::with_capacity(headers.len());
    let mut pairs = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        let name = encode(&name.to_ascii_lowercase());
        list.push(name.clone());
        pairs.push(format!("{name}={}", encode(value)));
    }
    (list.join(";"), pairs.join("&"))
}

pub fn encoded_query(query: &BTreeMap<String, String>) -> (String, String) {
    let mut list = Vec::with_capacity(query.len());
    let mut pairs = Vec::with_capacity(query.len());
    for (name, value) in query {
        let name = encode(name);
        list.push(name.clone());
        pairs.push(format!("{name}={}", encode(value)));
    }
    (list.join(";"), pairs.join("&"))
}

/// Percent-encode the path for both the URL and the signature input.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte == b'/' || is_unreserved(byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_unreserved(byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~')
}

fn hmac_sha1_hex(key: &[u8], data: &[u8]) -> String {
    let mut mac = HmacSha1::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

/// KeyTime helper: valid from 60s ago for one hour.
pub fn key_time_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("{};{}", now.saturating_sub(60), now + 3600)
}

/// Validate a bucket name before it is embedded in a hostname.
pub fn validate_bucket(bucket: &str) -> anyhow::Result<()> {
    if bucket.is_empty()
        || bucket.len() > 128
        || !bucket
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("invalid COS bucket name: {bucket}");
    }
    Ok(())
}

pub fn validate_region(region: &str) -> anyhow::Result<()> {
    if region.is_empty()
        || !region
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        bail!("invalid COS region: {region}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Example from the COS signature v5 documentation (upload object).
    const SECRET_KEY: &str = "BQYIM75p8x0iWVFSIgqEKwFprpRSVHlz";
    const KEY_TIME: &str = "1557989151;1557996351";

    #[test]
    fn signing_key_matches_documented_vector() {
        assert_eq!(
            signing_key(SECRET_KEY, KEY_TIME),
            "eb2519b498b02ac213cb1f3d1a3d27a3b3c9bc5f"
        );
    }

    #[test]
    fn header_encoding_matches_documented_vector() {
        let headers = BTreeMap::from([
            ("content-length".to_string(), "13".to_string()),
            (
                "content-md5".to_string(),
                "mQ/fVh815F3k6TAUm8m0eg==".to_string(),
            ),
            ("content-type".to_string(), "text/plain".to_string()),
            (
                "date".to_string(),
                "Thu, 16 May 2019 06:45:51 GMT".to_string(),
            ),
            (
                "host".to_string(),
                "examplebucket-1250000000.cos.ap-beijing.myqcloud.com".to_string(),
            ),
            ("x-cos-acl".to_string(), "private".to_string()),
            (
                "x-cos-grant-read".to_string(),
                "uin=\"100000000011\"".to_string(),
            ),
        ]);
        let (list, encoded) = encoded_headers(&headers);
        assert_eq!(
            list,
            "content-length;content-md5;content-type;date;host;x-cos-acl;x-cos-grant-read"
        );
        assert_eq!(
            encoded,
            "content-length=13&content-md5=mQ%2FfVh815F3k6TAUm8m0eg%3D%3D&content-type=text%2Fplain&date=Thu%2C%2016%20May%202019%2006%3A45%3A51%20GMT&host=examplebucket-1250000000.cos.ap-beijing.myqcloud.com&x-cos-acl=private&x-cos-grant-read=uin%3D%22100000000011%22"
        );
    }

    #[test]
    fn signature_matches_documented_http_string() {
        let http_string = "put\n/exampleobject(腾讯云)\n\ncontent-length=13&content-md5=mQ%2FfVh815F3k6TAUm8m0eg%3D%3D&content-type=text%2Fplain&date=Thu%2C%2016%20May%202019%2006%3A45%3A51%20GMT&host=examplebucket-1250000000.cos.ap-beijing.myqcloud.com&x-cos-acl=private&x-cos-grant-read=uin%3D%22100000000011%22\n";
        let signature = signature(SECRET_KEY, KEY_TIME, http_string).unwrap();
        // The documentation redacts the last four characters as `1234`.
        assert_eq!(signature, "3b8851a11a569213c17ba8fa7dcf2abec6935172");
    }

    #[test]
    fn path_encoding_keeps_slashes_and_escapes_spaces() {
        assert_eq!(
            encode_path("/artifacts/full/1.0.0/My App.exe"),
            "/artifacts/full/1.0.0/My%20App.exe"
        );
    }

    #[test]
    fn rejects_unsafe_bucket_and_region_names() {
        assert!(validate_bucket("examplebucket-1250000000").is_ok());
        assert!(validate_bucket("Example Bucket").is_err());
        assert!(validate_region("ap-guangzhou").is_ok());
        assert!(validate_region("ap guangzhou").is_err());
    }
}
