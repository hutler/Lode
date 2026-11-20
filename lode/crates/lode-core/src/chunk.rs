//! Content-defined chunking of large files and the chunk list object (ARCHITECTURE §3.5).
//!
//! A file at or above [`CHUNK_THRESHOLD`] may be stored as a list of chunks. The file keeps
//! its id (`BLAKE3` of the whole content); each chunk is an ordinary blob under its own id,
//! and the list is stored under the file's id with kind [`crate::ObjectKind::ChunkList`].
//!
//! The cut points come from FastCDC (Xia et al., 2020) with normalized chunking at level 2
//! and a gear table derived from BLAKE3, so every client cuts the same content at the same
//! places and a small edit changes only the chunks around it.

use std::sync::OnceLock;

use crate::id::ObjectId;
use crate::varint;

/// Smallest chunk, except for the last chunk of a file.
pub const CHUNK_MIN: u32 = 256 * 1024;
/// Target average chunk size.
pub const CHUNK_AVG: u32 = 1024 * 1024;
/// Largest chunk.
pub const CHUNK_MAX: u32 = 4 * 1024 * 1024;
/// Files smaller than this are always stored whole.
pub const CHUNK_THRESHOLD: u64 = 16 * 1024 * 1024;
/// Name of the chunking scheme, reported by the server in `/info`.
pub const CHUNK_ALGORITHM: &str = "fastcdc-2020-lode-v1";

/// Normalization level: the mask before the average has this many bits more, the one after
/// it this many fewer, which pulls chunk sizes towards the average.
const NC_LEVEL: u32 = 2;

fn mask(bits: u32) -> u64 {
    // The gear hash shifts left, so the high bits depend on the most recent 64 bytes and
    // carry the most entropy; the masks select them.
    (!0u64) << (64 - bits)
}

fn masks() -> (u64, u64) {
    let avg_bits = CHUNK_AVG.trailing_zeros();
    (mask(avg_bits + NC_LEVEL), mask(avg_bits - NC_LEVEL))
}

/// The gear table: entry `i` is the first 8 bytes, little-endian, of
/// `BLAKE3("lode gear v1" || i)`.
pub fn gear_table() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u64; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut h = blake3::Hasher::new();
            h.update(b"lode gear v1");
            h.update(&[i as u8]);
            let out = h.finalize();
            let mut b = [0u8; 8];
            b.copy_from_slice(&out.as_bytes()[..8]);
            *slot = u64::from_le_bytes(b);
        }
        t
    })
}

/// Streaming cut-point finder. Feed bytes with [`Chunker::push`]; the boundaries it reports do
/// not depend on how the input is split into calls.
#[derive(Debug, Clone)]
pub struct Chunker {
    gear: &'static [u64; 256],
    mask_s: u64,
    mask_l: u64,
    /// Bytes in the current, unfinished chunk.
    pos: u32,
    hash: u64,
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunker {
    /// A chunker at the start of a file.
    pub fn new() -> Self {
        let (mask_s, mask_l) = masks();
        Self {
            gear: gear_table(),
            mask_s,
            mask_l,
            pos: 0,
            hash: 0,
        }
    }

    /// Bytes in the current chunk so far.
    pub fn pending(&self) -> u32 {
        self.pos
    }

    /// Scans `data` and returns the offset in `data` just after the first chunk end in it,
    /// or `None` if the current chunk continues past `data`. Call again with the rest of
    /// the slice after a boundary.
    pub fn next_boundary(&mut self, data: &[u8]) -> Option<usize> {
        for (i, &b) in data.iter().enumerate() {
            self.pos += 1;
            if self.pos > CHUNK_MIN {
                self.hash = (self.hash << 1).wrapping_add(self.gear[b as usize]);
                let m = if self.pos <= CHUNK_AVG {
                    self.mask_s
                } else {
                    self.mask_l
                };
                if self.hash & m == 0 || self.pos >= CHUNK_MAX {
                    self.pos = 0;
                    self.hash = 0;
                    return Some(i + 1);
                }
            }
        }
        None
    }

    /// Every chunk end in `data`, as offsets into `data`.
    pub fn push(&mut self, data: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut start = 0;
        while let Some(n) = self.next_boundary(&data[start..]) {
            start += n;
            out.push(start);
        }
        out
    }
}

/// One chunk of a chunked file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkRef {
    /// Blob id of the chunk's bytes.
    pub id: ObjectId,
    /// Chunk length in bytes.
    pub size: u32,
}

/// The payload of a chunk list object: a file's chunks, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkList {
    /// Chunks in file order.
    pub chunks: Vec<ChunkRef>,
}

/// Why bytes are not a valid chunk list.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChunkListError {
    /// Input ends early.
    #[error("chunk list is truncated")]
    Truncated,
    /// Unknown format version.
    #[error("unsupported chunk list version {0}")]
    Version(u8),
    /// A varint has a non-canonical form.
    #[error("chunk list has a non-canonical number")]
    NonCanonical,
    /// Fewer than two chunks.
    #[error("a chunk list needs at least two chunks")]
    TooFew,
    /// A chunk is smaller than the minimum (only the last may be) or larger than the maximum.
    #[error("chunk {index} has size {size}, outside the allowed range")]
    ChunkSize {
        /// Position of the chunk.
        index: usize,
        /// Its size.
        size: u64,
    },
    /// Bytes after the last chunk.
    #[error("chunk list has trailing bytes")]
    Trailing,
}

impl ChunkList {
    /// Format version written by this build.
    pub const VERSION: u8 = 1;

    /// Total file size.
    pub fn total_size(&self) -> u64 {
        self.chunks.iter().map(|c| u64::from(c.size)).sum()
    }

    /// Canonical encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(2 + self.chunks.len() * 36);
        out.push(Self::VERSION);
        varint::encode(self.chunks.len() as u64, &mut out);
        for c in &self.chunks {
            out.extend_from_slice(c.id.as_bytes());
            varint::encode(u64::from(c.size), &mut out);
        }
        out
    }

    /// Strict decoding: rejects anything [`ChunkList::encode`] would not produce or that
    /// breaks the size rules.
    pub fn decode(bytes: &[u8]) -> Result<Self, ChunkListError> {
        let (&version, mut rest) = bytes.split_first().ok_or(ChunkListError::Truncated)?;
        if version != Self::VERSION {
            return Err(ChunkListError::Version(version));
        }
        let (count, n) = read_varint(rest)?;
        rest = &rest[n..];
        if count < 2 {
            return Err(ChunkListError::TooFew);
        }
        // Each entry takes at least 33 bytes; refuse counts the input cannot hold before
        // allocating for them.
        if count > (rest.len() / 33) as u64 {
            return Err(ChunkListError::Truncated);
        }
        let mut chunks = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            if rest.len() < 32 {
                return Err(ChunkListError::Truncated);
            }
            let mut id = [0u8; 32];
            id.copy_from_slice(&rest[..32]);
            rest = &rest[32..];
            let (size, n) = read_varint(rest)?;
            rest = &rest[n..];
            let last = index + 1 == count as usize;
            let min = if last { 1 } else { u64::from(CHUNK_MIN) };
            if size < min || size > u64::from(CHUNK_MAX) {
                return Err(ChunkListError::ChunkSize { index, size });
            }
            chunks.push(ChunkRef {
                id: ObjectId::from_bytes(id),
                size: size as u32,
            });
        }
        if !rest.is_empty() {
            return Err(ChunkListError::Trailing);
        }
        Ok(Self { chunks })
    }
}

fn read_varint(input: &[u8]) -> Result<(u64, usize), ChunkListError> {
    varint::decode(input).map_err(|e| match e {
        varint::VarintError::Truncated => ChunkListError::Truncated,
        varint::VarintError::NonCanonical => ChunkListError::NonCanonical,
    })
}

/// Hashes a file and cuts it into chunks in one pass: the file id, its size and, for a file
/// at or above [`CHUNK_THRESHOLD`], its chunk list.
#[derive(Debug, Clone)]
pub struct ChunkingHasher {
    whole: blake3::Hasher,
    chunk: blake3::Hasher,
    chunker: Chunker,
    chunks: Vec<ChunkRef>,
    total: u64,
}

impl Default for ChunkingHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// What [`ChunkingHasher::finish`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunked {
    /// Blob id of the whole file.
    pub id: ObjectId,
    /// File size.
    pub size: u64,
    /// The chunks, when the file is large enough to be chunked.
    pub chunks: Option<ChunkList>,
}

impl ChunkingHasher {
    /// Starts a file.
    pub fn new() -> Self {
        Self {
            whole: blake3::Hasher::new(),
            chunk: blake3::Hasher::new(),
            chunker: Chunker::new(),
            chunks: Vec::new(),
            total: 0,
        }
    }

    /// Feeds the next bytes of the file.
    pub fn update(&mut self, mut data: &[u8]) {
        self.whole.update(data);
        self.total += data.len() as u64;
        while let Some(n) = self.chunker.next_boundary(data) {
            self.chunk.update(&data[..n]);
            self.close_chunk();
            data = &data[n..];
        }
        self.chunk.update(data);
    }

    fn close_chunk(&mut self) {
        let id = ObjectId::from_bytes(*self.chunk.finalize().as_bytes());
        // The chunker never lets a chunk exceed CHUNK_MAX, so it fits in u32.
        let size = self.chunk.count() as u32;
        self.chunks.push(ChunkRef { id, size });
        self.chunk.reset();
    }

    /// Ends the file.
    pub fn finish(mut self) -> Chunked {
        if self.chunker.pending() > 0 {
            self.close_chunk();
        }
        let id = ObjectId::from_bytes(*self.whole.finalize().as_bytes());
        let chunks =
            (self.total >= CHUNK_THRESHOLD && self.chunks.len() >= 2).then_some(ChunkList {
                chunks: self.chunks,
            });
        Chunked {
            id,
            size: self.total,
            chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Deterministic pseudo-random bytes (xorshift), so tests need no RNG crate.
    pub(crate) fn noise(seed: u64, len: usize) -> Vec<u8> {
        let mut x = seed | 1;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    fn sizes(data: &[u8]) -> Vec<usize> {
        let mut c = Chunker::new();
        let mut ends = c.push(data);
        if c.pending() > 0 {
            ends.push(data.len());
        }
        let mut prev = 0;
        ends.iter()
            .map(|&e| {
                let s = e - prev;
                prev = e;
                s
            })
            .collect()
    }

    #[test]
    fn chunks_respect_bounds_and_average() {
        let data = noise(7, 48 * 1024 * 1024);
        let s = sizes(&data);
        for (i, &n) in s.iter().enumerate() {
            assert!(n <= CHUNK_MAX as usize);
            if i + 1 < s.len() {
                assert!(n >= CHUNK_MIN as usize, "chunk {i} is {n}");
            }
        }
        let avg = data.len() / s.len();
        assert!(
            (CHUNK_AVG as usize / 2..CHUNK_AVG as usize * 2).contains(&avg),
            "average {avg}"
        );
    }

    #[test]
    fn zeros_cut_at_max() {
        let data = vec![0u8; 10 * 1024 * 1024];
        let s = sizes(&data);
        assert_eq!(s[0], CHUNK_MAX as usize);
    }

    /// The fixed vector of ARCHITECTURE §3.5: other implementations must cut this input at
    /// the same places.
    #[test]
    fn fixed_vector() {
        let data = noise(0x10de, 24 * 1024 * 1024);
        let h = {
            let mut h = ChunkingHasher::new();
            h.update(&data);
            h.finish()
        };
        let list = h.chunks.expect("chunked");
        let ends: Vec<u64> = list
            .chunks
            .iter()
            .scan(0u64, |acc, c| {
                *acc += u64::from(c.size);
                Some(*acc)
            })
            .collect();
        assert_eq!(ends.last().copied(), Some(data.len() as u64));
        assert_eq!(h.id, crate::blob_id(&data[..]).unwrap());
        // Recorded with this implementation; see ARCHITECTURE §3.5.
        let first: Vec<u64> = ends.iter().take(4).copied().collect();
        assert_eq!(first, FIXED_VECTOR_FIRST_ENDS, "all ends: {ends:?}");
        assert_eq!(list.chunks.len(), FIXED_VECTOR_COUNT);
    }

    const FIXED_VECTOR_FIRST_ENDS: [u64; 4] = [1_394_431, 2_789_073, 3_920_034, 5_066_432];
    const FIXED_VECTOR_COUNT: usize = 20;

    #[test]
    fn local_edit_changes_few_chunks() {
        let a = noise(3, 40 * 1024 * 1024);
        let mut b = a.clone();
        let mid = b.len() / 2;
        b.splice(mid..mid, noise(9, 100_000));
        b.truncate(b.len() - 10);
        let ids = |d: &[u8]| {
            let mut h = ChunkingHasher::new();
            h.update(d);
            h.finish().chunks.unwrap().chunks
        };
        let old: std::collections::HashSet<_> = ids(&a).into_iter().map(|c| c.id).collect();
        let new = ids(&b);
        let fresh = new.iter().filter(|c| !old.contains(&c.id)).count();
        assert!(fresh <= 3, "{fresh} new chunks");
    }

    #[test]
    fn small_files_are_not_chunked() {
        let mut h = ChunkingHasher::new();
        h.update(&noise(1, 1024 * 1024));
        let r = h.finish();
        assert!(r.chunks.is_none());
        assert_eq!(r.size, 1024 * 1024);
    }

    #[test]
    fn list_round_trip_and_rejects() {
        let mut h = ChunkingHasher::new();
        h.update(&noise(5, 20 * 1024 * 1024));
        let list = h.finish().chunks.unwrap();
        let enc = list.encode();
        assert_eq!(ChunkList::decode(&enc), Ok(list.clone()));
        assert_eq!(
            ChunkList::decode(&enc[..enc.len() - 1]),
            Err(ChunkListError::Truncated)
        );
        let mut trailing = enc.clone();
        trailing.push(0);
        assert_eq!(ChunkList::decode(&trailing), Err(ChunkListError::Trailing));
        let one = ChunkList {
            chunks: vec![list.chunks[0]],
        };
        assert_eq!(
            ChunkList::decode(&one.encode()),
            Err(ChunkListError::TooFew)
        );
        let mut small = list.clone();
        small.chunks[0].size = 10;
        assert!(matches!(
            ChunkList::decode(&small.encode()),
            Err(ChunkListError::ChunkSize { index: 0, .. })
        ));
        assert_eq!(ChunkList::decode(&[2, 0]), Err(ChunkListError::Version(2)));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]
        #[test]
        fn boundaries_do_not_depend_on_split(seed in any::<u64>(), cuts in prop::collection::vec(0usize..6_000_000, 0..8)) {
            let data = noise(seed, 6 * 1024 * 1024);
            let whole = Chunker::new().push(&data);
            let mut c = Chunker::new();
            let mut got = Vec::new();
            let mut cuts = cuts;
            cuts.push(0);
            cuts.push(data.len());
            cuts.sort_unstable();
            for w in cuts.windows(2) {
                let (a, b) = (w[0].min(data.len()), w[1].min(data.len()));
                got.extend(c.push(&data[a..b]).into_iter().map(|e| e + a));
            }
            prop_assert_eq!(whole, got);
        }

        #[test]
        fn decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
            let _ = ChunkList::decode(&bytes);
        }
    }
}
