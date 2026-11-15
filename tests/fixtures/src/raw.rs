//! Raw TCP serving for wire violations hyper will not emit: `raw=dup_cl|cl_and_te|bad_status|
//! huge_headers|crlf_location` and `lie_cl=+N|-N`.
//!
//! The mode is detected by peeking at the first request line of a new connection, so it only
//! applies to the first request on a connection; the response always closes the connection.
//!
//! `lie_cl` honours `ranges`, `short206`, `bad_cr`, `etag`, `lm`, `cd` and `cl` only. Flags it
//! cannot honour (`slow`, `drop_after`, `stall_after`, `max_conns`, `status`, `redirect`,
//! `signed`, `auth`, `multipart`) are rejected with 400, and `head=0` turns HEAD into 405 as elsewhere.

use crate::body::FixtureBody;
use crate::content;
use crate::flags::{self, Flags, LieCl, RawMode};
use crate::response::{self, Plan, Planned};
use crate::scenario::Resolved;
use crate::server::{Conn, Rejection, header_text, log_request, parse_path};
use crate::state::Shared;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use std::io;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PEEK_BYTES: usize = 4096;
const PEEK_POLL: Duration = Duration::from_millis(10);
const PEEK_PATIENCE: Duration = Duration::from_secs(30);
const HEAD_LIMIT: usize = 16 * 1024;
const HEAD_PATIENCE: Duration = Duration::from_secs(10);
const WRITE_CHUNK: u64 = 64 * 1024;
const PAD_HEADERS: usize = 300;
const HUGE_VALUE_BYTES: usize = 160 * 1024;
const BASE_HEADERS: &str = "Content-Type: application/octet-stream\r\nConnection: close\r\n";

/// Returns true when the first request line names a raw mode (or `lie_cl`).
pub(crate) async fn wants_raw(stream: &TcpStream) -> bool {
    let mut buf = [0u8; PEEK_BYTES];
    let peeked = tokio::time::timeout(PEEK_PATIENCE, async {
        loop {
            let n = stream.peek(&mut buf).await?;
            if n == 0 || n == buf.len() || buf[..n].contains(&b'\n') {
                return Ok::<usize, io::Error>(n);
            }
            tokio::time::sleep(PEEK_POLL).await;
        }
    })
    .await;
    let Ok(Ok(n)) = peeked else { return false };
    let text = String::from_utf8_lossy(&buf[..n]);
    let query = text
        .lines()
        .next()
        .and_then(|first| first.split(' ').nth(1))
        .and_then(|target| target.split_once('?'))
        .map_or("", |(_, q)| q);
    flags::parse_query(query)
        .iter()
        .any(|(name, _)| *name == "raw" || *name == "lie_cl")
}

/// Serves exactly one request on `stream` and closes it.
pub(crate) async fn serve(mut stream: TcpStream, shared: &Shared, conn: Conn) {
    let Some(req) = read_request(&mut stream).await else {
        // Best effort: the peer may already be gone, and a clear 400 beats a silent close.
        let _ = write_simple(&mut stream, StatusCode::BAD_REQUEST, "malformed request").await;
        let _ = stream.shutdown().await;
        return;
    };
    let pairs = flags::parse_query(req.uri().query().unwrap_or(""));
    log_request(&req, conn, shared, &pairs);
    let _slot = shared.log.try_enter(None);
    let result = match prepare(&req, &pairs) {
        Ok((flags, seed, size)) => respond(&mut stream, &req, shared, &flags, (seed, size)).await,
        Err(rejection) => {
            write_simple(&mut stream, rejection.status(), &rejection.to_string()).await
        }
    };
    // The peer may already have reset the connection (that is often the point of the test).
    let _ = result;
    let _ = stream.shutdown().await;
}

fn prepare(req: &Request<()>, pairs: &[(&str, &str)]) -> Result<(Flags, u64, u64), Rejection> {
    let (seed, size) = parse_path(req.uri().path())?;
    let flags = Flags::from_pairs(pairs)?;
    match *req.method() {
        Method::GET => {}
        Method::HEAD if flags.head || flags.raw.is_some() => {}
        _ => return Err(Rejection::MethodNotAllowed),
    }
    if flags.raw.is_none() {
        reject_unsupported_with_lie_cl(&flags)?;
    }
    Ok((flags, seed, size))
}

/// `lie_cl` is served on the raw socket path, which applies none of these flags.
fn reject_unsupported_with_lie_cl(flags: &Flags) -> Result<(), Rejection> {
    let unsupported = [
        (flags.slow_bps.is_some(), "slow"),
        (flags.drop_after.is_some(), "drop_after"),
        (flags.stall_after.is_some(), "stall_after"),
        (flags.max_conns.is_some(), "max_conns"),
        (flags.status.is_some(), "status"),
        (flags.redirect.is_some(), "redirect"),
        (flags.signed.is_some(), "signed"),
        (flags.auth_basic, "auth"),
        (flags.multipart, "multipart"),
    ];
    match unsupported.iter().find(|(set, _)| *set) {
        Some((_, name)) => Err(Rejection::UnsupportedWithLieCl(name)),
        None => Ok(()),
    }
}

async fn respond(
    stream: &mut TcpStream,
    req: &Request<()>,
    shared: &Shared,
    flags: &Flags,
    (seed, size): (u64, u64),
) -> io::Result<()> {
    if let Some(mode) = flags.raw {
        return write_mode(stream, mode, seed, size).await;
    }
    let Some(lie) = flags.lie_cl else {
        return write_simple(stream, StatusCode::BAD_REQUEST, "no raw mode requested").await;
    };
    let resolved = Resolved::new(
        flags,
        shared,
        req.uri().path(),
        header_text(req, header::RANGE),
        header_text(req, header::IF_RANGE),
    );
    let is_head = *req.method() == Method::HEAD;
    let target = resolved.target(is_head, seed, size);
    match response::plan(flags, &target) {
        Planned::Content(plan) => {
            write_lying(stream, &plan, target.content_seed(), is_head, lie).await
        }
        Planned::Early(resp) => write_early(stream, resp).await,
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request<()>> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    let complete = tokio::time::timeout(HEAD_PATIENCE, async {
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < HEAD_LIMIT {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return false,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        true
    })
    .await;
    if complete != Ok(true) {
        return None;
    }
    parse_head(&String::from_utf8_lossy(&head))
}

fn parse_head(text: &str) -> Option<Request<()>> {
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?, first.next()?);
    let mut headers = HeaderMap::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':')?;
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.trim().as_bytes()),
            HeaderValue::from_str(value.trim()),
        ) {
            headers.append(name, value);
        }
    }
    let mut req = Request::builder()
        .method(method)
        .uri(target)
        .body(())
        .ok()?;
    *req.headers_mut() = headers;
    Some(req)
}

fn status_line(status: StatusCode) -> String {
    format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
}

fn header_lines<'a>(headers: impl Iterator<Item = (&'a HeaderName, &'a HeaderValue)>) -> String {
    headers
        .map(|(name, value)| format!("{name}: {}\r\n", String::from_utf8_lossy(value.as_bytes())))
        .collect()
}

async fn write_simple(stream: &mut TcpStream, status: StatusCode, message: &str) -> io::Result<()> {
    let head = format!(
        "{}Content-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{message}",
        status_line(status),
        message.len()
    );
    stream.write_all(head.as_bytes()).await
}

/// Writes a finished hyper-style response (400/416) with an explicit length.
async fn write_early(stream: &mut TcpStream, resp: Response<FixtureBody>) -> io::Result<()> {
    let (parts, body) = resp.into_parts();
    let bytes = body
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    let head = format!(
        "{}{}Content-Length: {}\r\nConnection: close\r\n\r\n",
        status_line(parts.status),
        header_lines(parts.headers.iter()),
        bytes.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&bytes).await
}

/// Streams generated bytes `[start, start + len)`.
async fn write_body(stream: &mut TcpStream, seed: u64, start: u64, len: u64) -> io::Result<()> {
    let mut pos = start;
    let end = start.saturating_add(len);
    while pos < end {
        let n = WRITE_CHUNK.min(end - pos);
        let mut buf = vec![0u8; usize::try_from(n).unwrap_or(0)];
        content::fill(seed, pos, &mut buf);
        stream.write_all(&buf).await?;
        pos += n;
    }
    Ok(())
}

async fn write_lying(
    stream: &mut TcpStream,
    plan: &Plan,
    seed: u64,
    is_head: bool,
    lie: LieCl,
) -> io::Result<()> {
    let claimed = match lie {
        LieCl::Over(n) => plan.len.saturating_add(n),
        LieCl::Under(n) => plan.len.saturating_sub(n),
    };
    let head = format!(
        "{}{}Content-Length: {claimed}\r\nConnection: close\r\n\r\n",
        status_line(plan.status),
        header_lines(plan.headers.iter().map(|(n, v)| (n, v)))
    );
    stream.write_all(head.as_bytes()).await?;
    if is_head {
        return Ok(());
    }
    write_body(stream, seed, plan.start, plan.len).await
}

async fn write_mode(stream: &mut TcpStream, mode: RawMode, seed: u64, size: u64) -> io::Result<()> {
    match mode {
        RawMode::DupCl => {
            let head = format!(
                "HTTP/1.1 200 OK\r\n{BASE_HEADERS}Content-Length: {size}\r\nContent-Length: {}\r\n\r\n",
                size.saturating_add(1)
            );
            stream.write_all(head.as_bytes()).await?;
            write_body(stream, seed, 0, size).await
        }
        RawMode::ClAndTe => {
            let head = format!(
                "HTTP/1.1 200 OK\r\n{BASE_HEADERS}Content-Length: {size}\r\nTransfer-Encoding: chunked\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await?;
            write_chunked(stream, seed, size).await
        }
        RawMode::BadStatus => {
            let head = "HTTP/1.1 2x0 Broken\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(head.as_bytes()).await
        }
        RawMode::HugeHeaders => stream.write_all(&huge_head()).await,
        RawMode::CrlfLocation => {
            // The bare LF before `X-Injected` is the violation.
            let head = format!(
                "HTTP/1.1 302 Found\r\nLocation: /f/{seed}/{size}\nX-Injected: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(head.as_bytes()).await
        }
    }
}

fn huge_head() -> Vec<u8> {
    let mut head = String::from("HTTP/1.1 200 OK\r\n");
    for i in 0..PAD_HEADERS {
        head.push_str(&format!("X-Pad-{i}: {}\r\n", "a".repeat(64)));
    }
    head.push_str(&format!("X-Huge: {}\r\n", "b".repeat(HUGE_VALUE_BYTES)));
    head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    head.into_bytes()
}

async fn write_chunked(stream: &mut TcpStream, seed: u64, size: u64) -> io::Result<()> {
    let mut pos = 0;
    while pos < size {
        let n = WRITE_CHUNK.min(size - pos);
        let mut buf = vec![0u8; usize::try_from(n).unwrap_or(0)];
        content::fill(seed, pos, &mut buf);
        stream.write_all(format!("{n:x}\r\n").as_bytes()).await?;
        stream.write_all(&buf).await?;
        stream.write_all(b"\r\n").await?;
        pos += n;
    }
    stream.write_all(b"0\r\n\r\n").await
}
