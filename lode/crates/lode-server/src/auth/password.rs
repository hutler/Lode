//! Argon2id password hashing.

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Shortest password accepted.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Hashes a password with Argon2id and a random salt (PHC string format).
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

/// Checks a password against a stored hash. A missing or malformed hash never matches, but
/// still costs one hash computation so the answer takes about as long either way.
pub fn verify_password(password: &str, stored: Option<&str>) -> bool {
    let parsed = stored.and_then(|s| PasswordHash::new(s).ok());
    match parsed {
        Some(h) => Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok(),
        None => {
            let _ = hash_password(password);
            false
        }
    }
}

/// Letters and digits that cannot be mistaken for one another when read aloud or copied by
/// hand (no 0/O, 1/l/I).
const GENERATED_ALPHABET: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// Length of a generated password: about 116 bits.
pub const GENERATED_PASSWORD_LEN: usize = 20;

/// A random password for a first sign-in, from an unambiguous alphabet.
pub fn generate_password(rng: &mut impl rand::RngCore) -> String {
    use rand::Rng;
    (0..GENERATED_PASSWORD_LEN)
        .map(|_| char::from(GENERATED_ALPHABET[rng.gen_range(0..GENERATED_ALPHABET.len())]))
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn hash_and_verify() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_password("correct horse", Some(&h)));
        assert!(!verify_password("wrong horse", Some(&h)));
        assert!(!verify_password("correct horse", None));
        assert!(!verify_password("correct horse", Some("not a hash")));
    }

    #[test]
    fn generated_passwords_use_the_unambiguous_alphabet() {
        let p = generate_password(&mut rand::rngs::OsRng);
        assert_eq!(p.len(), GENERATED_PASSWORD_LEN);
        assert!(p.bytes().all(|b| GENERATED_ALPHABET.contains(&b)));
    }
}
