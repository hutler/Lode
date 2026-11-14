//! AWS Signature Version 4 for S3 requests, with `sha2` and HMAC written out here (no AWS SDK).
//!
//! The canonical request is built from the method, the already encoded path, the query
//! pairs (encoded and sorted here), the signed headers (lower-case names, values trimmed with
//! inner runs of spaces collapsed, sorted) and the payload hash.

use sha2::{Digest, Sha256};

/// The hash of an empty payload.
pub const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
/// The payload hash S3 accepts for a body that is not hashed.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// Access key, secret and an optional session token.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    /// Access key id.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Session token of temporary credentials, sent as `x-amz-security-token`.
    pub session_token: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"***")
            .field("session_token", &self.session_token.as_ref().map(|_| "***"))
            .finish()
    }
}

/// One request to sign.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// `GET`, `PUT`, …
    pub method: &'a str,
    /// The path exactly as sent, already percent-encoded, such as `/bucket/a%20b`.
    pub path: &'a str,
    /// Query pairs, not yet encoded.
    pub query: &'a [(&'a str, &'a str)],
    /// Headers to sign (`host` and `x-amz-date` among them), any case.
    pub headers: &'a [(&'a str, &'a str)],
    /// Hex SHA-256 of the body, or [`UNSIGNED_PAYLOAD`].
    pub payload_hash: &'a str,
    /// `YYYYMMDDTHHMMSSZ`, the value of `x-amz-date`.
    pub amz_date: &'a str,
    /// Region, such as `us-east-1`.
    pub region: &'a str,
    /// Service, `s3`.
    pub service: &'a str,
}

/// Percent-encodes everything but the unreserved characters `A-Z a-z 0-9 - . _ ~`, and `/`
/// too when `slash` is true.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(char::from(b))
            }
            b'/' if keep_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The query string as sent and signed: pairs encoded and sorted by key, then value.
pub fn canonical_query(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn trim_value(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn signed_headers(req: &Request<'_>) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), trim_value(v)))
        .collect();
    h.sort();
    h
}

/// The canonical request.
pub fn canonical_request(req: &Request<'_>) -> String {
    let headers = signed_headers(req);
    let canon_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let names = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        req.path,
        canonical_query(req.query),
        canon_headers,
        names,
        req.payload_hash
    )
}

/// HMAC-SHA256 (RFC 2104).
fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().to_vec()
}

/// The value of the `Authorization` header.
pub fn authorization(creds: &Credentials, req: &Request<'_>) -> String {
    let date = req.amz_date.get(..8).unwrap_or(req.amz_date);
    let scope = format!("{date}/{}/{}/aws4_request", req.region, req.service);
    let creq = canonical_request(req);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        req.amz_date,
        hex::encode(Sha256::digest(creq.as_bytes()))
    );
    let k_date = hmac(
        format!("AWS4{}", creds.secret_access_key).as_bytes(),
        date.as_bytes(),
    );
    let k_region = hmac(&k_date, req.region.as_bytes());
    let k_service = hmac(&k_region, req.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex::encode(hmac(&k_signing, to_sign.as_bytes()));
    let names = signed_headers(req)
        .iter()
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}",
        creds.access_key_id
    )
}

/// `YYYYMMDDTHHMMSSZ` for a time.
pub fn amz_date(t: std::time::SystemTime) -> String {
    let t = time::OffsetDateTime::from(t);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The AWS Signature Version 4 test suite (as shipped with botocore's tests): credentials,
    // date and scope are the same for every case.
    fn suite() -> Credentials {
        Credentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    fn case(
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        payload_hash: &str,
    ) -> (String, String) {
        let mut headers = vec![
            ("Host", "example.amazonaws.com"),
            ("X-Amz-Date", "20150830T123600Z"),
        ];
        headers.extend_from_slice(extra);
        let req = Request {
            method,
            path,
            query,
            headers: &headers,
            payload_hash,
            amz_date: "20150830T123600Z",
            region: "us-east-1",
            service: "service",
        };
        (canonical_request(&req), authorization(&suite(), &req))
    }

    fn sig(authz: &str) -> &str {
        authz.rsplit("Signature=").next().unwrap_or("")
    }

    #[test]
    fn get_vanilla() {
        let (creq, authz) = case("GET", "/", &[], &[], EMPTY_SHA256);
        assert_eq!(
            creq,
            "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n\
             e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            authz,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn get_vanilla_query_order_key_case() {
        let (creq, authz) = case(
            "GET",
            "/",
            &[("Param2", "value2"), ("Param1", "value1")],
            &[],
            EMPTY_SHA256,
        );
        assert!(creq.contains("\nParam1=value1&Param2=value2\n"));
        assert_eq!(
            sig(&authz),
            "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        );
    }

    #[test]
    fn get_utf8_path_and_query() {
        let path = format!("/{}", uri_encode("ሴ", true));
        let (_, authz) = case("GET", &path, &[], &[], EMPTY_SHA256);
        assert_eq!(
            sig(&authz),
            "8318018e0b0f223aa2bbf98705b62bb787dc9c0e678f255a891fd03141be5d85"
        );
        let (creq, authz) = case("GET", "/", &[("ሴ", "bar")], &[], EMPTY_SHA256);
        assert!(creq.contains("\n%E1%88%B4=bar\n"));
        assert_eq!(
            sig(&authz),
            "2cdec8eed098649ff3a119c94853b13c643bcf08f8b0a1d91e12c9027818dd04"
        );
    }

    #[test]
    fn header_values_are_trimmed() {
        let (creq, authz) = case(
            "GET",
            "/",
            &[],
            &[("My-Header1", " value1"), ("My-Header2", "\"a   b   c\"")],
            EMPTY_SHA256,
        );
        assert!(creq.contains("my-header1:value1\nmy-header2:\"a b c\"\n"));
        assert_eq!(
            sig(&authz),
            "acc3ed3afb60bb290fc8d2dd0098b9911fcaa05412b367055dee359757a9c736"
        );
    }

    #[test]
    fn post_with_a_body() {
        let body_hash = hex::encode(Sha256::digest(b"Param1=value1"));
        let (_, authz) = case(
            "POST",
            "/",
            &[],
            &[("Content-Type", "application/x-www-form-urlencoded")],
            &body_hash,
        );
        assert!(authz.contains("SignedHeaders=content-type;host;x-amz-date,"));
        assert_eq!(
            sig(&authz),
            "ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a"
        );
    }

    #[test]
    fn hmac_matches_rfc_4231() {
        // Test case 2.
        assert_eq!(
            hex::encode(hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: a key longer than the block.
        assert_eq!(
            hex::encode(hmac(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn encoding_and_dates() {
        assert_eq!(uri_encode("a b/c~d+e", true), "a%20b/c~d%2Be");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_440_938_160);
        assert_eq!(amz_date(t), "20150830T123600Z");
        assert!(!format!("{:?}", suite()).contains("EXAMPLEKEY"));
    }
}
