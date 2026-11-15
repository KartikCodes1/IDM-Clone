//! Property tests: request roundtrip and decoder robustness.

use forgedm_protocol::{
    ClientKind, DownloadIdRequest, DownloadState, Empty, EnqueueUrlRequest, FrameDecoder,
    HandshakeRequest, LimitScope, ListDownloadsRequest, MAX_FRAME, MAX_SAFE_INTEGER,
    PauseAllRequest, PauseRequest, Priority, RemoveRequest, Request, RequestBody, RetryRequest,
    SubscribeRequest, UpdateLimitsRequest, UpdatePriorityRequest, encode_frame, parse_request,
};
use proptest::prelude::*;
use uuid::Uuid;

fn uuid() -> impl Strategy<Value = Uuid> {
    any::<u128>().prop_map(Uuid::from_u128)
}

fn priority() -> impl Strategy<Value = Priority> {
    prop_oneof![
        Just(Priority::High),
        Just(Priority::Normal),
        Just(Priority::Low)
    ]
}

fn state() -> impl Strategy<Value = DownloadState> {
    (0..DownloadState::ALL.len()).prop_map(|i| DownloadState::ALL[i])
}

fn text() -> impl Strategy<Value = String> {
    // Includes quotes, backslashes, control characters, BMP and astral characters.
    prop::collection::vec(any::<char>(), 0..40).prop_map(|c| c.into_iter().collect())
}

fn enqueue() -> impl Strategy<Value = EnqueueUrlRequest> {
    (
        (
            text(),
            prop::option::of(text()),
            prop::option::of(text()),
            prop::option::of(text()),
        ),
        (
            prop::option::of(1u32..=16),
            prop::option::of(prop::option::of(1024u64..=MAX_SAFE_INTEGER)),
            prop::option::of(priority()),
            any::<bool>(),
        ),
    )
        .prop_map(
            |((url, target_dir, file_name, sha), (conns, rate, priority, paused))| {
                EnqueueUrlRequest {
                    url,
                    target_dir,
                    file_name,
                    expected_sha256: sha,
                    max_connections: conns,
                    rate_limit_bps: rate,
                    priority,
                    start_paused: paused,
                }
            },
        )
}

fn limits() -> impl Strategy<Value = UpdateLimitsRequest> {
    (
        prop_oneof![
            Just(LimitScope::Global),
            uuid().prop_map(|download_id| LimitScope::Download { download_id })
        ],
        prop::option::of(1u32..=16),
        prop::option::of(prop::option::of(1024u64..=MAX_SAFE_INTEGER)),
        prop::option::of(1u32..=10),
    )
        .prop_map(
            |(scope, max_connections, rate_limit_bps, max_active_downloads)| UpdateLimitsRequest {
                scope,
                max_connections,
                rate_limit_bps,
                max_active_downloads,
            },
        )
}

fn simple_body() -> impl Strategy<Value = RequestBody> {
    prop_oneof![
        Just(RequestBody::Ping(Empty {})),
        Just(RequestBody::GetAgentStatus(Empty {})),
        Just(RequestBody::GetSnapshot(Empty {})),
        Just(RequestBody::Unsubscribe(Empty {})),
        Just(RequestBody::ShutdownAgentSafely(Empty {})),
        Just(RequestBody::ResumeAll(Empty {})),
        uuid().prop_map(|download_id| RequestBody::GetDownload(DownloadIdRequest { download_id })),
        uuid().prop_map(|download_id| RequestBody::Resume(DownloadIdRequest { download_id })),
        uuid().prop_map(|download_id| RequestBody::Cancel(DownloadIdRequest { download_id })),
        (uuid(), any::<bool>()).prop_map(|(download_id, discard_progress)| {
            RequestBody::Pause(PauseRequest {
                download_id,
                discard_progress,
            })
        }),
        (uuid(), any::<bool>()).prop_map(|(download_id, restart_from_zero)| {
            RequestBody::Retry(RetryRequest {
                download_id,
                restart_from_zero,
            })
        }),
        (uuid(), any::<bool>()).prop_map(|(download_id, delete_partial_file)| {
            RequestBody::Remove(RemoveRequest {
                download_id,
                delete_partial_file,
            })
        }),
        any::<bool>().prop_map(|discard_progress| RequestBody::PauseAll(PauseAllRequest {
            discard_progress
        })),
        (uuid(), priority()).prop_map(|(download_id, priority)| {
            RequestBody::UpdatePriority(UpdatePriorityRequest {
                download_id,
                priority,
            })
        }),
        (uuid(), 0..=MAX_SAFE_INTEGER).prop_map(|(agent_instance_id, after_seq)| {
            RequestBody::Subscribe(SubscribeRequest {
                agent_instance_id,
                after_seq,
            })
        }),
    ]
}

fn complex_body() -> impl Strategy<Value = RequestBody> {
    prop_oneof![
        enqueue().prop_map(RequestBody::EnqueueUrl),
        limits().prop_map(RequestBody::UpdateLimits),
        (
            prop::option::of(prop::collection::vec(state(), 0..5)),
            1u32..=500,
            any::<u32>()
        )
            .prop_map(|(states, limit, offset)| {
                RequestBody::ListDownloads(ListDownloadsRequest {
                    states,
                    limit,
                    offset,
                })
            }),
        (
            prop_oneof![Just(ClientKind::Cli), Just(ClientKind::Desktop)],
            text(),
            text(),
            any::<u32>(),
            any::<u32>()
        )
            .prop_map(
                |(client_kind, client_name, client_version, min_protocol, max_protocol)| {
                    RequestBody::Handshake(HandshakeRequest {
                        client_kind,
                        client_name,
                        client_version,
                        min_protocol,
                        max_protocol,
                    })
                }
            ),
    ]
}

fn request() -> impl Strategy<Value = Request> {
    (any::<u32>(), prop_oneof![simple_body(), complex_body()])
        .prop_map(|(request_id, body)| Request { request_id, body })
}

proptest! {
    #[test]
    fn proptest_request_roundtrip(req in request()) {
        let json = req.to_json().unwrap();
        let parsed = parse_request(&json).unwrap();
        prop_assert_eq!(&parsed, &req);

        // And through the frame codec at an arbitrary split point.
        let wire = req.to_frame().unwrap();
        let mut decoder = FrameDecoder::new();
        let split = wire.len() / 2;
        decoder.feed(&wire[..split]).unwrap();
        decoder.feed(&wire[split..]).unwrap();
        let frame = decoder.next_frame().unwrap();
        prop_assert_eq!(parse_request(&frame).unwrap(), req);
        prop_assert!(decoder.next_frame().is_none());
    }

    #[test]
    fn proptest_decoder_never_panics_on_arbitrary_chunks(
        chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 0..16),
        lead_with_valid in any::<bool>(),
    ) {
        let mut decoder = FrameDecoder::new();
        if lead_with_valid {
            let _ = decoder.feed(&encode_frame(b"{}").unwrap());
        }
        for chunk in &chunks {
            let result = decoder.feed(chunk);
            // The decoder never buffers more than one maximum-size body.
            prop_assert!(decoder.buffered_bytes() <= MAX_FRAME as usize);
            if result.is_err() {
                prop_assert_eq!(decoder.buffered_bytes(), 0);
            }
            while let Some(frame) = decoder.next_frame() {
                prop_assert!(!frame.is_empty() && frame.len() <= MAX_FRAME as usize);
                let _ = parse_request(&frame); // must not panic
            }
        }
        let _ = decoder.finish();
    }

    #[test]
    fn arbitrary_bytes_never_panic_request_parser(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = parse_request(&bytes);
    }
}
