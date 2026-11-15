//! Builds fixture responses from parsed flags and the evaluated range.

use crate::body::{Delivery, FixtureBody};
use crate::flags::{EtagMode, Flags};
use crate::range::{self, RangeOutcome};
use bytes::Bytes;
use http::{HeaderValue, Response, StatusCode, header};

/// The request parts that influence the response.
pub struct Target<'a> {
    /// True for HEAD.
    pub is_head: bool,
    /// Content seed from the path.
    pub seed: u64,
    /// Representation size from the path.
    pub size: u64,
    /// Raw `Range` header.
    pub range: Option<&'a str>,
    /// Raw `If-Range` header.
    pub if_range: Option<&'a str>,
}

/// A non-success response with a short plain-text body.
#[must_use]
pub fn error_response(status: StatusCode, message: &str) -> Response<FixtureBody> {
    let mut resp = Response::new(FixtureBody::from_bytes(Bytes::from(message.to_owned())));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    if status == StatusCode::METHOD_NOT_ALLOWED {
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET"));
    }
    resp
}

fn etag_value(flags: &Flags, seed: u64, size: u64) -> Option<String> {
    match flags.etag {
        EtagMode::Strong => Some(format!("\"{seed}-{size}\"")),
        EtagMode::Weak => Some(format!("W/\"{seed}-{size}\"")),
        EtagMode::None => None,
    }
}

/// True when `If-Range` still matches the current validator (strong ETag or exact Last-Modified).
fn if_range_matches(flags: &Flags, target: &Target<'_>) -> bool {
    let Some(candidate) = target.if_range else {
        return true;
    };
    let candidate = candidate.trim();
    let strong_etag = etag_value(flags, target.seed, target.size).filter(|e| !e.starts_with("W/"));
    strong_etag.as_deref() == Some(candidate) || flags.last_modified.as_deref() == Some(candidate)
}

fn outcome(flags: &Flags, target: &Target<'_>) -> RangeOutcome {
    match target.range {
        Some(raw) if flags.ranges && if_range_matches(flags, target) => {
            range::evaluate(raw, target.size)
        }
        _ => RangeOutcome::Full,
    }
}

fn set(resp: &mut Response<FixtureBody>, name: header::HeaderName, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        resp.headers_mut().insert(name, v);
    }
}

fn apply_metadata(resp: &mut Response<FixtureBody>, flags: &Flags, target: &Target<'_>) {
    set(resp, header::CONTENT_TYPE, "application/octet-stream");
    if flags.ranges {
        set(resp, header::ACCEPT_RANGES, "bytes");
    }
    if let Some(etag) = etag_value(flags, target.seed, target.size) {
        set(resp, header::ETAG, &etag);
    }
    if let Some(lm) = &flags.last_modified {
        set(resp, header::LAST_MODIFIED, lm);
    }
    if flags.gzip_header {
        set(resp, header::CONTENT_ENCODING, "gzip");
    }
    if let Some(cd) = flags
        .content_disposition
        .as_deref()
        .and_then(|b| HeaderValue::from_bytes(b).ok())
    {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
}

/// Builds the response for a validated GET/HEAD request.
///
/// A 400 response is returned when `cd` is not a valid header value.
pub fn build(flags: &Flags, target: &Target<'_>) -> Response<FixtureBody> {
    if flags
        .content_disposition
        .as_deref()
        .is_some_and(|b| HeaderValue::from_bytes(b).is_err())
    {
        return error_response(StatusCode::BAD_REQUEST, "invalid cd flag");
    }
    let (status, start, len) = match outcome(flags, target) {
        RangeOutcome::Full => (StatusCode::OK, 0, target.size),
        RangeOutcome::Partial { start, end } => {
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        RangeOutcome::Unsatisfiable => {
            let mut resp =
                error_response(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable");
            set(
                &mut resp,
                header::CONTENT_RANGE,
                &format!("bytes */{}", target.size),
            );
            return resp;
        }
    };
    let delivery = Delivery {
        slow_bps: flags.slow_bps,
        drop_after: flags.drop_after,
    };
    let body = if target.is_head {
        FixtureBody::empty()
    } else {
        FixtureBody::generated(target.seed, start, len, flags.content_length, delivery)
    };
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    apply_metadata(&mut resp, flags, target);
    if status == StatusCode::PARTIAL_CONTENT {
        let range = format!("bytes {}-{}/{}", start, start + len - 1, target.size);
        set(&mut resp, header::CONTENT_RANGE, &range);
    }
    if flags.content_length {
        set(&mut resp, header::CONTENT_LENGTH, &len.to_string());
    }
    resp
}
