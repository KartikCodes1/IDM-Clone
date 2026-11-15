//! Server and client transport behaviour over real named pipes, plus timing behaviour over
//! in-memory streams with paused time.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code fails by panicking

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::{
    RunningServer, TestHandler, assert_closed, client_config, handshake_frame, handshake_request,
    list_frame, ping_frame, raw_connect, read_error, read_frame, read_message, test_peer,
    unique_pipe_name, write_all,
};
use forgedm_ipc::{
    CLIENT_REQUEST_TIMEOUT, Client, CloseReason, Connection, IpcError, Server, ServerOptions,
    Timeouts, serve_connection,
};
use forgedm_platform::pipe::{PipeError, PipeSecurity, create_pipe_instance};
use forgedm_protocol::{
    ClientKind, Empty, ErrorCode, FRAME_BODY_TIMEOUT, HANDSHAKE_TIMEOUT, IDLE_TIMEOUT, MAX_FRAME,
    RequestBody, Response, ResultBody, ServerMessage, encode_frame,
};
use tokio::io::{AsyncReadExt, DuplexStream};
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const ERROR_PIPE_BUSY: i32 = 231;

/// Serves one in-memory connection with the default timeouts.
fn spawn_conn(handler: Arc<TestHandler>) -> (DuplexStream, JoinHandle<CloseReason>) {
    let (client, server) = tokio::io::duplex(65_536);
    let task = tokio::spawn(async move {
        serve_connection(
            server,
            Connection::new(1, test_peer()),
            handler.as_ref(),
            &Timeouts::default(),
            &CancellationToken::new(),
        )
        .await
    });
    (client, task)
}

async fn handshake_over(stream: &mut DuplexStream) {
    write_all(stream, &handshake_frame(1)).await;
    match read_message(stream).await {
        Some(ServerMessage::Response(Response::Success { request_id: 1, .. })) => {}
        other => panic!("handshake failed: {other:?}"),
    }
}

#[tokio::test]
async fn handshake_roundtrip() {
    let server = RunningServer::start("handshake");
    let mut client = server.client().await;

    let result = client
        .handshake(ClientKind::Cli, "test", "0.0.0")
        .await
        .expect("handshake");
    assert_eq!(result.protocol_version, 1);
    assert_eq!(result.agent_pid, std::process::id());

    let ping = client
        .call(RequestBody::Ping(Empty {}))
        .await
        .expect("ping");
    assert!(matches!(ping, ResultBody::Ping(_)));

    let err = client
        .call(RequestBody::GetSnapshot(Empty {}))
        .await
        .expect_err("not implemented");
    assert!(matches!(err, IpcError::Server(e) if e.code == ErrorCode::NotImplemented));
    server.stop().await.expect("clean stop");
}

#[tokio::test]
async fn non_handshake_first_frame_closes() {
    let server = RunningServer::start("nohs");
    let mut raw = raw_connect(&server.name).await;

    write_all(&mut raw, &ping_frame(1)).await;

    assert_eq!(
        read_error(&mut raw).await.code,
        ErrorCode::HandshakeRequired
    );
    assert_closed(&mut raw).await;
    assert_eq!(server.handler.handled.load(Ordering::SeqCst), 0);
    server.stop().await.expect("clean stop");
}

#[tokio::test(start_paused = true)]
async fn handshake_timeout_closes_connection() {
    let (mut client, task) = spawn_conn(Arc::new(TestHandler::default()));
    let start = Instant::now();

    assert_eq!(task.await.expect("task"), CloseReason::HandshakeTimeout);

    let elapsed = start.elapsed();
    assert!(elapsed >= HANDSHAKE_TIMEOUT, "closed early: {elapsed:?}");
    assert!(elapsed < HANDSHAKE_TIMEOUT + Duration::from_secs(1));
    assert_eq!(
        read_error(&mut client).await.code,
        ErrorCode::HandshakeRequired
    );
    assert_closed(&mut client).await;
}

#[tokio::test]
async fn handshake_timeout_over_real_pipe_closes_connection() {
    let options = ServerOptions {
        timeouts: Timeouts {
            handshake: Duration::from_millis(300),
            ..Timeouts::default()
        },
    };
    let server = RunningServer::start_with("hstimeout", options);
    let mut raw = raw_connect(&server.name).await;

    assert_eq!(
        read_error(&mut raw).await.code,
        ErrorCode::HandshakeRequired
    );
    assert_closed(&mut raw).await;
    server.stop().await.expect("clean stop");
}

#[tokio::test]
async fn oversized_frame_closes_connection() {
    let server = RunningServer::start("oversized");
    let header = (MAX_FRAME + 1).to_le_bytes();

    let mut before_handshake = raw_connect(&server.name).await;
    write_all(&mut before_handshake, &header).await;
    assert_eq!(
        read_error(&mut before_handshake).await.code,
        ErrorCode::FrameTooLarge
    );
    assert_closed(&mut before_handshake).await;

    let mut after_handshake = raw_connect(&server.name).await;
    write_all(&mut after_handshake, &handshake_frame(1)).await;
    assert!(read_frame(&mut after_handshake).await.is_some());
    write_all(&mut after_handshake, &header).await;
    assert_eq!(
        read_error(&mut after_handshake).await.code,
        ErrorCode::FrameTooLarge
    );
    assert_closed(&mut after_handshake).await;
    server.stop().await.expect("clean stop");
}

#[tokio::test]
async fn garbage_client_then_next_client_served() {
    let server = RunningServer::start("garbage");

    let mut huge_length = raw_connect(&server.name).await;
    write_all(&mut huge_length, &[0xFF; 64]).await;
    assert_eq!(
        read_error(&mut huge_length).await.code,
        ErrorCode::FrameTooLarge
    );
    assert_closed(&mut huge_length).await;

    let mut not_json = raw_connect(&server.name).await;
    write_all(
        &mut not_json,
        &encode_frame(b"hello, not json").expect("frame"),
    )
    .await;
    assert_eq!(
        read_error(&mut not_json).await.code,
        ErrorCode::MalformedFrame
    );
    assert_closed(&mut not_json).await;

    let mut zero_length = raw_connect(&server.name).await;
    write_all(&mut zero_length, &[0, 0, 0, 0, 1, 2, 3]).await;
    assert_eq!(
        read_error(&mut zero_length).await.code,
        ErrorCode::MalformedFrame
    );
    assert_closed(&mut zero_length).await;

    let mut client = server.client().await;
    client
        .handshake(ClientKind::Cli, "next", "0.0.0")
        .await
        .expect("next client is served");
    server.stop().await.expect("clean stop");
}

#[tokio::test(start_paused = true)]
async fn partial_frame_times_out() {
    let (mut client, task) = spawn_conn(Arc::new(TestHandler::default()));
    handshake_over(&mut client).await;
    let start = Instant::now();

    // A header promising 100 bytes, then only 10 of them.
    let mut partial = 100u32.to_le_bytes().to_vec();
    partial.extend_from_slice(&[b'{'; 10]);
    write_all(&mut client, &partial).await;

    assert_eq!(task.await.expect("task"), CloseReason::FrameTimeout);
    let elapsed = start.elapsed();
    assert!(elapsed >= FRAME_BODY_TIMEOUT, "closed early: {elapsed:?}");
    assert!(elapsed < FRAME_BODY_TIMEOUT + Duration::from_secs(1));
    assert_eq!(
        read_error(&mut client).await.code,
        ErrorCode::MalformedFrame
    );
    assert_closed(&mut client).await;
}

#[tokio::test(start_paused = true)]
async fn idle_connection_closed_after_timeout() {
    let (mut client, task) = spawn_conn(Arc::new(TestHandler::default()));
    handshake_over(&mut client).await;
    let start = Instant::now();

    assert_eq!(task.await.expect("task"), CloseReason::IdleTimeout);
    let elapsed = start.elapsed();
    assert!(elapsed >= IDLE_TIMEOUT, "closed early: {elapsed:?}");
    assert!(elapsed < IDLE_TIMEOUT + Duration::from_secs(1));
    assert_closed(&mut client).await;
}

#[tokio::test]
async fn seventeenth_client_waits_then_served() {
    let server = RunningServer::start("capacity");
    let mut held = Vec::new();
    for id in 1..=16 {
        let mut raw = raw_connect(&server.name).await;
        write_all(&mut raw, &handshake_frame(id)).await;
        assert!(read_frame(&mut raw).await.is_some(), "client {id} served");
        held.push(raw);
    }

    // The 17th connects to the listening instance but is not served while 16 are active.
    let mut seventeenth = raw_connect(&server.name).await;
    write_all(&mut seventeenth, &handshake_frame(17)).await;
    let early =
        tokio::time::timeout(Duration::from_millis(500), read_frame(&mut seventeenth)).await;
    assert!(early.is_err(), "17th client must wait for a free permit");

    // The 18th finds every instance busy.
    let eighteenth = ClientOptions::new().open(&server.name);
    assert_eq!(
        eighteenth.err().and_then(|e| e.raw_os_error()),
        Some(ERROR_PIPE_BUSY)
    );

    drop(held.remove(0));
    let served = tokio::time::timeout(Duration::from_secs(5), read_message(&mut seventeenth))
        .await
        .expect("17th client served once a permit frees");
    assert!(
        matches!(
            served,
            Some(ServerMessage::Response(Response::Success {
                request_id: 17,
                ..
            }))
        ),
        "17th client got {served:?}"
    );
    assert!(!server.task.is_finished(), "server keeps serving");
    server.stop().await.expect("clean stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipe_name_never_vanishes_under_churn() {
    let server = RunningServer::start("churn");
    let stop = Arc::new(AtomicBool::new(false));
    let probe = spawn_probe(server.name.clone(), stop.clone());

    // Sixteen workers all retry on a 50 ms cadence, so they can starve each other for the
    // whole busy window; `Busy` is therefore a legitimate outcome here and is only counted.
    let mut clients = Vec::new();
    for worker in 0..16u64 {
        let name = server.name.clone();
        clients.push(tokio::spawn(
            async move { churn_worker(&name, worker).await },
        ));
    }
    let mut served = 0u32;
    for task in clients {
        served += task.await.expect("churn client");
    }
    stop.store(true, Ordering::SeqCst);
    let (attempts, unexpected) = probe.join().expect("probe thread");

    assert!(served >= 100, "too few clients were served: {served}");
    assert!(attempts > 0, "probe never ran");
    assert!(
        unexpected.is_empty(),
        "name vanished or odd error: {unexpected:?}"
    );
    server.stop().await.expect("clean stop");
}

/// Connects and disconnects 25 times, mixing completed handshakes with abrupt closes.
/// Returns how many handshakes completed.
async fn churn_worker(name: &str, worker: u64) -> u32 {
    let mut served = 0;
    for round in 0..25u64 {
        tokio::time::sleep(Duration::from_millis((worker * 7 + round * 3) % 11)).await;
        if round % 3 == 0 {
            // Open and drop without a handshake; a busy pipe is fine here.
            drop(ClientOptions::new().open(name));
            continue;
        }
        match Client::connect(client_config(name)).await {
            Ok(mut client) => {
                client
                    .handshake(ClientKind::Cli, "churn", "0.0.0")
                    .await
                    .unwrap_or_else(|e| panic!("worker {worker} round {round}: {e}"));
                served += 1;
            }
            Err(IpcError::Busy) => {}
            Err(other) => panic!("worker {worker} round {round}: {other}"),
        }
    }
    served
}

/// A thread that keeps trying to claim the pipe name with `FIRST_PIPE_INSTANCE`.
fn spawn_probe(name: String, stop: Arc<AtomicBool>) -> std::thread::JoinHandle<(u64, Vec<String>)> {
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let _enter = runtime.enter();
        let security = PipeSecurity::for_current_user().expect("security");
        let (mut attempts, mut unexpected) = (0u64, Vec::new());
        while !stop.load(Ordering::SeqCst) {
            match create_pipe_instance(&name, true, &security) {
                Err(PipeError::NameInUse) => {}
                Ok(_) => unexpected.push("probe created the first instance".to_owned()),
                Err(other) => unexpected.push(format!("{other}")),
            }
            attempts += 1;
        }
        (attempts, unexpected)
    })
}

#[tokio::test(start_paused = true)]
async fn pipe_busy_retried_then_errors() {
    let name = unique_pipe_name("busy");
    let security = PipeSecurity::for_current_user().expect("security");
    let instance = create_pipe_instance(&name, true, &security).expect("instance");
    let _occupant = raw_connect(&name).await;
    instance.connect().await.expect("connect");
    // The only instance is connected, so a new client sees ERROR_PIPE_BUSY.

    let start = Instant::now();
    let result = Client::connect(client_config(&name)).await;

    assert!(
        matches!(result, Err(IpcError::Busy)),
        "got {:?}",
        result.err()
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(5),
        "gave up early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "kept retrying: {elapsed:?}"
    );
}

#[tokio::test]
async fn absent_pipe_is_agent_not_running() {
    let result = Client::connect(client_config(&unique_pipe_name("absent"))).await;

    assert!(
        matches!(result, Err(IpcError::NotRunning)),
        "got {:?}",
        result.err()
    );
}

#[tokio::test]
async fn version_mismatch_structured_error() {
    let server = RunningServer::start("version");

    // Handshake range that excludes major 1.
    let mut request = handshake_request(7);
    if let RequestBody::Handshake(p) = &mut request.body {
        p.min_protocol = 2;
        p.max_protocol = 3;
    }
    let mut by_range = raw_connect(&server.name).await;
    write_all(&mut by_range, &request.to_frame().expect("frame")).await;
    let error = read_error(&mut by_range).await;
    assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion);
    let details = error.details.expect("details");
    assert_eq!((details.server_min, details.server_max), (Some(1), Some(1)));
    assert_closed(&mut by_range).await;

    // Envelope protocol_version 2.
    let body = br#"{"protocol_version":2,"request_id":9,"action":"Ping","payload":{}}"#;
    let mut by_envelope = raw_connect(&server.name).await;
    write_all(&mut by_envelope, &encode_frame(body).expect("frame")).await;
    match read_message(&mut by_envelope).await {
        Some(ServerMessage::Response(Response::Failure {
            request_id: 9,
            error,
        })) => {
            assert_eq!(error.code, ErrorCode::UnsupportedProtocolVersion);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_closed(&mut by_envelope).await;
    assert_eq!(server.handler.handled.load(Ordering::SeqCst), 0);
    server.stop().await.expect("clean stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_clients_get_own_responses() {
    let server = RunningServer::start("concurrent");
    let mut tasks = Vec::new();
    for client_no in 0..16u32 {
        let name = server.name.clone();
        tasks.push(tokio::spawn(async move {
            let mut client = Client::connect(client_config(&name))
                .await
                .expect("connect");
            client
                .handshake(ClientKind::Desktop, "concurrent", "0.0.0")
                .await
                .expect("handshake");
            for request_no in 0..50u32 {
                let (limit, offset) = (client_no + 1, request_no % 3);
                let result = client
                    .call(common::list_request(0, limit, offset).body)
                    .await
                    .expect("call");
                let ResultBody::ListDownloads(page) = result else {
                    panic!("wrong result kind");
                };
                assert_eq!(
                    page.total,
                    limit * 1000 + offset,
                    "client {client_no} got a foreign answer"
                );
            }
        }));
    }
    for task in tasks {
        task.await.expect("client task");
    }
    server.stop().await.expect("clean stop");
}

#[tokio::test]
async fn squatted_name_server_start_fails_closed() {
    let name = unique_pipe_name("squat");
    let security = PipeSecurity::for_current_user().expect("security");
    let _squatter = create_pipe_instance(&name, true, &security).expect("squatter");

    let result = Server::bind(
        &name,
        Arc::new(TestHandler::default()),
        ServerOptions::default(),
    );

    assert!(
        matches!(result, Err(IpcError::NameInUse)),
        "must not serve a taken name"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replacement_creation_failure_is_fatal_and_stops_server() {
    let name = unique_pipe_name("fatal");
    let handler = Arc::new(TestHandler::default());
    let server = Server::bind(&name, handler, ServerOptions::default()).expect("bind");
    // Fill every remaining instance slot so the replacement cannot be created.
    let security = PipeSecurity::for_current_user().expect("security");
    let fillers: Vec<_> = (0..16)
        .map(|_| create_pipe_instance(&name, false, &security).expect("filler"))
        .collect();
    let task = tokio::spawn(server.serve(CancellationToken::new()));

    let _client = raw_connect(&name).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("server stops")
        .expect("task");

    assert!(
        matches!(outcome, Err(IpcError::Fatal(_))),
        "got {outcome:?}"
    );
    drop(fillers);
}

#[tokio::test]
async fn pipelined_requests_answered_in_order_one_at_a_time() {
    let server = RunningServer::start("pipelined");
    let mut raw = raw_connect(&server.name).await;
    write_all(&mut raw, &handshake_frame(1)).await;
    assert!(read_frame(&mut raw).await.is_some());

    // Five requests in a single write; the first is the slowest.
    let offsets = [40u32, 0, 25, 0, 10];
    let mut wire = Vec::new();
    for (i, offset) in offsets.iter().enumerate() {
        wire.extend(list_frame(10 + u32::try_from(i).expect("i"), 1, *offset));
    }
    write_all(&mut raw, &wire).await;

    for (i, offset) in offsets.iter().enumerate() {
        let id = 10 + u32::try_from(i).expect("i");
        let Some(ServerMessage::Response(Response::Success { request_id, result })) =
            read_message(&mut raw).await
        else {
            panic!("response {i} missing");
        };
        assert_eq!(request_id, id, "responses must arrive in request order");
        assert_eq!(result["total"], 1000 + offset);
    }
    assert_eq!(server.handler.max_in_flight.load(Ordering::SeqCst), 1);
    assert_eq!(
        *server.handler.started.lock().expect("lock"),
        [1, 10, 11, 12, 13, 14]
    );
    server.stop().await.expect("clean stop");
}

#[tokio::test(start_paused = true)]
async fn client_request_times_out_after_10s() {
    let (client_half, mut server_half) = tokio::io::duplex(65_536);
    let mut client = Client::from_verified_stream(client_half, CLIENT_REQUEST_TIMEOUT);
    let start = Instant::now();

    let result = client.call(RequestBody::Ping(Empty {})).await;

    assert!(
        matches!(result, Err(IpcError::Timeout)),
        "got {:?}",
        result.err()
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(10),
        "timed out early: {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(11));
    // The request really went out; the server just never answered.
    let mut header = [0u8; 4];
    server_half
        .read_exact(&mut header)
        .await
        .expect("request frame was sent");
    // A timed-out connection is not reused.
    let again = client.call(RequestBody::Ping(Empty {})).await;
    assert!(
        matches!(again, Err(IpcError::Closed)),
        "got {:?}",
        again.err()
    );
}
