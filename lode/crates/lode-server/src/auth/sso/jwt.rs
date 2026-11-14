//! ID token checks: the JWS compact form, RS256 and ES256 signatures against the provider's
//! keys, and the claims OpenID Connect requires (issuer, audience, expiry, nonce).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Why an ID token was refused. The texts are shown to the person signing in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TokenError {
    #[error("the sign-in token is malformed")]
    Malformed,
    #[error("the sign-in token is signed with {0}; Lode accepts RS256 and ES256")]
    Algorithm(String),
    #[error("the identity provider publishes no key that matches the sign-in token")]
    UnknownKey,
    #[error("the sign-in token's signature is not valid")]
    BadSignature,
    #[error("the sign-in token was issued by {0}, not by the configured provider")]
    Issuer(String),
    #[error("the sign-in token is meant for another application (audience)")]
    Audience,
    #[error("the sign-in token has expired")]
    Expired,
    #[error(
        "the sign-in token is not valid yet; check the clocks of this server and the provider"
    )]
    NotYetValid,
    #[error("the sign-in token does not belong to this sign-in (nonce)")]
    Nonce,
}

/// Clock difference allowed between this server and the provider, in seconds.
pub(crate) const SKEW_SECONDS: i64 = 120;

#[derive(Debug, Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

/// A parsed but not yet verified token.
#[derive(Debug)]
pub(crate) struct Jwt {
    alg: String,
    kid: Option<String>,
    signed: Vec<u8>,
    signature: Vec<u8>,
    /// The payload's claims.
    pub(crate) claims: Map<String, Value>,
}

fn b64(part: &str) -> Result<Vec<u8>, TokenError> {
    URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| TokenError::Malformed)
}

/// Splits and decodes a compact JWS.
pub(crate) fn parse(token: &str) -> Result<Jwt, TokenError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let header: Header = serde_json::from_slice(&b64(h)?).map_err(|_| TokenError::Malformed)?;
    let claims: Map<String, Value> =
        serde_json::from_slice(&b64(p)?).map_err(|_| TokenError::Malformed)?;
    Ok(Jwt {
        alg: header.alg,
        kid: header.kid,
        signed: format!("{h}.{p}").into_bytes(),
        signature: b64(s)?,
        claims,
    })
}

/// A JSON Web Key Set, as the provider's `jwks_uri` serves it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Jwks {
    #[serde(default)]
    pub(crate) keys: Vec<Jwk>,
}

/// One key. Members Lode does not use are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Jwk {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    use_: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

impl Jwk {
    fn fits(&self, alg: &str, kid: Option<&str>) -> bool {
        let kty_ok = match alg {
            "RS256" => self.kty == "RSA",
            "ES256" => self.kty == "EC" && self.crv.as_deref() == Some("P-256"),
            _ => false,
        };
        kty_ok
            && self.use_.as_deref().is_none_or(|u| u == "sig")
            && self.alg.as_deref().is_none_or(|a| a == alg)
            && match (kid, &self.kid) {
                (Some(want), Some(have)) => want == have,
                (Some(_), None) => false,
                (None, _) => true,
            }
    }

    fn verify(&self, alg: &str, msg: &[u8], sig: &[u8]) -> bool {
        match alg {
            "RS256" => {
                let (Some(n), Some(e)) = (&self.n, &self.e) else {
                    return false;
                };
                let (Ok(n), Ok(e)) = (b64(n), b64(e)) else {
                    return false;
                };
                RsaPublicKeyComponents { n: &n, e: &e }
                    .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                    .is_ok()
            }
            "ES256" => {
                let (Some(x), Some(y)) = (&self.x, &self.y) else {
                    return false;
                };
                let (Ok(x), Ok(y)) = (b64(x), b64(y)) else {
                    return false;
                };
                if x.len() != 32 || y.len() != 32 {
                    return false;
                }
                let mut point = Vec::with_capacity(65);
                point.push(4);
                point.extend_from_slice(&x);
                point.extend_from_slice(&y);
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, &point)
                    .verify(msg, sig)
                    .is_ok()
            }
            _ => false,
        }
    }
}

impl Jwt {
    /// Whether `keys` has a key this token could be checked with.
    pub(crate) fn has_key(&self, keys: &Jwks) -> bool {
        keys.keys
            .iter()
            .any(|k| k.fits(&self.alg, self.kid.as_deref()))
    }

    /// Checks the signature with the matching keys of `keys`.
    pub(crate) fn verify(&self, keys: &Jwks) -> Result<(), TokenError> {
        if !matches!(self.alg.as_str(), "RS256" | "ES256") {
            return Err(TokenError::Algorithm(self.alg.clone()));
        }
        let mut candidates = keys
            .keys
            .iter()
            .filter(|k| k.fits(&self.alg, self.kid.as_deref()))
            .peekable();
        if candidates.peek().is_none() {
            return Err(TokenError::UnknownKey);
        }
        if candidates.any(|k| k.verify(&self.alg, &self.signed, &self.signature)) {
            Ok(())
        } else {
            Err(TokenError::BadSignature)
        }
    }
}

/// What the claims must say.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Expect<'a> {
    pub(crate) issuer: &'a str,
    pub(crate) client_id: &'a str,
    pub(crate) nonce: &'a str,
    /// Now, in seconds since the Unix epoch.
    pub(crate) now: i64,
}

fn seconds(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

/// Checks issuer, audience, authorized party, expiry, issue time and nonce.
pub(crate) fn check_claims(claims: &Map<String, Value>, e: Expect<'_>) -> Result<(), TokenError> {
    let iss = claims.get("iss").and_then(Value::as_str).unwrap_or("");
    if iss != e.issuer {
        return Err(TokenError::Issuer(iss.to_owned()));
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    if !audiences.contains(&e.client_id) {
        return Err(TokenError::Audience);
    }
    if let Some(azp) = claims.get("azp").and_then(Value::as_str) {
        if azp != e.client_id {
            return Err(TokenError::Audience);
        }
    }
    let exp = seconds(claims.get("exp")).ok_or(TokenError::Malformed)?;
    if exp + SKEW_SECONDS <= e.now {
        return Err(TokenError::Expired);
    }
    for early in ["iat", "nbf"] {
        if seconds(claims.get(early)).is_some_and(|t| t - SKEW_SECONDS > e.now) {
            return Err(TokenError::NotYetValid);
        }
    }
    let nonce = claims.get("nonce").and_then(Value::as_str);
    if nonce != Some(e.nonce) {
        return Err(TokenError::Nonce);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use serde_json::json;

    fn enc(v: &Value) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
    }

    fn ec_key() -> (EcdsaKeyPair, Jwks) {
        let rng = SystemRandom::new();
        let pk8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let kp =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pk8.as_ref(), &rng).unwrap();
        let p = kp.public_key().as_ref();
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("k1".into()),
            crv: Some("P-256".into()),
            x: Some(URL_SAFE_NO_PAD.encode(&p[1..33])),
            y: Some(URL_SAFE_NO_PAD.encode(&p[33..65])),
            ..Jwk::default()
        };
        (kp, Jwks { keys: vec![jwk] })
    }

    fn sign(kp: &EcdsaKeyPair, header: &Value, claims: &Value) -> String {
        let input = format!("{}.{}", enc(header), enc(claims));
        let sig = kp.sign(&SystemRandom::new(), input.as_bytes()).unwrap();
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }

    fn claims(now: i64) -> Value {
        json!({"iss": "https://idp", "aud": "lode", "exp": now + 300, "iat": now, "nonce": "n1", "email": "ana@example.com"})
    }

    const EXPECT: Expect<'static> = Expect {
        issuer: "https://idp",
        client_id: "lode",
        nonce: "n1",
        now: 1_800_000_000,
    };

    #[test]
    fn es256_tokens_verify_and_claims_are_checked() {
        let (kp, keys) = ec_key();
        let t = sign(
            &kp,
            &json!({"alg": "ES256", "kid": "k1"}),
            &claims(EXPECT.now),
        );
        let jwt = parse(&t).unwrap();
        assert!(jwt.has_key(&keys));
        jwt.verify(&keys).unwrap();
        check_claims(&jwt.claims, EXPECT).unwrap();

        // A changed payload no longer matches the signature.
        let mut parts: Vec<&str> = t.split('.').collect();
        let mut c = claims(EXPECT.now);
        c["email"] = json!("eve@example.com");
        let forged = enc(&c);
        parts[1] = &forged;
        assert_eq!(
            parse(&parts.join(".")).unwrap().verify(&keys),
            Err(TokenError::BadSignature)
        );
        // Another key id, another algorithm.
        let other = sign(
            &kp,
            &json!({"alg": "ES256", "kid": "k2"}),
            &claims(EXPECT.now),
        );
        assert_eq!(
            parse(&other).unwrap().verify(&keys),
            Err(TokenError::UnknownKey)
        );
        let none = format!(
            "{}.{}.",
            enc(&json!({"alg": "none"})),
            enc(&claims(EXPECT.now))
        );
        assert_eq!(
            parse(&none).unwrap().verify(&keys),
            Err(TokenError::Algorithm("none".into()))
        );
        assert_eq!(parse("a.b").unwrap_err(), TokenError::Malformed);
    }

    #[test]
    fn claims_are_refused_one_by_one() {
        let now = EXPECT.now;
        let check = |edit: &dyn Fn(&mut Value)| {
            let mut c = claims(now);
            edit(&mut c);
            check_claims(c.as_object().unwrap(), EXPECT)
        };
        assert_eq!(check(&|_| {}), Ok(()));
        assert_eq!(
            check(&|c| c["iss"] = json!("https://evil")),
            Err(TokenError::Issuer("https://evil".into()))
        );
        assert_eq!(
            check(&|c| c["aud"] = json!("other")),
            Err(TokenError::Audience)
        );
        assert_eq!(check(&|c| c["aud"] = json!(["other", "lode"])), Ok(()));
        assert_eq!(
            check(&|c| {
                c["aud"] = json!(["other", "lode"]);
                c["azp"] = json!("other");
            }),
            Err(TokenError::Audience)
        );
        assert_eq!(
            check(&|c| c["exp"] = json!(now - 600)),
            Err(TokenError::Expired)
        );
        assert_eq!(
            check(&|c| c["exp"] = json!(now - 60)),
            Ok(()),
            "within the skew"
        );
        assert_eq!(
            check(&|c| c["iat"] = json!(now + 600)),
            Err(TokenError::NotYetValid)
        );
        assert_eq!(check(&|c| c["nonce"] = json!("n0")), Err(TokenError::Nonce));
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            Err(TokenError::Nonce)
        );
        assert_eq!(
            check(&|c| {
                c.as_object_mut().unwrap().remove("exp");
            }),
            Err(TokenError::Malformed)
        );
    }
}
