//! `validate()` enforces the Appendix A.3 limits on every request.

use forgedm_protocol::{
    ClientKind, DownloadState, EnqueueUrlRequest, ErrorCode, HandshakeRequest, LimitScope,
    ListDownloadsRequest, MAX_SAFE_INTEGER, Request, RequestBody, SubscribeRequest,
    UpdateLimitsRequest,
};
use uuid::Uuid;

fn enqueue(edit: impl FnOnce(&mut EnqueueUrlRequest)) -> Request {
    let mut payload = EnqueueUrlRequest::new("https://example.com/a.bin");
    edit(&mut payload);
    Request {
        request_id: 1,
        body: RequestBody::EnqueueUrl(payload),
    }
}

fn limits(scope: LimitScope, edit: impl FnOnce(&mut UpdateLimitsRequest)) -> Request {
    let mut payload = UpdateLimitsRequest::new(scope);
    edit(&mut payload);
    Request {
        request_id: 1,
        body: RequestBody::UpdateLimits(payload),
    }
}

fn code(request: &Request) -> ErrorCode {
    // A request that unexpectedly validates yields `Unknown`, failing the comparison.
    match request.validate() {
        Err(err) => err.to_error_body().code,
        Ok(()) => ErrorCode::Unknown,
    }
}

fn list(limit: u32, states: Option<Vec<DownloadState>>) -> Request {
    Request {
        request_id: 1,
        body: RequestBody::ListDownloads(ListDownloadsRequest {
            states,
            limit,
            offset: 0,
        }),
    }
}

fn subscribe(after_seq: u64) -> Request {
    Request {
        request_id: 1,
        body: RequestBody::Subscribe(SubscribeRequest {
            agent_instance_id: Uuid::nil(),
            after_seq,
        }),
    }
}

fn handshake(name: String, version: String) -> Request {
    Request {
        request_id: 1,
        body: RequestBody::Handshake(HandshakeRequest {
            client_kind: ClientKind::Cli,
            client_name: name,
            client_version: version,
            min_protocol: 1,
            max_protocol: 1,
        }),
    }
}

#[test]
fn validate_rejects_each_limit() {
    enqueue_limits();
    target_dir_rules();
    numeric_limits();
    handshake_limits();
}

fn enqueue_limits() {
    assert!(enqueue(|_| {}).validate().is_ok());
    let url_at_limit = format!(
        "https://e.com/{}",
        "a".repeat(8192 - "https://e.com/".len())
    );
    assert_eq!(url_at_limit.len(), 8192);
    assert!(enqueue(|p| p.url = url_at_limit.clone()).validate().is_ok());
    assert!(
        enqueue(|p| p.file_name = Some("a".repeat(255)))
            .validate()
            .is_ok()
    );

    assert_eq!(
        code(&enqueue(|p| p.url = format!("{url_at_limit}a"))),
        ErrorCode::InvalidUrl
    );
    assert_eq!(
        code(&enqueue(|p| p.url = String::new())),
        ErrorCode::InvalidUrl
    );

    // 255 UTF-16 units; an astral char is two units.
    let too_long = Some("a".repeat(256));
    assert_eq!(
        code(&enqueue(|p| p.file_name = too_long.clone())),
        ErrorCode::InvalidFileName
    );
    let astral = Some(format!("{}{}", "a".repeat(254), "\u{1F600}"));
    assert_eq!(
        code(&enqueue(|p| p.file_name = astral.clone())),
        ErrorCode::InvalidFileName
    );
    assert_eq!(
        code(&enqueue(|p| p.file_name = Some(String::new()))),
        ErrorCode::InvalidFileName
    );

    for bad in ["abc", &"g".repeat(64), &"a".repeat(63), &"a".repeat(65)] {
        assert_eq!(
            code(&enqueue(|p| p.expected_sha256 = Some(bad.to_owned()))),
            ErrorCode::InvalidChecksum,
            "{bad}"
        );
    }
    let upper = "ABCDEF0123456789".repeat(4);
    let ok = enqueue(|p| p.expected_sha256 = Some(upper.clone()));
    assert!(ok.validate().is_ok());
    let RequestBody::EnqueueUrl(payload) = &ok.body else {
        unreachable!()
    };
    assert_eq!(payload.normalized_sha256(), Some(upper.to_lowercase()));
}

fn target_dir_rules() {
    for bad in ["relative\\dir", "C:dir", "\\rooted", "", "./x", "dir/x"] {
        assert_eq!(
            code(&enqueue(|p| p.target_dir = Some(bad.to_owned()))),
            ErrorCode::InvalidDestination,
            "{bad:?}"
        );
    }
    for good in ["C:\\x", "d:/x", "\\\\server\\share\\x", "\\\\?\\C:\\x"] {
        assert!(
            enqueue(|p| p.target_dir = Some(good.to_owned()))
                .validate()
                .is_ok(),
            "{good}"
        );
    }
}

fn numeric_limits() {
    assert!(enqueue(|p| p.max_connections = Some(16)).validate().is_ok());
    assert!(
        enqueue(|p| p.rate_limit_bps = Some(Some(1024)))
            .validate()
            .is_ok()
    );
    assert!(
        enqueue(|p| p.rate_limit_bps = Some(None))
            .validate()
            .is_ok()
    );
    for bad in [0, 17] {
        assert_eq!(
            code(&enqueue(|p| p.max_connections = Some(bad))),
            ErrorCode::InvalidPayload
        );
    }
    assert_eq!(
        code(&enqueue(|p| p.rate_limit_bps = Some(Some(1023)))),
        ErrorCode::InvalidPayload
    );
    assert_eq!(
        code(&enqueue(
            |p| p.rate_limit_bps = Some(Some(MAX_SAFE_INTEGER + 1))
        )),
        ErrorCode::InvalidPayload
    );
    assert!(
        enqueue(|p| p.rate_limit_bps = Some(Some(MAX_SAFE_INTEGER)))
            .validate()
            .is_ok()
    );

    assert!(list(1, None).validate().is_ok());
    assert!(list(500, None).validate().is_ok());
    assert_eq!(code(&list(0, None)), ErrorCode::InvalidPayload);
    assert_eq!(code(&list(501, None)), ErrorCode::InvalidPayload);
    assert_eq!(
        code(&list(10, Some(vec![DownloadState::Unknown]))),
        ErrorCode::InvalidPayload
    );

    assert!(subscribe(MAX_SAFE_INTEGER).validate().is_ok());
    assert_eq!(
        code(&subscribe(MAX_SAFE_INTEGER + 1)),
        ErrorCode::InvalidPayload
    );

    assert!(
        limits(LimitScope::Global, |p| p.max_active_downloads = Some(10))
            .validate()
            .is_ok()
    );
    for bad in [0, 11] {
        assert_eq!(
            code(&limits(LimitScope::Global, |p| p.max_active_downloads = Some(bad))),
            ErrorCode::InvalidPayload
        );
    }
    let per_download = LimitScope::Download {
        download_id: Uuid::nil(),
    };
    assert_eq!(
        code(&limits(per_download.clone(), |p| p.max_active_downloads = Some(3))),
        ErrorCode::InvalidPayload,
        "max_active_downloads is global only"
    );
    assert_eq!(
        code(&limits(per_download, |p| p.max_connections = Some(17))),
        ErrorCode::InvalidPayload
    );
    assert_eq!(
        code(&limits(LimitScope::Global, |p| p.rate_limit_bps = Some(Some(10)))),
        ErrorCode::InvalidPayload
    );
}

fn handshake_limits() {
    assert!(handshake("n".repeat(64), "v".repeat(32)).validate().is_ok());
    assert!(
        handshake("\u{e9}".repeat(64), "v".into())
            .validate()
            .is_ok()
    );
    assert_eq!(
        code(&handshake("n".repeat(65), "v".into())),
        ErrorCode::InvalidPayload
    );
    assert_eq!(
        code(&handshake("n".into(), "v".repeat(33))),
        ErrorCode::InvalidPayload
    );
}
