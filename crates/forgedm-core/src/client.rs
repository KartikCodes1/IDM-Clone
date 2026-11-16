//! The HTTP client (ARCHITECTURE 4.3, ADR-007) and manual redirect following (ADR-008).
//!
//! The client is HTTP/1.1 only, never decompresses, never follows redirects itself and has
//! no total timeout (a total deadline would kill long downloads). Every request asks for
//! `Accept-Encoding: identity`.

use std::error::Error as StdError;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, HeaderMap, HeaderValue, IF_RANGE, LOCATION, RANGE,
};
use reqwest::{Client, Method, Response, redirect};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::http::content_range::RangeRequest;
use crate::http::range_decision::ResponseMeta;
use crate::http::redirect::{HopDecision, RedirectError, RedirectState, next_hop};
use crate::retry::{TransferErrorKind, parse_retry_after};

/// Connect timeout (A.9).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Read (stall) timeout (A.9).
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Idle connection lifetime (implementation constant, ARCHITECTURE 4.3).
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Idle connections kept per host (implementation constant).
const POOL_MAX_IDLE_PER_HOST: usize = 16;
/// Most response headers accepted (A.9 `MAX_RESPONSE_HEADERS`).
const MAX_RESPONSE_HEADERS: usize = 128;
/// `ForgeDM/<version>`.
pub const USER_AGENT: &str = concat!("ForgeDM/", env!("CARGO_PKG_VERSION"));
/// WSAHOST_NOT_FOUND: the name does not exist.
const WSAHOST_NOT_FOUND: i32 = 11001;
/// WSATRY_AGAIN: a temporary resolver failure.
const WSATRY_AGAIN: i32 = 11002;

/// Where proxy settings come from (the agent's `--proxy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyMode {
    /// reqwest's system and environment proxy detection.
    System,
    /// Connect directly.
    None,
}

/// The client could not be built or a request failed.
#[derive(Debug, Error)]
pub enum ClientError {
    /// The TLS or HTTP stack could not be initialised.
    #[error("cannot build the HTTP client")]
    Build(#[source] reqwest::Error),
    /// A redirect was refused by the policy.
    #[error("redirect refused: {0}")]
    Redirect(#[from] RedirectError),
    /// A transport failure, already classified.
    #[error("transport failure: {0:?}")]
    Transport(TransferErrorKind),
    /// The caller's token was cancelled.
    #[error("request cancelled")]
    Cancelled,
}

/// What to send: the method plus the optional `Range` and `If-Range`.
#[derive(Debug, Clone)]
pub struct FetchSpec {
    /// `GET` or `HEAD`.
    pub method: Method,
    /// The window to ask for, as `Range: bytes=start-last`.
    pub range: Option<RangeRequest>,
    /// The `If-Range` validator, sent byte-identical.
    pub if_range: Option<String>,
}

impl FetchSpec {
    /// A plain GET.
    #[must_use]
    pub const fn get() -> Self {
        Self {
            method: Method::GET,
            range: None,
            if_range: None,
        }
    }

    /// A HEAD. It has no body; callers bound it with their own deadline (the client has no
    /// total timeout: a total deadline would kill long downloads).
    #[must_use]
    pub const fn head() -> Self {
        Self {
            method: Method::HEAD,
            range: None,
            if_range: None,
        }
    }

    /// A GET of `range`, with `If-Range` when a strong validator is known.
    #[must_use]
    pub const fn ranged(range: RangeRequest, if_range: Option<String>) -> Self {
        Self {
            method: Method::GET,
            range: Some(range),
            if_range,
        }
    }
}

/// Client settings beyond the proxy mode. The defaults are the A.9 values; the overrides
/// exist so tests can use short timeouts and fixed name resolution.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Where proxy settings come from.
    pub proxy: ProxyMode,
    /// Connect timeout.
    pub connect_timeout: Duration,
    /// Read (stall) timeout.
    pub read_timeout: Duration,
    /// Fixed name resolutions (`host` to address), bypassing DNS for those names.
    pub resolve: Vec<(String, SocketAddr)>,
}

impl ClientOptions {
    /// The A.9 timeouts, no fixed resolutions.
    #[must_use]
    pub const fn new(proxy: ProxyMode) -> Self {
        Self {
            proxy,
            connect_timeout: CONNECT_TIMEOUT,
            read_timeout: READ_TIMEOUT,
            resolve: Vec::new(),
        }
    }
}

/// A response after redirects.
#[derive(Debug)]
pub struct Fetched {
    /// The final response (body unread).
    pub response: Response,
    /// The URL that produced it.
    pub final_url: Url,
    /// Redirect hops followed.
    pub hops: u32,
}

/// The configured HTTP client.
#[derive(Debug, Clone)]
pub struct HttpClient {
    inner: Client,
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    headers
}

impl HttpClient {
    /// Builds the client per ADR-007.
    ///
    /// # Errors
    /// [`ClientError::Build`] when the TLS backend cannot initialise.
    pub fn new(proxy: ProxyMode) -> Result<Self, ClientError> {
        Self::with_options(&ClientOptions::new(proxy))
    }

    /// Builds the client per ADR-007 with `options`.
    ///
    /// # Errors
    /// [`ClientError::Build`] when the TLS backend cannot initialise.
    pub fn with_options(options: &ClientOptions) -> Result<Self, ClientError> {
        let mut builder = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(default_headers())
            .http1_only()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(redirect::Policy::none())
            .referer(false)
            .connect_timeout(options.connect_timeout)
            .read_timeout(options.read_timeout)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .http1_max_headers(MAX_RESPONSE_HEADERS)
            .tls_backend_rustls();
        for (host, addr) in &options.resolve {
            builder = builder.resolve(host, *addr);
        }
        let builder = match options.proxy {
            ProxyMode::System => builder,
            ProxyMode::None => builder.no_proxy(),
        };
        Ok(Self {
            inner: builder
                .build()
                .map_err(|e| ClientError::Build(e.without_url()))?,
        })
    }

    /// GETs `start`, following redirects through the pure policy until a final response.
    /// Redirect bodies are dropped unread.
    ///
    /// # Errors
    /// [`ClientError::Redirect`] for a refused hop, [`ClientError::Transport`] for network
    /// failures, [`ClientError::Cancelled`] when `cancel` fires first.
    pub async fn get(
        &self,
        start: &Url,
        cancel: &CancellationToken,
    ) -> Result<Fetched, ClientError> {
        self.fetch(start, &FetchSpec::get(), cancel).await
    }

    /// Sends `spec` to `start`, following redirects through the pure policy (the same
    /// headers go to every hop) until a final response.
    ///
    /// # Errors
    /// As [`get`](Self::get).
    pub async fn fetch(
        &self,
        start: &Url,
        spec: &FetchSpec,
        cancel: &CancellationToken,
    ) -> Result<Fetched, ClientError> {
        let mut state = RedirectState::new(start);
        loop {
            let url = state.current().clone();
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ClientError::Cancelled),
                sent = self.request(url.clone(), spec).send() => {
                    sent.map_err(|e| ClientError::Transport(classify_reqwest(&e)))?
                }
            };
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            match next_hop(&state, response.status().as_u16(), location.as_deref()) {
                HopDecision::Final => {
                    return Ok(Fetched {
                        response,
                        final_url: url,
                        hops: state.hops(),
                    });
                }
                HopDecision::Follow(hop) => state = hop.state,
                HopDecision::Reject(error) => return Err(error.into()),
            }
        }
    }
}

impl HttpClient {
    fn request(&self, url: Url, spec: &FetchSpec) -> reqwest::RequestBuilder {
        let mut builder = self.inner.request(spec.method.clone(), url);
        if let Some(value) = spec.range.and_then(|r| r.header_value()) {
            builder = builder.header(RANGE, value);
        }
        if let Some(value) = &spec.if_range {
            builder = builder.header(IF_RANGE, value);
        }
        builder
    }
}

/// The `Retry-After` of `response`, as a delay (an HTTP-date is read against the response's
/// own `Date`). `None` when absent or unusable.
#[must_use]
pub fn retry_after_of(response: &Response) -> Option<Duration> {
    let headers = response.headers();
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let date = headers
        .get(reqwest::header::DATE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| httpdate::parse_http_date(v).ok());
    parse_retry_after(value, date)
}

/// The raw `Content-Disposition` of `response`. [`crate::naming`] ignores values above 4 KiB.
#[must_use]
pub fn content_disposition_of(response: &Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

/// The parts of `response` the range and probe decisions read.
#[must_use]
pub fn response_meta(response: &Response) -> ResponseMeta {
    let headers = response.headers();
    let text = |name: &str| {
        let joined: Vec<&str> = headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        (!joined.is_empty()).then(|| joined.join(", "))
    };
    // An unreadable (non-ASCII) Content-Encoding must not pass as identity.
    let encoding_unreadable = headers
        .get_all(CONTENT_ENCODING)
        .iter()
        .any(|v| v.to_str().is_err());
    ResponseMeta {
        status: response.status().as_u16(),
        content_range: text("content-range"),
        content_length: text("content-length"),
        content_encoding: if encoding_unreadable {
            Some("unreadable".to_owned())
        } else {
            text("content-encoding")
        },
        content_type: text("content-type"),
        validators: crate::http::validators::Validators {
            etag: text("etag"),
            last_modified: text("last-modified"),
            date: text("date"),
        },
    }
}

/// Reduces a reqwest failure to a [`TransferErrorKind`] by walking the error chain: kinds
/// and OS codes, never message text.
#[must_use]
pub fn classify_reqwest(err: &reqwest::Error) -> TransferErrorKind {
    let io_error = io_in_chain(err);
    if let Some(kind) = io_error.and_then(classify_io_error) {
        if err.is_connect() && kind == TransferErrorKind::PrematureEof {
            // A connection that ends while still connecting ended in the TLS handshake.
            return TransferErrorKind::Tls;
        }
        return kind;
    }
    if err.is_connect() && io_error.is_some_and(is_wrapped_tls_error) {
        return TransferErrorKind::Tls;
    }
    if err.is_timeout() {
        return if err.is_connect() {
            TransferErrorKind::ConnectTimeout
        } else {
            TransferErrorKind::ReadTimeout
        };
    }
    if err.is_connect() {
        return TransferErrorKind::ConnectRefused;
    }
    if err.is_redirect() {
        return TransferErrorKind::TooManyRedirects;
    }
    if err.is_body() || err.is_decode() {
        return TransferErrorKind::PrematureEof;
    }
    TransferErrorKind::MalformedFraming
}

/// rustls reports certificate and handshake failures wrapped as an `io::Error` of kind
/// `Other` carrying the TLS error. Plain-text connects never produce one, so in the connect
/// phase it is a TLS failure (this crate cannot name the rustls error type to downcast).
fn is_wrapped_tls_error(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::Other && err.get_ref().is_some()
}

fn io_in_chain<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a io::Error> {
    let mut current: Option<&'a (dyn StdError + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<io::Error>() {
            return Some(io);
        }
        current = e.source();
    }
    None
}

fn classify_io_error(err: &io::Error) -> Option<TransferErrorKind> {
    use io::ErrorKind as K;
    if let Some(code) = err.raw_os_error() {
        match code {
            WSAHOST_NOT_FOUND => return Some(TransferErrorKind::DnsFailure),
            WSATRY_AGAIN => return Some(TransferErrorKind::DnsTemporary),
            _ => {}
        }
    }
    match err.kind() {
        K::ConnectionRefused => Some(TransferErrorKind::ConnectRefused),
        K::ConnectionReset | K::ConnectionAborted | K::BrokenPipe => {
            Some(TransferErrorKind::ConnectionReset)
        }
        K::TimedOut => Some(TransferErrorKind::ConnectTimeout),
        K::UnexpectedEof => Some(TransferErrorKind::PrematureEof),
        // rustls reports certificate and handshake failures as InvalidData.
        K::InvalidData => Some(TransferErrorKind::Tls),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_and_reset_codes_are_classified() {
        let dns = io::Error::from_raw_os_error(WSAHOST_NOT_FOUND);
        assert_eq!(classify_io_error(&dns), Some(TransferErrorKind::DnsFailure));
        let again = io::Error::from_raw_os_error(WSATRY_AGAIN);
        assert_eq!(
            classify_io_error(&again),
            Some(TransferErrorKind::DnsTemporary)
        );
        let reset = io::Error::from(io::ErrorKind::ConnectionReset);
        assert_eq!(
            classify_io_error(&reset),
            Some(TransferErrorKind::ConnectionReset)
        );
        let invalid = io::Error::from(io::ErrorKind::InvalidData);
        assert_eq!(classify_io_error(&invalid), Some(TransferErrorKind::Tls));
    }

    #[test]
    fn a_wrapped_tls_error_is_told_from_a_plain_other_error() {
        let wrapped = io::Error::other(io::Error::from(io::ErrorKind::InvalidInput));
        assert!(is_wrapped_tls_error(&wrapped));
        assert!(!is_wrapped_tls_error(&io::Error::from(
            io::ErrorKind::Other
        )));
        assert!(!is_wrapped_tls_error(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
    }

    #[test]
    fn the_client_builds_in_both_proxy_modes() {
        assert!(HttpClient::new(ProxyMode::None).is_ok());
        assert!(HttpClient::new(ProxyMode::System).is_ok());
    }

    #[test]
    fn default_headers_ask_for_identity() {
        let headers = default_headers();
        assert_eq!(headers.get(ACCEPT_ENCODING).unwrap(), "identity");
        assert_eq!(headers.get(ACCEPT).unwrap(), "*/*");
    }
}
