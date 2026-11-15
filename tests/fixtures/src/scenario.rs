//! Stateful and short-circuit scenarios decided before any content is produced:
//! `auth`, `status`/`fail_first`/`retry_after`, `redirect`, `signed_expiring`, plus the
//! counters behind `etag=mutable` and `range_200_after`.

use crate::body::FixtureBody;
use crate::flags::{EtagMode, Flags, RedirectMode};
use crate::response::{self, Target, error_response, set};
use crate::state::Shared;
use http::{HeaderValue, Response, StatusCode, header};

const CROSS_HOST: &str = "cross.test";
const PRIVATE_HOST: &str = "192.168.0.10";
const LOOPBACK_HOST: &str = "127.0.0.1";
const DEFAULT_SIGNED_TTL: u64 = 1;

/// The request facts the scenarios need.
pub struct Ctx<'a> {
    /// Request path (`/f/{seed}/{size}`).
    pub path: &'a str,
    /// Raw query string.
    pub query: &'a str,
    /// Representation size.
    pub size: u64,
    /// True when an `Authorization: Basic ...` header was sent.
    pub has_basic_auth: bool,
    /// Shared server state.
    pub shared: &'a Shared,
}

fn counter_key(kind: &str, flags: &Flags, path: &str) -> String {
    Shared::key(kind, flags.test_key.as_deref(), path)
}

/// Returns the answer for scenarios that pre-empt the content, or `None` to serve content.
#[must_use]
pub fn early_response(flags: &Flags, ctx: &Ctx<'_>) -> Option<Response<FixtureBody>> {
    auth_challenge(flags, ctx)
        .or_else(|| signed_expired(flags, ctx))
        .or_else(|| forced_status(flags, ctx))
        .or_else(|| redirect(flags, ctx))
}

fn auth_challenge(flags: &Flags, ctx: &Ctx<'_>) -> Option<Response<FixtureBody>> {
    (flags.auth_basic && !ctx.has_basic_auth).then(unauthorized)
}

fn unauthorized() -> Response<FixtureBody> {
    let mut resp = error_response(StatusCode::UNAUTHORIZED, "authorization required");
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Basic realm=\"forgedm-fixture\""),
    );
    resp
}

fn signed_expired(flags: &Flags, ctx: &Ctx<'_>) -> Option<Response<FixtureBody>> {
    let token = flags.signed.as_deref()?;
    let used = ctx
        .shared
        .counters
        .bump(&counter_key(&format!("sig:{token}"), flags, ctx.path));
    let ttl = flags.ttl_requests.unwrap_or(DEFAULT_SIGNED_TTL);
    (used >= ttl).then(|| error_response(StatusCode::FORBIDDEN, "signed link expired"))
}

fn forced_status(flags: &Flags, ctx: &Ctx<'_>) -> Option<Response<FixtureBody>> {
    let code = StatusCode::from_u16(flags.status?).ok()?;
    let index = ctx
        .shared
        .counters
        .bump(&counter_key("status", flags, ctx.path));
    if flags.fail_first.is_some_and(|first| index >= first) {
        return None;
    }
    let mut resp = if code == StatusCode::UNAUTHORIZED {
        unauthorized()
    } else if code == StatusCode::RANGE_NOT_SATISFIABLE {
        response::unsatisfiable(ctx.size)
    } else {
        error_response(code, "forced status")
    };
    if let Some(value) = &flags.retry_after {
        set(&mut resp, header::RETRY_AFTER, value);
    }
    Some(resp)
}

/// The query with the named parameters removed and `extra` (a raw `name=value`) appended.
fn rewrite_query(query: &str, drop: &[&str], extra: Option<&str>) -> String {
    let kept = query
        .split('&')
        .filter(|seg| !seg.is_empty())
        .filter(|seg| !drop.contains(&seg.split('=').next().unwrap_or("")));
    kept.chain(extra).collect::<Vec<_>>().join("&")
}

fn with_query(base: String, query: &str) -> String {
    if query.is_empty() {
        base
    } else {
        format!("{base}?{query}")
    }
}

fn redirect(flags: &Flags, ctx: &Ctx<'_>) -> Option<Response<FixtureBody>> {
    let location = redirect_location(flags, flags.redirect?, ctx)?;
    let mut resp = error_response(StatusCode::FOUND, "redirect");
    set(&mut resp, header::LOCATION, &location);
    Some(resp)
}

fn redirect_location(flags: &Flags, mode: RedirectMode, ctx: &Ctx<'_>) -> Option<String> {
    let port = ctx.shared.port;
    let stripped = rewrite_query(ctx.query, &["redirect"], None);
    let absolute = |host: &str| with_query(format!("http://{host}:{port}{}", ctx.path), &stripped);
    Some(match mode {
        RedirectMode::Cross => absolute(CROSS_HOST),
        RedirectMode::Private => absolute(PRIVATE_HOST),
        RedirectMode::Downgrade => absolute(LOOPBACK_HOST),
        RedirectMode::Loop => with_query(ctx.path.to_owned(), ctx.query),
        RedirectMode::Chain(0) => return None,
        RedirectMode::Chain(n) => {
            let next = format!("redirect=chain:{}", n - 1);
            with_query(
                ctx.path.to_owned(),
                &rewrite_query(ctx.query, &["redirect"], Some(&next)),
            )
        }
        RedirectMode::SignedExpiring => {
            let issued = ctx
                .shared
                .counters
                .bump(&counter_key("sig-issue", flags, ctx.path));
            let token = format!("signed=s{issued}");
            let query = rewrite_query(ctx.query, &["redirect", "signed"], Some(&token));
            with_query(ctx.path.to_owned(), &query)
        }
    })
}

/// The per-request values that depend on server state.
pub struct Resolved {
    /// The `Range` header, unless `range_200_after` says to ignore it now.
    pub range: Option<String>,
    /// The `If-Range` header.
    pub if_range: Option<String>,
    /// True once an `etag=mutable` resource has changed.
    pub mutated: bool,
}

impl Resolved {
    /// Evaluates the stateful flags (this counts the request) for one content request.
    #[must_use]
    pub fn new(
        flags: &Flags,
        shared: &Shared,
        path: &str,
        range: Option<String>,
        if_range: Option<String>,
    ) -> Self {
        let mutated = match (flags.etag, flags.mutate_after) {
            (EtagMode::Mutable, Some(after)) => {
                shared.counters.bump(&counter_key("mutate", flags, path)) >= after
            }
            _ => false,
        };
        let range = match (range, flags.range_200_after) {
            (Some(_), Some(after))
                if shared.counters.bump(&counter_key("range", flags, path)) >= after =>
            {
                None
            }
            (range, _) => range,
        };
        Self {
            range,
            if_range,
            mutated,
        }
    }

    /// The response target for these values.
    #[must_use]
    pub fn target(&self, is_head: bool, seed: u64, size: u64) -> Target<'_> {
        Target {
            is_head,
            seed,
            size,
            range: self.range.as_deref(),
            if_range: self.if_range.as_deref(),
            mutated: self.mutated,
        }
    }
}
