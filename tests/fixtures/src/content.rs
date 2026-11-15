//! Deterministic content: a splitmix64 counter generator.
//!
//! The stream is a sequence of little-endian 64-bit words. Word `i` is the splitmix64 output
//! for state `seed + (i + 1) * GAMMA`, so any offset is computed in O(1), the stream never
//! repeats within 2^64 words, and offsets beyond 4 GiB need no storage.

use sha2::{Digest, Sha256};

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
const WORD: usize = 8;
const HASH_CHUNK: usize = 64 * 1024;

fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The 64-bit word at word index `index` for `seed`.
#[must_use]
pub fn word(seed: u64, index: u64) -> u64 {
    mix(seed.wrapping_add(index.wrapping_add(1).wrapping_mul(GAMMA)))
}

fn widen(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// Fills `buf` with the bytes of the stream for `seed` starting at byte `offset`.
///
/// Offsets are taken modulo 2^64 (word indices wrap), which is far beyond any real size.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    let mut index = offset / 8;
    let skip = usize::try_from(offset % 8).unwrap_or(0);
    let mut rest: &mut [u8] = buf;
    if skip > 0 && !rest.is_empty() {
        let bytes = word(seed, index).to_le_bytes();
        let n = (WORD - skip).min(rest.len());
        let (head, tail) = rest.split_at_mut(n);
        head.copy_from_slice(&bytes[skip..skip + n]);
        rest = tail;
        index = index.wrapping_add(1);
    }
    let (words, tail) = rest.as_chunks_mut::<WORD>();
    for chunk in words {
        *chunk = word(seed, index).to_le_bytes();
        index = index.wrapping_add(1);
    }
    if !tail.is_empty() {
        let bytes = word(seed, index).to_le_bytes();
        tail.copy_from_slice(&bytes[..tail.len()]);
    }
}

/// SHA-256 of the first `size` bytes of the stream for `seed`, computed by streaming.
#[must_use]
pub fn sha256(seed: u64, size: u64) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; HASH_CHUNK];
    let mut pos = 0_u64;
    while pos < size {
        let n = usize::try_from(size - pos).map_or(HASH_CHUNK, |left| left.min(HASH_CHUNK));
        fill(seed, pos, &mut buf[..n]);
        hasher.update(&buf[..n]);
        pos += widen(n);
    }
    hasher.finalize().into()
}
