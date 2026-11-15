//! File name derivation and sanitising (ARCHITECTURE 4.14, THREAT_MODEL FS-01).
//!
//! Names come from untrusted sources (Content-Disposition, the URL path), so every one
//! passes through [`sanitize`] before it can reach the file system. The recorded name
//! always equals the on-disk name.

use url::Url;
use uuid::Uuid;

pub mod content_disposition;
mod percent;
pub mod sanitize;

pub use sanitize::{sanitize, sanitize_opt};

use sanitize::{prefix_within, split_extension, utf16_len};

/// Longest file name in UTF-16 units, so `<name>.<id8>.forgedm.part` fits in 255 (A.9).
pub const NAME_MAX_UTF16: usize = 233;
/// Length of the `.<id8>.forgedm.part` suffix in UTF-16 units (A.9).
pub const PART_SUFFIX_UTF16: usize = 22;
/// Highest collision counter: `name (999).ext` (A.9).
pub const COLLISION_MAX: u32 = 999;
/// Content-Disposition values above this many bytes are ignored (A.9).
pub const MAX_CONTENT_DISPOSITION_BYTES: usize = 4096;
/// The name used when a candidate sanitises to nothing.
pub const FALLBACK_NAME: &str = "download";

/// A name proposed for a download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedName {
    /// The name shown in lists. Phase 1 shows the on-disk name as is.
    pub display_name: String,
    /// The sanitised name to create on disk (at most [`NAME_MAX_UTF16`] units).
    pub file_name: String,
}

/// Proposes a name from, in order: the Content-Disposition header, the last segment of
/// the final URL path (percent-decoded), then `download-<id8>`.
///
/// A source that sanitises to nothing is skipped. The result is always safe to create.
#[must_use]
pub fn suggest(content_disposition: Option<&str>, final_url: &Url, id: Uuid) -> SuggestedName {
    let from_header = content_disposition
        .and_then(content_disposition::parse_filename)
        .and_then(|raw| sanitize_opt(&raw));
    let name = from_header
        .or_else(|| name_from_url_path(final_url))
        .unwrap_or_else(|| fallback_name(id));
    SuggestedName {
        display_name: name.clone(),
        file_name: name,
    }
}

fn name_from_url_path(url: &Url) -> Option<String> {
    let segment = url.path().rsplit('/').next()?;
    sanitize_opt(&percent::decode_lossy(segment))
}

fn fallback_name(id: Uuid) -> String {
    let id8: String = id.simple().to_string().chars().take(8).collect();
    format!("{FALLBACK_NAME}-{id8}")
}

/// The `n`-th alternative for a name that already exists: `report.pdf` becomes
/// `report (2).pdf`, and `a.tar.gz` becomes `a (1).tar.gz`. `n == 0` returns the name
/// unchanged. The stem is shortened when needed so the result stays within
/// [`NAME_MAX_UTF16`]. Intended for names that already passed [`sanitize`].
#[must_use]
pub fn collision_candidate(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, ext) = split_extension(name);
    let suffix = format!(" ({n})");
    let used = utf16_len(ext) + utf16_len(&suffix);
    let kept = if utf16_len(stem) + used > NAME_MAX_UTF16 {
        prefix_within(stem, NAME_MAX_UTF16.saturating_sub(used)).trim_end_matches(['.', ' '])
    } else {
        stem
    };
    if kept.is_empty() {
        return format!("{}{ext}", suffix.trim_start());
    }
    format!("{kept}{suffix}{ext}")
}
