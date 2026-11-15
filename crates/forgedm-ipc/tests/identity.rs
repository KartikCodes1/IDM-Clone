//! Caller and server identity decisions, and the wrong-server and low-integrity guards.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code fails by panicking

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use common::{RunningServer, client_config, unique_pipe_name};
use forgedm_ipc::{Client, ClientConfig, IpcError, decide_identity};
use forgedm_platform::peer::{PeerError, PeerIdentity};
use forgedm_platform::pipe::{PipeSecurity, create_pipe_instance};
use forgedm_platform::sid::Sid;
use tokio::io::AsyncReadExt;

const HELPER_PIPE_VAR: &str = "FORGEDM_IPC_LOW_IL_HELPER_PIPE";
const HELPER_TEST: &str = "low_integrity_helper_probe";
const ERROR_ACCESS_DENIED: i32 = 5;

fn me() -> Sid {
    Sid::current_user().expect("current user")
}

fn this_exe() -> PathBuf {
    std::env::current_exe().expect("current exe")
}

#[test]
fn identity_decision_rejects_other_sid() {
    let system = Sid::parse("S-1-5-18").expect("system sid");
    let other_user = PeerIdentity {
        sid: system,
        image_path: this_exe(),
    };

    let result = decide_identity(&other_user, &me(), Some(&this_exe()));
    assert!(
        matches!(result, Err(PeerError::SidMismatch)),
        "got {result:?}"
    );
    // The SID check also applies when no image is pinned.
    assert!(matches!(
        decide_identity(&other_user, &me(), None),
        Err(PeerError::SidMismatch)
    ));

    let same_user = PeerIdentity {
        sid: me(),
        image_path: this_exe(),
    };
    assert!(decide_identity(&same_user, &me(), None).is_ok());
}

#[test]
fn identity_decision_compares_full_image_path_case_insensitively() {
    let exe = this_exe();
    let peer = PeerIdentity {
        sid: me(),
        image_path: exe.clone(),
    };

    let shouting = PathBuf::from(exe.to_string_lossy().to_uppercase());
    assert!(decide_identity(&peer, &me(), Some(&shouting)).is_ok());
    let verbatim = PathBuf::from(format!(r"\\?\{}", exe.display()));
    assert!(decide_identity(&peer, &me(), Some(&verbatim)).is_ok());

    // Same file name in another directory is a different image.
    let name = exe.file_name().expect("file name");
    let elsewhere = Path::new(r"C:\Windows\Temp").join(name);
    assert!(matches!(
        decide_identity(&peer, &me(), Some(&elsewhere)),
        Err(PeerError::ImageMismatch { .. })
    ));
}

/// A server that records every byte a client sends before closing.
async fn spawn_recording_server(name: &str) -> tokio::task::JoinHandle<usize> {
    let security = PipeSecurity::for_current_user().expect("security");
    let mut instance = create_pipe_instance(name, true, &security).expect("instance");
    tokio::spawn(async move {
        instance.connect().await.expect("connect");
        let mut total = 0usize;
        let mut buf = [0u8; 4096];
        // Ends on EOF (client closed), error, or a 2 s quiet period.
        while let Ok(Ok(n)) =
            tokio::time::timeout(Duration::from_secs(2), instance.read(&mut buf)).await
        {
            if n == 0 {
                break;
            }
            total += n;
        }
        total
    })
}

#[tokio::test]
async fn wrong_server_image_rejected_and_server_receives_zero_frames() {
    let name = unique_pipe_name("wrongimage");
    let recorder = spawn_recording_server(&name).await;
    let wrong = ClientConfig::new(&name, PathBuf::from(r"C:\Windows\System32\notepad.exe"));

    let result = Client::connect(wrong).await;

    assert!(
        matches!(
            result,
            Err(IpcError::ServerRejected(PeerError::ImageMismatch { .. }))
        ),
        "got {:?}",
        result.err()
    );
    assert_eq!(
        recorder.await.expect("recorder"),
        0,
        "server must see zero bytes"
    );
}

#[tokio::test]
async fn right_server_image_is_accepted_and_does_send_bytes() {
    // Control for the zero-bytes test: with the correct image the same recorder sees bytes.
    let name = unique_pipe_name("rightimage");
    let recorder = spawn_recording_server(&name).await;
    let mut config = client_config(&name);
    config.expected_agent_path = PathBuf::from(this_exe().to_string_lossy().to_uppercase());
    config.request_timeout = Duration::from_millis(300);

    let mut client = Client::connect(config).await.expect("verified server");
    let result = client
        .handshake(forgedm_protocol::ClientKind::Cli, "control", "0.0.0")
        .await;

    assert!(matches!(result, Err(IpcError::Timeout)));
    drop(client);
    assert!(
        recorder.await.expect("recorder") > 0,
        "control must send a frame"
    );
}

/// The helper entry point: runs only inside the copied, re-labelled test executable.
///
/// Tries a plain read/write open of the pipe named by [`HELPER_PIPE_VAR`] and prints
/// `HELPER_RESULT=<connected|denied|error>` to stdout. A no-op in a normal run.
#[test]
fn low_integrity_helper_probe() {
    let Ok(pipe) = std::env::var(HELPER_PIPE_VAR) else {
        return;
    };
    let outcome = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pipe)
    {
        Ok(_) => "connected".to_owned(),
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => "denied".to_owned(),
        Err(e) => format!("error:{e}"),
    };
    let _ = writeln!(std::io::stdout(), "HELPER_RESULT={outcome}");
}

/// Runs the helper copy and returns its reported outcome.
fn run_helper(exe: &Path, pipe: &str) -> Result<String, String> {
    let output = Command::new(exe)
        .args(["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"])
        .env(HELPER_PIPE_VAR, pipe)
        .output()
        .map_err(|e| format!("cannot start helper: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|l| {
            l.split_once("HELPER_RESULT=")
                .map(|(_, r)| r.trim().to_owned())
        })
        .ok_or_else(|| format!("helper reported nothing (status {:?})", output.status))
}

fn set_low_integrity(exe: &Path) -> Result<(), String> {
    let out = Command::new("icacls")
        .arg(exe)
        .args(["/setintegritylevel", "low"])
        .output()
        .map_err(|e| format!("icacls missing: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "icacls failed: {}",
            String::from_utf8_lossy(&out.stdout)
        ))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn low_integrity_client_denied() {
    let server = RunningServer::start("lowil");
    let dir = tempfile::tempdir().expect("temp dir");
    let helper = dir.path().join("forgedm-ipc-helper.exe");
    std::fs::copy(this_exe(), &helper).expect("copy test exe");
    let pipe = server.name.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        // Control: the unlabelled copy runs at medium integrity and must connect.
        let control = run_helper(&helper, &pipe)?;
        if control != "connected" {
            return Err(format!("control helper did not connect: {control}"));
        }
        set_low_integrity(&helper)?;
        run_helper(&helper, &pipe)
    })
    .await
    .expect("blocking task");

    // A setup failure is a hard failure: the claim must never be reported as passed unrun.
    let result = outcome.unwrap_or_else(|reason| {
        panic!("low_integrity_client_denied setup failed (NOT RUN): {reason}")
    });
    assert_eq!(
        result, "denied",
        "a low-integrity client must not open the pipe"
    );
    server.stop().await.expect("clean stop");
}

#[tokio::test]
async fn client_rejects_non_local_pipe_names() {
    let config = forgedm_ipc::ClientConfig::new(r"\\remotehost\pipe\ForgeDM-test", r"C:\x.exe");
    let err = forgedm_ipc::Client::connect(config)
        .await
        .expect_err("remote pipe name must be refused");
    assert!(matches!(err, forgedm_ipc::IpcError::InvalidPipeName));
}
