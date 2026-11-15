//! Minimal percent-decoding (RFC 3986), shared by the Content-Disposition and URL-path
//! name sources. Pure and allocation-light; invalid escapes are reported, not guessed.

/// Decodes `%XX` escapes. `None` when an escape is truncated or not hexadecimal.
pub(super) fn decode_strict(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            let hi = bytes.get(at + 1).copied().and_then(hex_value)?;
            let lo = bytes.get(at + 2).copied().and_then(hex_value)?;
            out.push(hi << 4 | lo);
            at += 3;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    Some(out)
}

/// Decodes `%XX` escapes, leaving malformed escapes as literal text, and replaces
/// invalid UTF-8 with U+FFFD.
pub(super) fn decode_lossy(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let pair = (bytes.get(at + 1).copied().and_then(hex_value))
            .zip(bytes.get(at + 2).copied().and_then(hex_value));
        match pair {
            Some((hi, lo)) if byte == b'%' => {
                out.push(hi << 4 | lo);
                at += 3;
            }
            _ => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}
