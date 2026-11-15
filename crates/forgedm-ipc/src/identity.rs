//! Peer identity decisions (ARCHITECTURE 3.4 and 3.5).
//!
//! [`decide_identity`] is pure: it compares an already-resolved [`PeerIdentity`] with what
//! is expected, so the decision is unit-testable with an injected identity. [`verify_pid`]
//! resolves a process id through the platform layer first. Every failure rejects (fail
//! closed).

use std::path::Path;

use forgedm_platform::peer::{PeerError, PeerIdentity, peer_identity};
use forgedm_platform::sid::Sid;

/// Accepts `actual` only if it runs as `expected_sid` and, when `expected_image` is given,
/// its image path equals it (canonicalised, case-insensitive, verbatim prefix ignored).
///
/// # Errors
/// [`PeerError::SidMismatch`] or [`PeerError::ImageMismatch`].
pub fn decide_identity(
    actual: &PeerIdentity,
    expected_sid: &Sid,
    expected_image: Option<&Path>,
) -> Result<(), PeerError> {
    if actual.sid != *expected_sid {
        return Err(PeerError::SidMismatch);
    }
    if let Some(expected) = expected_image
        && comparable(&actual.image_path) != comparable(expected)
    {
        return Err(PeerError::ImageMismatch {
            actual: actual.image_path.clone(),
            expected: expected.to_path_buf(),
        });
    }
    Ok(())
}

/// Resolves `pid` and applies [`decide_identity`].
///
/// # Errors
/// [`PeerError::Platform`] when the process cannot be inspected (access denied, exited,
/// bad pid), otherwise as [`decide_identity`].
pub fn verify_pid(
    pid: u32,
    expected_sid: &Sid,
    expected_image: Option<&Path>,
) -> Result<PeerIdentity, PeerError> {
    let identity = peer_identity(pid)?;
    decide_identity(&identity, expected_sid, expected_image)?;
    Ok(identity)
}

/// Canonicalises (when the file exists), lowercases and strips the verbatim prefix.
fn comparable(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let lower = canonical.to_string_lossy().to_lowercase();
    if let Some(unc) = lower.strip_prefix(r"\\?\unc\") {
        return format!(r"\\{unc}");
    }
    match lower.strip_prefix(r"\\?\") {
        Some(rest) => rest.to_owned(),
        None => lower,
    }
}
