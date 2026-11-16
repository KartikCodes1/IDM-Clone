//! Secret canary audit (PLAN S1.14, THREAT_MODEL Inv-11..Inv-13, Inv-19..Inv-21): a URL with
//! `?token=CANARY_Q` and `#CANARY_F`, whose server redirects to a `Location` carrying
//! `?sig=CANARY_R`, goes through the real agent (real DPAPI sealer), the real CLI and the
//! pipe. None of the three canaries may appear in the raw bytes of `forgedm.db`, its `-wal`
//! and `-shm`, in any log file (at `debug` level), in the `Zone.Identifier` stream, in the
//! CLI's stdout and stderr, or in the IPC messages a client receives.
//!
//! Documented residual: a secret embedded in the URL *path* is not recognised as a secret
//! and is kept (redaction removes userinfo, query and fragment only).
//!
//! The CLI binary must be built first (`cargo build -p forgedm-cli --bins`): this test fails,
//! it does not skip, when `forgedm.exe` is missing.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use forgedm_fixture::{content, verify};
use forgedm_ipc::Client;
use forgedm_protocol::{
    Action, DownloadIdRequest, DownloadState, Empty, EnqueueUrlRequest, RequestBody, Response,
    ResultBody,
};
use support::AgentProc;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const CANARIES: [&str; 3] = ["CANARY_Q", "CANARY_F", "CANARY_R"];
const WAIT: Duration = Duration::from_secs(60);
const SEED: u64 = 91;
const SIZE: u64 = 400_000;

/// Serves `/start` (a redirect whose `Location` carries `?sig=CANARY_R`), `/data` (the
/// content, ranges supported) and `/missing` (404). Counts the requests it sees.
async fn spawn_server() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = serve(stream).await;
            });
        }
    });
    (port, seen)
}

async fn serve(mut stream: TcpStream) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).await? == 0 || head.len() > 16 * 1024 {
            return Ok(());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, target) = (
        first.next().unwrap_or_default(),
        first.next().unwrap_or_default(),
    );
    let range = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(n, _)| n.eq_ignore_ascii_case("range"))
        .and_then(|(_, v)| v.trim().strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok())));
    let path = target.split('?').next().unwrap_or_default();
    let reply = match path {
        "/start" => b"HTTP/1.1 302 Found\r\nLocation: /data?sig=CANARY_R\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        "/data" => return serve_data(&mut stream, method, range).await,
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    };
    stream.write_all(&reply).await?;
    stream.shutdown().await
}

async fn serve_data(
    stream: &mut TcpStream,
    method: &str,
    range: Option<(u64, Option<u64>)>,
) -> std::io::Result<()> {
    let (status, first, last) = match range {
        Some((a, b)) if a < SIZE => (
            "206 Partial Content",
            a,
            b.unwrap_or(u64::MAX).min(SIZE - 1),
        ),
        _ => ("200 OK", 0, SIZE - 1),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"canary\"\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        last - first + 1
    );
    if status.starts_with("206") {
        head.push_str(&format!("Content-Range: bytes {first}-{last}/{SIZE}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    if method != "HEAD" {
        let mut body = vec![0u8; usize::try_from(last - first + 1).unwrap()];
        content::fill(SEED, first, &mut body);
        stream.write_all(&body).await?;
    }
    stream.shutdown().await
}

fn cli_exe() -> PathBuf {
    let exe = support::agent_exe().with_file_name("forgedm.exe");
    assert!(
        exe.is_file(),
        "{} is missing: run `cargo build -p forgedm-cli --bins` first",
        exe.display()
    );
    exe
}

fn run_cli(data: &Path, args: &[&str]) -> Output {
    Command::new(cli_exe())
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run forgedm")
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Every canary found in `haystack` (raw bytes, as a lossy string would hide nothing here:
/// the canaries are ASCII).
fn leaked(haystack: &[u8]) -> Vec<&'static str> {
    CANARIES
        .into_iter()
        .filter(|c| haystack.windows(c.len()).any(|w| w == c.as_bytes()))
        .collect()
}

fn assert_clean(what: &str, haystack: &[u8]) {
    let found = leaked(haystack);
    assert!(
        found.is_empty(),
        "{what} contains {found:?}: {}",
        String::from_utf8_lossy(&haystack[..haystack.len().min(600)])
    );
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

async fn wait_ended(client: &mut Client, id: Uuid) -> forgedm_protocol::DownloadDetail {
    let started = Instant::now();
    loop {
        let body = RequestBody::GetDownload(DownloadIdRequest { download_id: id });
        let ResultBody::GetDownload(detail) = client.call(body).await.unwrap() else {
            panic!("not a detail");
        };
        if matches!(
            detail.summary.state,
            DownloadState::Completed | DownloadState::Failed
        ) {
            return *detail;
        }
        assert!(
            started.elapsed() < WAIT,
            "timed out in {:?}",
            detail.summary.state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_canary_absent() {
    let (port, _seen) = spawn_server().await;
    let (data, target) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let agent = AgentProc::start(data.path(), &["--log-level", "debug"]);
    let (mut client, _) = agent.connect().await;
    let ok_url = format!("http://127.0.0.1:{port}/start?token=CANARY_Q#CANARY_F");
    let bad_url = format!("http://127.0.0.1:{port}/missing?token=CANARY_Q#CANARY_F");
    let mut frames: Vec<String> = Vec::new();

    // Two downloads: one that completes through the redirect, one that fails (404).
    let mut ids = Vec::new();
    for (url, name) in [(&ok_url, "canary.bin"), (&bad_url, "missing.bin")] {
        let mut request = EnqueueUrlRequest::new(url);
        request.target_dir = Some(target.path().to_str().unwrap().to_owned());
        request.file_name = Some(name.to_owned());
        let response = client
            .request(RequestBody::EnqueueUrl(request))
            .await
            .unwrap();
        frames.push(serde_json::to_string(&response).unwrap());
        ids.push(response_id(response));
    }
    let done = wait_ended(&mut client, ids[0]).await;
    assert_eq!(
        done.summary.state,
        DownloadState::Completed,
        "{:?}",
        done.summary.last_error
    );
    let failed = wait_ended(&mut client, ids[1]).await;
    assert_eq!(failed.summary.state, DownloadState::Failed);
    for id in &ids {
        let body = RequestBody::GetDownload(DownloadIdRequest { download_id: *id });
        let response = client.request(body).await.unwrap();
        frames.push(serde_json::to_string(&response).unwrap());
    }
    frames.push(
        serde_json::to_string(
            &client
                .request(RequestBody::GetSnapshot(Empty {}))
                .await
                .unwrap(),
        )
        .unwrap(),
    );
    frames.push(serde_json::to_string(&done).unwrap());
    frames.push(serde_json::to_string(&failed).unwrap());
    let final_path = PathBuf::from(done.summary.final_path.clone().expect("final path"));
    verify::file_matches(&final_path, SEED, SIZE).unwrap();

    // The real CLI, every command that could print a URL, plus rejections that might echo one.
    let id_text = ids[0].to_string();
    let bad_id = ids[1].to_string();
    let invocations: Vec<Vec<&str>> = vec![
        vec!["list"],
        vec!["list", "--json"],
        vec!["get", &id_text],
        vec!["get", &id_text, "--json"],
        vec!["get", &bad_id],
        vec!["get", &bad_id, "--json"],
        vec!["status"],
        vec!["add", "ftp://127.0.0.1/x?token=CANARY_Q#CANARY_F"],
        vec!["add", "not a url?token=CANARY_Q#CANARY_F"],
    ];
    let mut cli_output = String::new();
    for args in invocations {
        let out = run_cli(data.path(), &args);
        cli_output.push_str(&text(&out));
    }
    let add = run_cli(
        data.path(),
        &[
            "add",
            &ok_url,
            "--dir",
            target.path().to_str().unwrap(),
            "--file-name",
            "again.bin",
        ],
    );
    cli_output.push_str(&text(&add));
    assert_eq!(add.status.code(), Some(0), "{}", text(&add));
    assert!(
        cli_output.contains("canary.bin") && cli_output.contains("127.0.0.1"),
        "the CLI really printed the downloads: {cli_output}"
    );
    assert_clean("CLI stdout and stderr", cli_output.as_bytes());

    // A client that subscribes sees every later change as events.
    let mut events = Vec::new();
    let hello = support::connect_when_ready(data.path()).await;
    let (mut subscriber, instance) = (hello.0, hello.1.agent_instance_id);
    let subscribe = forgedm_protocol::SubscribeRequest {
        agent_instance_id: instance,
        after_seq: 0,
    };
    let response = subscriber
        .request(RequestBody::Subscribe(subscribe))
        .await
        .unwrap();
    frames.push(serde_json::to_string(&response).unwrap());
    while let Ok(Ok(event)) =
        tokio::time::timeout(Duration::from_millis(1500), subscriber.next_event()).await
    {
        events.push(serde_json::to_string(&event).unwrap());
    }
    assert!(!events.is_empty(), "the replay delivered events");
    frames.extend(events);
    assert!(
        frames.iter().any(|f| f.contains("127.0.0.1")),
        "the frames really carry the redacted URL"
    );
    assert_clean("IPC messages", frames.join("\n").as_bytes());

    // The Zone.Identifier stream of the completed file.
    let zone = std::fs::read(format!("{}:Zone.Identifier", final_path.display()))
        .expect("Zone.Identifier stream");
    assert!(contains(&zone, "ZoneId=3") && contains(&zone, "127.0.0.1"));
    assert_clean("Zone.Identifier", &zone);

    // Raw database files while the agent is running (WAL and SHM exist), then after a
    // graceful stop (checkpointed), and the logs of both.
    // A file that does not exist is empty; any other read error fails the test, so a locked
    // file can never make a scan pass vacuously.
    let read_opt = |name: &str| match std::fs::read(data.path().join(name)) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!("cannot read {name}: {err}"),
    };
    let read = |name: &str| read_opt(name).unwrap_or_default();
    let db = read_opt("forgedm.db").expect("the database file exists");
    let (wal, shm) = (read("forgedm.db-wal"), read("forgedm.db-shm"));
    assert!(
        contains(&db, "SQLite format 3") && contains(&wal, "/start"),
        "the database files are really read (the redacted display URL is stored in the clear,          and recent rows are still in the write-ahead log)"
    );
    assert!(!shm.is_empty(), "the shared-memory index is really read");
    for (name, bytes) in [
        ("forgedm.db", &db),
        ("forgedm.db-wal", &wal),
        ("forgedm.db-shm", &shm),
    ] {
        assert_clean(name, bytes);
    }
    let stop = run_cli(data.path(), &["agent", "stop"]);
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop));
    drop(agent);
    let db_after = read_opt("forgedm.db").expect("the database file exists after the stop");
    assert!(
        contains(&db_after, "SQLite format 3"),
        "the post-stop database is really read"
    );
    assert_clean("forgedm.db after a graceful stop", &db_after);
    assert_clean(
        "forgedm.db-wal after a graceful stop",
        &read("forgedm.db-wal"),
    );
    let logs = support::log_files(data.path());
    assert!(!logs.is_empty(), "the agent wrote logs");
    let all_logs: String = logs.iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        all_logs.contains("agent ready"),
        "the debug log is really read"
    );
    assert_clean("the logs", all_logs.as_bytes());
}

/// The `download_id` of an `EnqueueUrl` response, decoded through the typed result.
fn response_id(response: Response) -> Uuid {
    let Response::Success { result, .. } = response else {
        panic!("enqueue failed: {response:?}");
    };
    match ResultBody::from_value(Action::EnqueueUrl, result).unwrap() {
        ResultBody::EnqueueUrl(done) => done.download_id,
        other => panic!("not an enqueue result: {other:?}"),
    }
}

#[test]
fn the_detector_sees_a_planted_canary() {
    assert_eq!(leaked(b"..CANARY_R.."), ["CANARY_R"]);
    assert_eq!(
        leaked(b"a?token=CANARY_Q#CANARY_F"),
        ["CANARY_Q", "CANARY_F"]
    );
    assert!(leaked(b"https://example.com/file").is_empty());
}
