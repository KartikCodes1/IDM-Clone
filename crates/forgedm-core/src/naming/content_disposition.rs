//! Content-Disposition parsing (RFC 6266 with RFC 5987 extended values).
//!
//! Only the file name is extracted, and it is returned raw: callers must pass it through
//! [`super::sanitize`]. Header text above [`MAX_CONTENT_DISPOSITION_BYTES`] is ignored.

use super::MAX_CONTENT_DISPOSITION_BYTES;
use super::percent::decode_strict;

/// Extracts the suggested file name from a Content-Disposition value.
///
/// `filename*` (RFC 5987, UTF-8 or ISO-8859-1) wins over `filename`; a `filename*` that
/// does not decode falls back to `filename`. Returns `None` when the header is too long
/// or carries no usable name. The first occurrence of each parameter wins.
#[must_use]
pub fn parse_filename(header: &str) -> Option<String> {
    if header.len() > MAX_CONTENT_DISPOSITION_BYTES {
        return None;
    }
    let mut plain: Option<String> = None;
    let mut extended: Option<Option<String>> = None;
    for part in split_unquoted(header, ';').into_iter() {
        let Some((name, value)) = part.split_once('=') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "filename" if plain.is_none() => plain = Some(unquote(value.trim())),
            "filename*" if extended.is_none() => extended = Some(decode_ext_value(value.trim())),
            _ => {}
        }
    }
    extended
        .flatten()
        .filter(|n| !n.trim().is_empty())
        .or(plain)
}

/// Splits on `sep` outside double-quoted strings (a backslash escapes inside quotes).
fn split_unquoted(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut in_quote, mut escaped) = (0, false, false);
    for (at, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_quote && next_is_escapable(text, at) => escaped = true,
            '"' => in_quote = !in_quote,
            c if c == sep && !in_quote => {
                parts.push(&text[start..at]);
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Whether the character after the backslash at byte `at` is `"` or `\`, the only
/// escapes [`unquote`] understands.
fn next_is_escapable(text: &str, at: usize) -> bool {
    matches!(text[at + 1..].chars().next(), Some('"' | '\\'))
}

/// Removes surrounding quotes and unescapes `\"` and `\\`. Other backslashes stay
/// literal, so legacy `"C:\dir\file.txt"` values still reduce to a base name.
fn unquote(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"') else {
        return value.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                out.extend(chars.next());
            }
            other => out.push(other),
        }
    }
    out
}

/// Decodes an RFC 5987 `charset'language'percent-encoded` value.
fn decode_ext_value(value: &str) -> Option<String> {
    let mut fields = value.splitn(3, '\'');
    let (charset, _language, encoded) = (fields.next()?, fields.next()?, fields.next()?);
    let bytes = decode_strict(encoded)?;
    if charset.eq_ignore_ascii_case("utf-8") {
        String::from_utf8(bytes).ok()
    } else if charset.eq_ignore_ascii_case("iso-8859-1") {
        Some(bytes.into_iter().map(char::from).collect())
    } else {
        None
    }
}
