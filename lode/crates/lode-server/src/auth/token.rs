//! Bearer token format and hashing.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Every token starts with this.
pub const TOKEN_PREFIX: &str = "lode_";

/// A new token: `lode_` followed by base64url of 32 random bytes.
pub fn generate_token(rng: &mut impl RngCore) -> String {
    let mut bytes = [0u8; 32];
    rng.fill_bytes(&mut bytes);
    format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// SHA-256 of the token text; the only form stored.
pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Compares without an early exit, so timing does not reveal how much matched.
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_hash() {
        let t = generate_token(&mut rand::rngs::OsRng);
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 43);
        assert!(t[TOKEN_PREFIX.len()..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(t, generate_token(&mut rand::rngs::OsRng));
        assert_eq!(hash_token(&t), hash_token(&t));
        assert!(ct_eq(&hash_token(&t), &hash_token(&t)));
        assert!(!ct_eq(&hash_token(&t), &hash_token("x")));
        assert!(!ct_eq(b"ab", b"abc"));
    }
}
