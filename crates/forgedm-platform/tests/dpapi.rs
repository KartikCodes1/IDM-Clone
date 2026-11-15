//! DPAPI protect / unprotect.

use forgedm_platform::PlatformError;
use forgedm_platform::dpapi::{protect, unprotect};

const ENTROPY: &[u8] = b"ForgeDM/v1/original_url_sealed/0190-test";
const CANARY: &[u8] = b"https://user:CANARY-s3cret@example.com/path?token=CANARY-abc";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn dpapi_roundtrip_and_ciphertext_hides_plaintext() {
    let blob = protect(CANARY, ENTROPY).expect("protect");
    assert!(!blob.is_empty());
    assert!(!contains(&blob, CANARY));
    assert!(!contains(&blob, b"CANARY"));
    let wide: Vec<u8> = "CANARY".encode_utf16().flat_map(u16::to_le_bytes).collect();
    assert!(!contains(&blob, &wide));
    let plain = unprotect(&blob, ENTROPY).expect("unprotect");
    assert_eq!(plain.as_slice(), CANARY);
}

#[test]
fn dpapi_roundtrip_large_and_binary_values() {
    let data: Vec<u8> = (0..70_000u32)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    let blob = protect(&data, ENTROPY).expect("protect");
    assert_eq!(
        unprotect(&blob, ENTROPY).expect("unprotect").as_slice(),
        data.as_slice()
    );
}

#[test]
fn dpapi_tampered_blob_fails() {
    let blob = protect(CANARY, ENTROPY).expect("protect");
    let mut flipped = blob.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x01;
    assert!(unprotect(&flipped, ENTROPY).is_err());
    let truncated = &blob[..blob.len() / 2];
    assert!(unprotect(truncated, ENTROPY).is_err());
    assert!(unprotect(&[], ENTROPY).is_err());
    assert!(unprotect(b"not a dpapi blob at all", ENTROPY).is_err());
}

#[test]
fn dpapi_wrong_entropy_fails() {
    let blob = protect(CANARY, ENTROPY).expect("protect");
    let r = unprotect(&blob, b"ForgeDM/v1/final_url_sealed/0190-test");
    assert!(matches!(r, Err(PlatformError::Win32 { .. })), "got {r:?}");
}

#[test]
fn dpapi_rejects_empty_entropy() {
    assert!(matches!(
        protect(CANARY, b""),
        Err(PlatformError::InvalidInput(_))
    ));
    let blob = protect(CANARY, ENTROPY).expect("protect");
    assert!(matches!(
        unprotect(&blob, b""),
        Err(PlatformError::InvalidInput(_))
    ));
}

#[test]
fn secret_bytes_debug_is_redacted() {
    let blob = protect(CANARY, ENTROPY).expect("protect");
    let plain = unprotect(&blob, ENTROPY).expect("unprotect");
    let shown = format!("{plain:?}");
    assert!(!shown.contains("CANARY"));
    assert!(shown.contains("redacted"));
}
