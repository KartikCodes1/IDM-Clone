//! Redirect policy and header scoping (ADR-008, THREAT_MODEL NET-04, NET-06, Inv-09, Inv-10).
//!
//! Redirects are followed manually; [`next_hop`] is the pure decision for each hop. It never
//! mutates the [`RedirectState`] it is given: a followed hop carries the next state.

use std::collections::HashSet;

use forgedm_protocol::DownloadErrorCode;
use thiserror::Error;
use url::Url;

use crate::url_policy::classify_host;

/// Most redirects followed for one request chain.
pub const MAX_REDIRECTS: u32 = 10;

/// Longest accepted `Location` value in bytes (8 KiB).
pub const MAX_LOCATION_BYTES: usize = 8 * 1024;

/// Why a hop was blocked (`redirect_blocked`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BlockReason {
    /// The target scheme is not http or https.
    #[error("redirect to a non-http(s) scheme")]
    Scheme,
    /// A public origin redirected to a loopback, private or otherwise local destination.
    #[error("redirect from a public origin to a local destination")]
    PrivateDestination,
    /// No usable `Location` header.
    #[error("redirect without a Location")]
    MissingLocation,
    /// `Location` longer than [`MAX_LOCATION_BYTES`].
    #[error("Location longer than 8 KiB")]
    LocationTooLong,
    /// `Location` could not be resolved to a URL with a host.
    #[error("unresolvable Location")]
    InvalidLocation,
    /// The target URL embeds credentials.
    #[error("redirect target carries userinfo")]
    Userinfo,
    /// `Location` contains control characters.
    #[error("Location contains control characters")]
    ControlChars,
}

/// A redirect the policy refuses to follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RedirectError {
    /// More than [`MAX_REDIRECTS`] hops.
    #[error("too many redirects")]
    TooManyRedirects,
    /// The target was already visited in this chain.
    #[error("redirect loop")]
    Loop,
    /// https redirected to http.
    #[error("redirect downgrades https to http")]
    Downgrade,
    /// Blocked by policy.
    #[error("redirect blocked: {0}")]
    Blocked(BlockReason),
}

impl RedirectError {
    /// The `last_error.code` this failure maps to (Appendix A.7).
    #[must_use]
    pub const fn code(self) -> DownloadErrorCode {
        match self {
            Self::TooManyRedirects => DownloadErrorCode::TooManyRedirects,
            Self::Loop => DownloadErrorCode::RedirectLoop,
            Self::Downgrade => DownloadErrorCode::RedirectDowngrade,
            Self::Blocked(_) => DownloadErrorCode::RedirectBlocked,
        }
    }
}

/// Where a redirect chain stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectState {
    original: Url,
    original_local: bool,
    current: Url,
    visited: HashSet<String>,
    hops: u32,
}

/// The URL without its fragment, as the loop-detection key.
fn visit_key(url: &Url) -> String {
    let mut key = url.clone();
    key.set_fragment(None);
    key.into()
}

impl RedirectState {
    /// Starts a chain at the user's original URL.
    #[must_use]
    pub fn new(original: &Url) -> Self {
        let mut current = original.clone();
        current.set_fragment(None);
        let original_local = current.host().is_some_and(|h| classify_host(&h).is_local());
        Self {
            original: current.clone(),
            original_local,
            visited: HashSet::from([visit_key(&current)]),
            current,
            hops: 0,
        }
    }

    /// Redirects followed so far.
    #[must_use]
    pub const fn hops(&self) -> u32 {
        self.hops
    }

    /// The URL the next request goes to.
    #[must_use]
    pub const fn current(&self) -> &Url {
        &self.current
    }

    /// Whether the original URL itself pointed at a local destination.
    #[must_use]
    pub const fn original_is_local(&self) -> bool {
        self.original_local
    }

    /// Whether origin-bound headers (Authorization, Cookie, custom headers) may be sent to
    /// `target`: only when it has the scheme, host and port of the ORIGINAL URL, never judged
    /// against the previous hop.
    #[must_use]
    pub fn allows_origin_bound_headers(&self, target: &Url) -> bool {
        self.original.origin() == target.origin()
    }
}

/// A redirect to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// The resolved target (no fragment).
    pub url: Url,
    /// The chain state after this hop.
    pub state: RedirectState,
    /// Whether origin-bound headers may accompany the request to `url`.
    pub send_origin_bound_headers: bool,
}

/// The decision for one response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HopDecision {
    /// Not a followed redirect status: this response is final (any 3xx outside
    /// 301/302/303/307/308 is the caller's to classify).
    Final,
    /// Follow the redirect.
    Follow(Box<Hop>),
    /// Refuse the redirect.
    Reject(RedirectError),
}

/// Whether `status` is one of the redirects followed: 301, 302, 303, 307, 308.
#[must_use]
pub const fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Decides what to do with a response of `status` carrying `location`.
#[must_use]
pub fn next_hop(state: &RedirectState, status: u16, location: Option<&str>) -> HopDecision {
    if !is_redirect_status(status) {
        return HopDecision::Final;
    }
    match resolve_hop(state, location) {
        Ok(hop) => HopDecision::Follow(Box::new(hop)),
        Err(error) => HopDecision::Reject(error),
    }
}

/// Resolves and validates the target of a `Location` header.
fn parse_target(state: &RedirectState, location: Option<&str>) -> Result<Url, RedirectError> {
    let blocked = |reason| RedirectError::Blocked(reason);
    let location = location
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .ok_or(blocked(BlockReason::MissingLocation))?;
    if location.len() > MAX_LOCATION_BYTES {
        return Err(blocked(BlockReason::LocationTooLong));
    }
    if location.chars().any(char::is_control) {
        return Err(blocked(BlockReason::ControlChars));
    }
    let mut target = state
        .current
        .join(location)
        .map_err(|_| blocked(BlockReason::InvalidLocation))?;
    if !matches!(target.scheme(), "http" | "https") {
        return Err(blocked(BlockReason::Scheme));
    }
    if target.host().is_none() {
        return Err(blocked(BlockReason::InvalidLocation));
    }
    if !target.username().is_empty() || target.password().is_some() {
        return Err(blocked(BlockReason::Userinfo));
    }
    target.set_fragment(None);
    Ok(target)
}

fn resolve_hop(state: &RedirectState, location: Option<&str>) -> Result<Hop, RedirectError> {
    if state.hops >= MAX_REDIRECTS {
        return Err(RedirectError::TooManyRedirects);
    }
    let target = parse_target(state, location)?;
    if state.current.scheme() == "https" && target.scheme() == "http" {
        return Err(RedirectError::Downgrade);
    }
    let local = target.host().is_some_and(|h| classify_host(&h).is_local());
    if local && !state.original_local {
        return Err(RedirectError::Blocked(BlockReason::PrivateDestination));
    }
    let key = visit_key(&target);
    if state.visited.contains(&key) {
        return Err(RedirectError::Loop);
    }
    let send_origin_bound_headers = state.allows_origin_bound_headers(&target);
    let mut visited = state.visited.clone();
    visited.insert(key);
    let next = RedirectState {
        original: state.original.clone(),
        original_local: state.original_local,
        current: target.clone(),
        visited,
        hops: state.hops + 1,
    };
    Ok(Hop {
        url: target,
        state: next,
        send_origin_bound_headers,
    })
}
