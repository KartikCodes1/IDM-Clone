//! The HTTP client (ARCHITECTURE 4.3, ADR-007) and manual redirect following (ADR-008).
//!
//! The client is HTTP/1.1 only, never decompresses, never follows redirects itself and has
//! no total timeout (a total deadline would kill long downloads). Every request asks for
//! `Accept-Encoding: identity`.

use std::error::Error as StdError;
use std::io;
use std::time::Duration;

use reqwest::header::{
    ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, HeaderMap, HeaderValue, LOCATION,
};
use reqwest::{Client, Response, redirect};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::http::range_decision::ResponseMeta;
use crate::http::redirect::{HopDecision, RedirectError, RedirectState, next_hop};
use crate::retry::TransferErrorKind;

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
        let builder = Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(default_headers())
            .http1_only()
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .redirect(redirect::Policy::none())
            .referer(false)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
            .http1_max_headers(MAX_RESPONSE_HEADERS)
            .tls_backend_rustls();
        let builder = match proxy {
            ProxyMode::System => builder,
            ProxyMode::None => builder.no_proxy(),
        };
        Ok(Self {
            inner: builder.build().map_err(ClientError::Build)?,
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
        let mut state = RedirectState::new(start);
        loop {
            let url = state.current().clone();
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ClientError::Cancelled),
                sent = self.inner.get(url.clone()).send() => {
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
        return kind;
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
