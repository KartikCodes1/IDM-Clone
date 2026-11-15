//! Generator and verifier tests (S0.5).

use forgedm_fixture::content;
use forgedm_fixture::verify::{self, VerifyError};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// Independent reference: byte `i` of the stream, computed straight from the splitmix64 definition.
fn reference_byte(seed: u64, i: u64) -> u8 {
    let mut z = seed.wrapping_add((i / 8 + 1).wrapping_mul(GAMMA));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    z.to_le_bytes()[usize::try_from(i % 8).expect("small")]
}

fn temp_file(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "forgedm-fixture-{}-{}-{tag}.bin",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn generator_offset_addressable_incl_beyond_4gib() {
    let seed = 0xDEAD_BEEF_u64;
    for offset in [
        0_u64,
        1,
        7,
        8,
        4_294_967_295,
        4_294_967_296,
        5 * 1024 * 1024 * 1024 + 3,
        u64::MAX - 100,
    ] {
        let mut buf = vec![0u8; 37];
        content::fill(seed, offset, &mut buf);
        for (i, b) in buf.iter().enumerate() {
            let pos = offset + u64::try_from(i).expect("small");
            assert_eq!(*b, reference_byte(seed, pos), "offset {offset} + {i}");
        }
    }
    // Reading one region in one piece or in unaligned pieces gives identical bytes.
    let base = 6 * 1024 * 1024 * 1024 + 5;
    let mut whole = vec![0u8; 1000];
    content::fill(seed, base, &mut whole);
    let mut parts = vec![0u8; 1000];
    let (a, rest) = parts.split_at_mut(3);
    let (b, c) = rest.split_at_mut(500);
    content::fill(seed, base, a);
    content::fill(seed, base + 3, b);
    content::fill(seed, base + 503, c);
    assert_eq!(whole, parts);
    // Different seeds differ.
    let mut other = vec![0u8; 1000];
    content::fill(seed + 1, base, &mut other);
    assert_ne!(whole, other);
}

#[test]
fn generator_detects_one_byte_shift() {
    let mut data = vec![0u8; 4096];
    content::fill(42, 0, &mut data);
    let mut expected = vec![0u8; 4095];
    content::fill(42, 0, &mut expected);
    let shifted = &data[1..];
    assert_ne!(shifted, expected.as_slice());
    let equal = shifted
        .iter()
        .zip(&expected)
        .filter(|(a, b)| a == b)
        .count();
    assert!(
        equal < 64,
        "a one-byte shift must not look like the original ({equal} equal)"
    );
    assert_eq!(content::sha256(42, 4096), content::sha256(42, 4096));
    assert_ne!(content::sha256(42, 4096), content::sha256(43, 4096));
}

#[test]
fn verify_reports_first_mismatch_offset() {
    let size = 100_000_u64;
    let mut data = vec![0u8; usize::try_from(size).expect("small")];
    content::fill(7, 0, &mut data);
    let path = temp_file("verify");

    std::fs::write(&path, &data).expect("write");
    assert!(verify::file_matches(&path, 7, size).is_ok());

    let mut bad = data.clone();
    bad[70_000] ^= 0xFF;
    bad[54_321] ^= 0x01;
    std::fs::write(&path, &bad).expect("write");
    assert!(matches!(
        verify::file_matches(&path, 7, size),
        Err(VerifyError::Mismatch { offset: 54_321 })
    ));

    std::fs::write(&path, &data[..60_000]).expect("write");
    assert!(matches!(
        verify::file_matches(&path, 7, size),
        Err(VerifyError::Mismatch { offset: 60_000 })
    ));

    let mut longer = data.clone();
    longer.push(0);
    std::fs::write(&path, &longer).expect("write");
    assert!(matches!(
        verify::file_matches(&path, 7, size),
        Err(VerifyError::Mismatch { offset: 100_000 })
    ));

    std::fs::remove_file(&path).expect("cleanup");
    assert!(matches!(
        verify::file_matches(&path, 7, size),
        Err(VerifyError::Io(_))
    ));
}
