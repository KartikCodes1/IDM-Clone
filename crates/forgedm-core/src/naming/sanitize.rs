//! Windows file name sanitiser (ARCHITECTURE 4.14, THREAT_MODEL FS-01).

use super::{FALLBACK_NAME, NAME_MAX_UTF16};

/// Longest extension (in UTF-16 units, dot included) kept when a name is cut to length.
pub(super) const EXTENSION_KEEP_UTF16: usize = 16;

/// Device names that Windows treats specially, compared case-insensitively on the stem.
const RESERVED_STEMS: [&str; 26] = [
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9",
];

/// Superscript digits that Windows maps onto COM/LPT ports (U+00B9, U+00B2, U+00B3).
const SUPERSCRIPT_DIGITS: [char; 3] = ['\u{b9}', '\u{b2}', '\u{b3}'];

/// Turns untrusted text into a safe Windows file name.
///
/// The result is never empty, `.` or `..`; has no path separators, `:` (so no
/// alternate data streams), forbidden or control characters, or format/bidi controls;
/// has no leading space or trailing dot/space; is not a reserved device name; and is at
/// most [`NAME_MAX_UTF16`] UTF-16 units. Sanitising is idempotent. Empty input yields
/// [`FALLBACK_NAME`].
#[must_use]
pub fn sanitize(raw: &str) -> String {
    sanitize_opt(raw).unwrap_or_else(|| FALLBACK_NAME.to_owned())
}

/// Like [`sanitize`], but `None` when nothing usable is left (empty, `.`, `..` or only
/// removed characters), so callers can try another name source.
#[must_use]
pub fn sanitize_opt(raw: &str) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !is_removed(*c)).collect();
    let trimmed = cleaned.trim_start_matches(' ').trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return None;
    }
    let capped = cap_length(trimmed);
    let named = if is_reserved(&capped) {
        cap_length(&format!("_{capped}"))
    } else {
        capped
    };
    (!named.is_empty()).then_some(named)
}

fn is_removed(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                | '\u{ad}'
                | '\u{61c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{2069}'
                | '\u{feff}'
        )
}

/// Whether the stem (before the first dot, trailing spaces trimmed) is a device name.
fn is_reserved(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    if RESERVED_STEMS.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let digit = chars.next();
    let is_port = prefix.eq_ignore_ascii_case("COM") || prefix.eq_ignore_ascii_case("LPT");
    is_port && digit.is_some_and(|d| SUPERSCRIPT_DIGITS.contains(&d)) && chars.next().is_none()
}

pub(super) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// Extensions that can follow `.tar` and stay glued to it as one compound extension.
const TAR_COMPRESSION_EXTENSIONS: [&str; 7] = [".gz", ".bz2", ".xz", ".zst", ".lz", ".lzma", ".z"];

/// Splits `name` into `(stem, extension)` at the last dot, where the extension keeps its
/// dot and `.tar.gz`-style pairs count as one extension. A dot at position 0 (hidden
/// file), no dot, or an extension longer than [`EXTENSION_KEEP_UTF16`] units means no
/// extension.
pub(super) fn split_extension(name: &str) -> (&str, &str) {
    let (stem, ext) = match name.rfind('.') {
        Some(at) if at > 0 && utf16_len(&name[at..]) <= EXTENSION_KEEP_UTF16 => name.split_at(at),
        _ => return (name, ""),
    };
    let is_compressed = TAR_COMPRESSION_EXTENSIONS
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e));
    let tar_at = stem.len().checked_sub(".tar".len()).filter(|at| *at > 0);
    match tar_at {
        Some(at)
            if is_compressed
                && stem
                    .get(at..)
                    .is_some_and(|t| t.eq_ignore_ascii_case(".tar")) =>
        {
            name.split_at(at)
        }
        _ => (stem, ext),
    }
}

/// Longest prefix of `text` that fits in `budget` UTF-16 units without splitting a
/// character (so never a surrogate pair).
pub(super) fn prefix_within(text: &str, budget: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        used += c.len_utf16();
        if used > budget {
            return &text[..at];
        }
    }
    text
}

/// Cuts `name` to [`NAME_MAX_UTF16`] units, truncating the stem and keeping a short
/// extension, then trims any trailing dots and spaces the cut exposed.
fn cap_length(name: &str) -> String {
    if utf16_len(name) <= NAME_MAX_UTF16 {
        return name.to_owned();
    }
    let (stem, ext) = split_extension(name);
    let budget = NAME_MAX_UTF16 - utf16_len(ext);
    let kept = prefix_within(stem, budget).trim_end_matches(['.', ' ']);
    format!("{kept}{ext}")
}
