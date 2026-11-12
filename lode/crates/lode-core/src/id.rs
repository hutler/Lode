//! Object identifiers and the two hash functions that produce them (ARCHITECTURE §3.1).

use std::fmt;
use std::io::{self, Read};
use std::str::FromStr;

/// Context string for tree ids. Separates the tree domain from the blob domain, so a tree can
/// never have the same id as a blob with identical bytes.
pub const TREE_ID_CONTEXT: &str = "lode 2026 tree v1";

/// A 32-byte content address, shown as 64 lower-case hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    /// Length of an id in bytes.
    pub const LEN: usize = 32;

    /// Wraps raw bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lower-case hex form.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parses exactly 64 hex characters. Upper-case input is accepted; output is always
    /// lower-case.
    pub fn from_hex(s: &str) -> Result<Self, IdParseError> {
        if s.len() != 64 {
            return Err(IdParseError::Length(s.len()));
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|_| IdParseError::NotHex)?;
        Ok(Self(out))
    }
}

/// Why a string is not an object id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdParseError {
    /// The string is not 64 characters long.
    #[error("object id must be 64 hex characters, got {0}")]
    Length(usize),
    /// The string contains a character that is not a hex digit.
    #[error("object id contains a non-hex character")]
    NotHex,
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.to_hex())
    }
}

impl FromStr for ObjectId {
    type Err = IdParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_hex(s)
    }
}

impl serde::Serialize for ObjectId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for ObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        Self::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

/// Blob id: plain BLAKE3 of the content, so any stock BLAKE3 tool gives the same answer.
/// Streams the reader with a fixed buffer.
pub fn blob_id<R: Read>(mut reader: R) -> io::Result<ObjectId> {
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
    }
    Ok(ObjectId(*hasher.finalize().as_bytes()))
}

/// Tree id: BLAKE3 in derive-key mode with [`TREE_ID_CONTEXT`] over the tree encoding.
pub fn tree_id(encoded: &[u8]) -> ObjectId {
    let mut hasher = blake3::Hasher::new_derive_key(TREE_ID_CONTEXT);
    hasher.update(encoded);
    ObjectId(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Published BLAKE3 test vectors (BLAKE3 repository, test_vectors.json): the input of
    // length n is the byte sequence 0, 1, …, 250, 0, 1, … truncated to n.
    fn vector_input(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn empty_blob_matches_published_vector() {
        let id = blob_id(&[][..]).unwrap();
        assert_eq!(
            id.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn one_byte_blob_matches_published_vector() {
        let id = blob_id(&vector_input(1)[..]).unwrap();
        assert_eq!(
            id.to_hex(),
            "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213"
        );
    }

    #[test]
    fn streaming_matches_one_shot_across_buffer_boundaries() {
        let data = vector_input(200_000);
        let streamed = blob_id(&data[..]).unwrap();
        assert_eq!(streamed.as_bytes(), blake3::hash(&data).as_bytes());
    }

    #[test]
    fn tree_id_differs_from_blob_id_for_same_bytes() {
        let bytes = b"\x01\x00";
        assert_ne!(tree_id(bytes), blob_id(&bytes[..]).unwrap());
    }

    #[test]
    fn hex_round_trip_and_errors() {
        let id = blob_id(&b"abc"[..]).unwrap();
        assert_eq!(ObjectId::from_hex(&id.to_hex()).unwrap(), id);
        assert_eq!(ObjectId::from_hex(&id.to_hex().to_uppercase()).unwrap(), id);
        assert_eq!(ObjectId::from_hex("ab"), Err(IdParseError::Length(2)));
        assert_eq!(
            ObjectId::from_hex(&"g".repeat(64)),
            Err(IdParseError::NotHex)
        );
    }

    #[test]
    fn serde_uses_hex_string() {
        let id = ObjectId::from_bytes([0xab; 32]);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", "ab".repeat(32)));
        let back: ObjectId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_str::<ObjectId>("\"zz\"").is_err());
    }
}
