//! Header of a stored object file (ARCHITECTURE §3.3).

/// What an object holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectKind {
    /// File content or a symlink target.
    Blob = 1,
    /// An encoded [`crate::Tree`].
    Tree = 2,
    /// Reserved for chunked storage; not implemented.
    ChunkList = 3,
}

/// How the payload after the header is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// Raw bytes.
    None = 0,
    /// A single zstd frame.
    Zstd = 1,
}

/// The 16-byte header at the start of every stored object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    /// Object kind.
    pub kind: ObjectKind,
    /// Payload codec.
    pub codec: Codec,
    /// Length of the content once decoded. Readers must not produce more than this.
    pub raw_len: u64,
}

/// Why 16 bytes are not a valid header.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HeaderError {
    /// Fewer than 16 bytes.
    #[error("object header is truncated")]
    Truncated,
    /// The first four bytes are not `LODE`.
    #[error("object header has a bad magic number")]
    BadMagic,
    /// A format version this build does not know.
    #[error("unsupported object format version {0}")]
    Version(u8),
    /// Unknown kind byte.
    #[error("unknown object kind {0}")]
    Kind(u8),
    /// Unknown codec byte.
    #[error("unknown object codec {0}")]
    Codec(u8),
    /// The reserved byte is not zero.
    #[error("object header reserved byte is not zero")]
    Reserved,
}

impl ObjectHeader {
    /// Encoded length.
    pub const LEN: usize = 16;
    /// Magic bytes.
    pub const MAGIC: [u8; 4] = *b"LODE";
    /// Format version written by this build.
    pub const VERSION: u8 = 1;

    /// Encodes the header.
    pub fn encode(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..4].copy_from_slice(&Self::MAGIC);
        out[4] = Self::VERSION;
        out[5] = self.kind as u8;
        out[6] = self.codec as u8;
        out[7] = 0;
        out[8..].copy_from_slice(&self.raw_len.to_le_bytes());
        out
    }

    /// Decodes the first 16 bytes of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderError> {
        let b: &[u8; 16] = bytes
            .get(..16)
            .and_then(|s| s.try_into().ok())
            .ok_or(HeaderError::Truncated)?;
        if b[..4] != Self::MAGIC {
            return Err(HeaderError::BadMagic);
        }
        if b[4] != Self::VERSION {
            return Err(HeaderError::Version(b[4]));
        }
        let kind = match b[5] {
            1 => ObjectKind::Blob,
            2 => ObjectKind::Tree,
            3 => ObjectKind::ChunkList,
            k => return Err(HeaderError::Kind(k)),
        };
        let codec = match b[6] {
            0 => Codec::None,
            1 => Codec::Zstd,
            c => return Err(HeaderError::Codec(c)),
        };
        if b[7] != 0 {
            return Err(HeaderError::Reserved);
        }
        let mut len = [0u8; 8];
        len.copy_from_slice(&b[8..16]);
        Ok(Self {
            kind,
            codec,
            raw_len: u64::from_le_bytes(len),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_layout() {
        let h = ObjectHeader {
            kind: ObjectKind::Tree,
            codec: Codec::Zstd,
            raw_len: 0x0102_0304_0506_0708,
        };
        let b = h.encode();
        assert_eq!(&b, b"LODE\x01\x02\x01\x00\x08\x07\x06\x05\x04\x03\x02\x01");
        assert_eq!(ObjectHeader::decode(&b), Ok(h));
    }

    #[test]
    fn rejects_bad_headers() {
        let good = ObjectHeader {
            kind: ObjectKind::Blob,
            codec: Codec::None,
            raw_len: 5,
        }
        .encode();
        assert_eq!(
            ObjectHeader::decode(&good[..15]),
            Err(HeaderError::Truncated)
        );
        let mut b = good;
        b[0] = b'X';
        assert_eq!(ObjectHeader::decode(&b), Err(HeaderError::BadMagic));
        let mut b = good;
        b[4] = 2;
        assert_eq!(ObjectHeader::decode(&b), Err(HeaderError::Version(2)));
        let mut b = good;
        b[5] = 9;
        assert_eq!(ObjectHeader::decode(&b), Err(HeaderError::Kind(9)));
        let mut b = good;
        b[6] = 7;
        assert_eq!(ObjectHeader::decode(&b), Err(HeaderError::Codec(7)));
        let mut b = good;
        b[7] = 1;
        assert_eq!(ObjectHeader::decode(&b), Err(HeaderError::Reserved));
    }
}
