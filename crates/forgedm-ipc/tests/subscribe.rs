//! Per-connection subscription plumbing: bounded queue, ordering and lag handling.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code fails by panicking

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{RunningServer, TestHandler, handshake_frame, read_message, test_peer, write_all};
use forgedm_ipc::{
    Client, Connection, EventSink, OUTBOUND_QUEUE, SinkError, SubscriptionItem, Timeouts,
    event_channel, serve_connection,
};
use forgedm_protocol::{
    ClientKind, DownloadRemovedData, Empty, Event, EventBody, IDLE_TIMEOUT, RequestBody,
    ResultBody, ResyncReason, ResyncRequiredData, ServerMessage, SubscribeRequest,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn event(seq: u64) -> Event {
    Event {
        seq,
        body: EventBody::DownloadRemoved(DownloadRemovedData { id: Uuid::nil() }),
    }
}

fn subscribe_request() -> RequestBody {
    RequestBody::Subscribe(SubscribeRequest {
        agent_instance_id: Uuid::nil(),
        after_seq: 0,
    })
}

async fn next_event<S>(client: &mut Client<S>) -> Event
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), client.next_event())
        .await
        .expect("event arrives in time")
        .expect("event")
}

#[tokio::test]
async fn subscribe_seq_ordered_and_resync_on_lag() {
    // Phase 1: the queue holds exactly OUTBOUND_QUEUE events, then reports lag.
    let (mut sink, mut receiver) = event_channel();
    for seq in 1..=u64::try_from(OUTBOUND_QUEUE).expect("queue size") {
        sink.try_send(event(seq)).expect("fits in the queue");
    }
    assert_eq!(sink.try_send(event(9_999)), Err(SinkError::Lagged));
    assert_eq!(
        sink.try_send(event(10_000)),
        Err(SinkError::Closed),
        "dropped after lag"
    );
    for expected in 1..=u64::try_from(OUTBOUND_QUEUE).expect("queue size") {
        match receiver.recv().await {
            SubscriptionItem::Event(e) => assert_eq!(e.seq, expected),
            other => panic!("expected event {expected}, got {other:?}"),
        }
    }
    match receiver.recv().await {
        SubscriptionItem::Resync(e) => assert_eq!(
            e.body,
            EventBody::ResyncRequired(ResyncRequiredData {
                reason: ResyncReason::Lagged
            })
        ),
        other => panic!("expected resync, got {other:?}"),
    }

    // Phase 2: a live connection delivers events in order while the reader keeps up.
    let server = RunningServer::start("subscribe");
    let mut client = server.client().await;
    client
        .handshake(ClientKind::Desktop, "sub", "0.0.0")
        .await
        .expect("handshake");
    client.call(subscribe_request()).await.expect("subscribe");
    let mut live = server
        .handler
        .sinks
        .lock()
        .expect("lock")
        .pop()
        .expect("sink");
    for batch in 0..20u64 {
        for seq in (batch * 50 + 1)..=(batch * 50 + 50) {
            live.try_send(event(seq)).expect("reader keeps up");
        }
        for expected in (batch * 50 + 1)..=(batch * 50 + 50) {
            assert_eq!(next_event(&mut client).await.seq, expected);
        }
    }

    // Phase 3: overflow on a live connection ends in ResyncRequired and a dropped sink.
    lag_a_live_sink(&server, &mut client).await;
    server.stop().await.expect("clean stop");
}

/// Pushes more than the queue holds without yielding, so the connection cannot drain it.
async fn lag_a_live_sink(server: &RunningServer, client: &mut Client) {
    client
        .call(RequestBody::Unsubscribe(Empty {}))
        .await
        .expect("unsubscribe");
    client.call(subscribe_request()).await.expect("resubscribe");
    let mut sink: EventSink = server
        .handler
        .sinks
        .lock()
        .expect("lock")
        .pop()
        .expect("sink");
    let total = u64::try_from(OUTBOUND_QUEUE).expect("queue size") + 44;
    let mut lagged_at = None;
    for seq in 1..=total {
        if sink.try_send(event(seq)).is_err() {
            lagged_at = Some(seq);
            break;
        }
    }
    let queued = lagged_at.expect("overflow must be reported") - 1;
    assert_eq!(queued, u64::try_from(OUTBOUND_QUEUE).expect("queue size"));
    for expected in 1..=queued {
        assert_eq!(
            next_event(client).await.seq,
            expected,
            "queued events stay ordered"
        );
    }
    let resync = next_event(client).await;
    assert_eq!(
        resync.body,
        EventBody::ResyncRequired(ResyncRequiredData {
            reason: ResyncReason::Lagged
        })
    );
    assert_eq!(sink.try_send(event(total + 1)), Err(SinkError::Closed));
    // The connection itself survives; only the subscription is gone.
    let pong = client
        .call(RequestBody::Ping(Empty {}))
        .await
        .expect("ping after resync");
    assert!(matches!(pong, ResultBody::Ping(_)));
}

#[tokio::test(start_paused = true)]
async fn subscribed_connection_is_not_idle_closed() {
    let handler = Arc::new(TestHandler::default());
    let (client_half, server_half) = tokio::io::duplex(65_536);
    let h = handler.clone();
    let server = tokio::spawn(async move {
        serve_connection(
            server_half,
            Connection::new(1, test_peer()),
            h.as_ref(),
            &Timeouts::default(),
            &CancellationToken::new(),
        )
        .await
    });
    let mut client = Client::from_verified_stream(client_half, Duration::from_secs(10));
    client
        .handshake(ClientKind::Cli, "idle", "0.0.0")
        .await
        .expect("handshake");
    client.call(subscribe_request()).await.expect("subscribe");

    // Two idle timeouts pass; the subscribed connection must still answer.
    tokio::time::sleep(IDLE_TIMEOUT * 2).await;
    assert!(
        !server.is_finished(),
        "subscribed connection was closed as idle"
    );
    let mut sink = handler.sinks.lock().expect("lock").pop().expect("sink");
    sink.try_send(event(1)).expect("send");
    assert_eq!(next_event(&mut client).await.seq, 1);

    // Unsubscribing makes it idle again.
    client
        .call(RequestBody::Unsubscribe(Empty {}))
        .await
        .expect("unsubscribe");
    assert_eq!(
        server.await.expect("server task"),
        forgedm_ipc::CloseReason::IdleTimeout
    );
}

#[tokio::test]
async fn events_replayed_by_the_handler_follow_the_subscribe_response() {
    let server = RunningServer::start("order");
    let mut raw = common::raw_connect(&server.name).await;
    write_all(&mut raw, &handshake_frame(1)).await;
    read_message(&mut raw).await.expect("handshake reply");

    // The fake handler replays one event (seq 8) inside the Subscribe request itself,
    // like the hub does in its actor turn. It must still arrive after the response.
    let subscribe = forgedm_protocol::Request::new(
        2,
        RequestBody::Subscribe(SubscribeRequest {
            agent_instance_id: Uuid::nil(),
            after_seq: common::REPLAY_AFTER_SEQ,
        }),
    );
    write_all(&mut raw, &subscribe.to_frame().expect("frame")).await;

    assert!(matches!(
        read_message(&mut raw).await,
        Some(ServerMessage::Response(
            forgedm_protocol::Response::Success { request_id: 2, .. }
        ))
    ));
    assert!(matches!(read_message(&mut raw).await, Some(ServerMessage::Event(e)) if e.seq == 8));
    server.stop().await.expect("clean stop");
}
