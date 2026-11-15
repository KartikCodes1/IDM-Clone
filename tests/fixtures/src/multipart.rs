//! `multipart=1`: answers range requests as `multipart/byteranges` (RFC 9110 section 14.6).

use crate::body::FixtureBody;
use crate::content;
use crate::flags::Flags;
use crate::range::RangeOutcome;
use crate::response::{Target, if_range_matches, metadata, set, unsatisfiable};
use bytes::Bytes;
use http::{Response, StatusCode, header};

const BOUNDARY: &str = "FORGEDM_BOUNDARY";
/// Parts are assembled in memory, so larger requests fall back to the normal behaviour.
const MAX_BODY_BYTES: u64 = 8 * 1024 * 1024;
/// Header and boundary overhead per part is not counted in the payload cap, so parts are capped.
const MAX_PARTS: usize = 256;

/// A satisfiable inclusive range `(start, end)`.
type Part = (u64, u64);

/// Parses every comma-separated range; `None` when the header is not a usable range request.
fn parse_parts(raw: &str, size: u64) -> Option<Vec<Part>> {
    let spec = raw.trim().strip_prefix("bytes=")?;
    let mut parts = Vec::new();
    for piece in spec.split(',') {
        match crate::range::evaluate(&format!("bytes={}", piece.trim()), size) {
            RangeOutcome::Partial { start, end } => parts.push((start, end)),
            RangeOutcome::Unsatisfiable => {}
            RangeOutcome::Full => return None,
        }
    }
    Some(parts)
}

fn assemble(seed: u64, size: u64, parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(start, end) in parts {
        let head = format!(
            "--{BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\
             Content-Range: bytes {start}-{end}/{size}\r\n\r\n"
        );
        out.extend_from_slice(head.as_bytes());
        let len = usize::try_from(end.saturating_sub(start).saturating_add(1)).unwrap_or(0);
        let at = out.len();
        out.resize(at + len, 0);
        content::fill(seed, start, &mut out[at..]);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

/// Builds the multipart 206, or `None` when the request is not a (small) valid range request.
#[must_use]
pub fn try_build(flags: &Flags, target: &Target<'_>) -> Option<Response<FixtureBody>> {
    let raw = target.range?;
    if !flags.ranges || !if_range_matches(flags, target) {
        return None;
    }
    let parts = parse_parts(raw, target.size)?;
    if parts.len() > MAX_PARTS {
        return None;
    }
    if parts.is_empty() {
        return Some(unsatisfiable(target.size));
    }
    let total: u64 = parts.iter().fold(0_u64, |acc, (s, e)| {
        acc.saturating_add(e.saturating_sub(*s).saturating_add(1))
    });
    if total > MAX_BODY_BYTES {
        return None;
    }
    let body = assemble(target.content_seed(), target.size, &parts);
    let len = body.len();
    let mut resp = Response::new(if target.is_head {
        FixtureBody::empty()
    } else {
        FixtureBody::from_bytes(Bytes::from(body))
    });
    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
    for (name, value) in metadata(flags, target) {
        resp.headers_mut().insert(name, value);
    }
    set(
        &mut resp,
        header::CONTENT_TYPE,
        &format!("multipart/byteranges; boundary={BOUNDARY}"),
    );
    set(&mut resp, header::CONTENT_LENGTH, &len.to_string());
    Some(resp)
}
