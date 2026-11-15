//! Security primitives: SIDs, pipe DACL, peer identity, UTF-16 and verbatim paths.

use std::fs::OpenOptions;
use std::os::windows::io::AsHandle;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use forgedm_platform::PlatformError;
use forgedm_platform::peer::{self, PeerError};
use forgedm_platform::pipe::{self, PIPE_MAX_INSTANCES, PipeError, PipeSecurity};
use forgedm_platform::sid::{Sid, validate_sid_string};
use forgedm_platform::wide;
use tokio::io::Interest;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, PipeEnd, PipeMode};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn unique_pipe_name() -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\ForgeDM-test-{}-{}", std::process::id(), n)
}

fn whoami_sid() -> Option<String> {
    let out = Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    // Output shape: "DOMAIN\user","S-1-5-21-..."
    let sid = text.trim().rsplit(',').next()?;
    Some(sid.trim_matches('"').to_owned())
}

#[test]
fn current_user_sid_matches_whoami_csv() {
    let sid = Sid::current_user().expect("current user");
    assert_eq!(
        sid.to_sid_string().expect("string"),
        whoami_sid().expect("whoami csv")
    );
}

#[test]
fn sid_validation_rejects_sddl_injection() {
    for good in ["S-1-5-18", "S-1-5-21-1-2-3-1001", "S-1-1-0"] {
        assert!(validate_sid_string(good).is_ok(), "{good} should be valid");
    }
    let bad = [
        "",
        "S-1-5",
        "s-1-5-18",
        "S-1-5-18\n",
        " S-1-5-18",
        "S-1-5-18 ",
        "S-1-5-21-1-2-3)(A;;GA;;;WD",
        "S-1-5-21-1-2-3)",
        "S-1-5-18;",
        "S-1-5--18",
        "S-1-5-1\u{0663}",
        "S-2-5-18",
        "WD",
        "S-1-5-18\0",
    ];
    for b in bad {
        assert!(validate_sid_string(b).is_err(), "{b:?} must be rejected");
        assert!(pipe::pipe_sddl(b).is_err(), "{b:?} must not reach SDDL");
    }
    let sddl = pipe::pipe_sddl("S-1-5-21-1-2-3-1001").expect("sddl");
    assert_eq!(
        sddl,
        "D:P(D;;GA;;;NU)(A;;GA;;;S-1-5-21-1-2-3-1001)S:(ML;;NWNR;;;ME)"
    );
}

#[test]
fn sid_equality_err_means_not_equal() {
    let me = Sid::current_user().expect("me");
    let system = Sid::parse("S-1-5-18").expect("system");
    assert_eq!(me, me.clone());
    assert_ne!(me, system);
    assert_eq!(system.to_sid_string().expect("s"), "S-1-5-18");
}

/// Splits a readback SDDL into (dacl flags, dacl aces, sacl flags, sacl aces); each ACE is
/// the raw `type;flags;rights;;;sid` text.
fn parse_sddl(sddl: &str) -> Option<(String, Vec<String>, String, Vec<String>)> {
    let rest = sddl.strip_prefix("D:")?;
    let (dacl, sacl) = rest.split_once("S:")?;
    let split = |s: &str| {
        let flags_end = s.find('(').unwrap_or(s.len());
        let aces = s[flags_end..]
            .split(')')
            .filter(|a| !a.is_empty())
            .map(|a| a.trim_start_matches('(').to_owned())
            .collect::<Vec<_>>();
        (s[..flags_end].to_owned(), aces)
    };
    let (df, da) = split(dacl);
    let (sf, sa) = split(sacl);
    Some((df, da, sf, sa))
}

#[tokio::test]
async fn pipe_dacl_readback_from_live_handle_is_deny_nu_allow_user_me_label_only() {
    let me = Sid::current_user().expect("me");
    let security = PipeSecurity::for_user(&me).expect("security");
    let handle = pipe::create_pipe_instance(&unique_pipe_name(), true, &security).expect("pipe");
    assert_locked_down(&handle, &me);
}

/// Reads the security descriptor back from a live pipe instance and asserts the exact
/// deny-NU / allow-user / medium-label policy.
#[allow(clippy::expect_used)]
fn assert_locked_down(handle: &NamedPipeServer, me: &Sid) {
    let sid = me.to_sid_string().expect("sid string");
    let readback = pipe::readback_security_sddl(handle.as_handle()).expect("readback");
    let (dflags, daces, sflags, saces) = parse_sddl(&readback).expect("readback parses");

    assert_eq!(
        sflags, "AI",
        "only the auto-inherited SACL flag expected: {readback}"
    );
    assert_eq!(dflags, "P", "DACL must be protected: {readback}");
    assert_eq!(daces.len(), 2, "exactly two DACL ACEs: {readback}");
    assert_eq!(daces[0], "D;;FA;;;NU", "deny NU must be first: {readback}");
    assert_eq!(
        daces[1],
        format!("A;;FA;;;{sid}"),
        "allow current user only: {readback}"
    );
    assert_eq!(
        saces,
        vec!["ML;;NWNR;;;ME".to_owned()],
        "medium label only: {readback}"
    );

    for ace in &daces {
        let trustee = ace.rsplit(';').next().expect("trustee");
        for forbidden in ["WD", "AN", "SY", "BA", "AC", "RC", "S-1-15-2-1"] {
            assert_ne!(trustee, forbidden, "forbidden trustee in {readback}");
        }
        assert!(
            !trustee.starts_with("S-1-15-3-"),
            "capability SID in {readback}"
        );
        assert!(
            !trustee.starts_with("S-1-15-2-"),
            "package SID in {readback}"
        );
    }
}

#[tokio::test]
async fn second_first_instance_is_name_in_use() {
    let security = PipeSecurity::for_current_user().expect("security");
    let name = unique_pipe_name();
    let _first = pipe::create_pipe_instance(&name, true, &security).expect("first");
    let second = pipe::create_pipe_instance(&name, true, &security);
    assert!(
        matches!(second, Err(PipeError::NameInUse)),
        "got {second:?}"
    );
    // A replacement (non-first) instance by the owner is allowed.
    let _replacement = pipe::create_pipe_instance(&name, false, &security).expect("replacement");
}

#[tokio::test]
async fn replacement_instances_carry_the_same_dacl_and_byte_mode() {
    let me = Sid::current_user().expect("me");
    let security = PipeSecurity::for_user(&me).expect("security");
    let name = unique_pipe_name();
    let first = pipe::create_pipe_instance(&name, true, &security).expect("first");
    let replacement = pipe::create_pipe_instance(&name, false, &security).expect("replacement");
    assert_locked_down(&first, &me);
    assert_locked_down(&replacement, &me);
    for server in [&first, &replacement] {
        let info = server.info().expect("pipe info");
        assert!(matches!(info.mode, PipeMode::Byte), "byte mode expected");
        assert!(matches!(info.end, PipeEnd::Server), "server end expected");
    }
}

#[tokio::test]
async fn pipe_instances_are_capped_at_max() {
    let security = PipeSecurity::for_current_user().expect("security");
    let name = unique_pipe_name();
    let mut held = vec![pipe::create_pipe_instance(&name, true, &security).expect("first")];
    for _ in 1..PIPE_MAX_INSTANCES {
        held.push(pipe::create_pipe_instance(&name, false, &security).expect("instance"));
    }
    assert_eq!(held.len(), 17);
    let over = pipe::create_pipe_instance(&name, false, &security);
    assert!(
        matches!(
            over,
            Err(PipeError::Platform(PlatformError::Win32 { code: 231, .. }))
        ),
        "expected ERROR_PIPE_BUSY, got {over:?}"
    );
}

#[tokio::test]
async fn pipe_instance_is_tokio_server_and_roundtrips_bytes() {
    let security = PipeSecurity::for_current_user().expect("security");
    let name = unique_pipe_name();
    let server: NamedPipeServer =
        pipe::create_pipe_instance(&name, true, &security).expect("server");
    let client = ClientOptions::new().open(&name).expect("same-user client");
    server.connect().await.expect("connect");
    let wrote = client
        .async_io(Interest::WRITABLE, || client.try_write(b"ping"))
        .await
        .expect("client write");
    assert_eq!(wrote, 4);
    let mut buf = [0_u8; 4];
    let read = server
        .async_io(Interest::READABLE, || server.try_read(&mut buf))
        .await
        .expect("server read");
    assert_eq!((read, &buf), (4, b"ping"));
    let wrote = server
        .async_io(Interest::WRITABLE, || server.try_write(b"pong"))
        .await
        .expect("server write");
    assert_eq!(wrote, 4);
    let read = client
        .async_io(Interest::READABLE, || client.try_read(&mut buf))
        .await
        .expect("client read");
    assert_eq!((read, &buf), (4, b"pong"));
}

#[tokio::test]
async fn pipe_rejects_names_outside_the_pipe_namespace() {
    let security = PipeSecurity::for_current_user().expect("security");
    for bad in [
        "",
        "ForgeDM",
        r"\\.\pipe\",
        r"\\server\pipe\x",
        "\\\\.\\pipe\\a\0b",
    ] {
        let r = pipe::create_pipe_instance(bad, true, &security);
        assert!(
            matches!(r, Err(PipeError::Platform(PlatformError::InvalidInput(_)))),
            "{bad:?}: {r:?}"
        );
    }
}

#[tokio::test]
async fn same_user_client_connects_and_pid_is_self() {
    let security = PipeSecurity::for_current_user().expect("security");
    let name = unique_pipe_name();
    let server = pipe::create_pipe_instance(&name, true, &security).expect("server");
    let client = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&name)
        .expect("same-user client");
    let pid = peer::pipe_client_pid(server.as_handle()).expect("client pid");
    assert_eq!(pid, std::process::id());
    let server_pid = peer::pipe_server_pid(client.as_handle()).expect("server pid");
    assert_eq!(server_pid, std::process::id());
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let s = canon.to_string_lossy().to_lowercase();
        s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_owned)
    };
    norm(a) == norm(b)
}

#[test]
fn peer_identity_of_self_matches_user_and_exe() {
    let me = Sid::current_user().expect("me");
    let ident = peer::peer_identity(std::process::id()).expect("identity");
    assert_eq!(ident.sid, me);
    let exe = std::env::current_exe().expect("exe");
    assert!(
        same_path(&ident.image_path, &exe),
        "{:?} vs {exe:?}",
        ident.image_path
    );
    // Accepted by verify_peer with the exact exe, in any case.
    let upper = PathBuf::from(exe.to_string_lossy().to_uppercase());
    assert!(peer::verify_peer(std::process::id(), &me, Some(&exe)).is_ok());
    assert!(peer::verify_peer(std::process::id(), &me, Some(&upper)).is_ok());
    assert!(peer::verify_peer(std::process::id(), &me, None).is_ok());
}

#[test]
fn peer_check_rejects_unexpected_image() {
    let me = Sid::current_user().expect("me");
    let other = Path::new(r"C:\Windows\System32\cmd.exe");
    let r = peer::verify_peer(std::process::id(), &me, Some(other));
    assert!(
        matches!(r, Err(PeerError::ImageMismatch { .. })),
        "got {r:?}"
    );
    let system = Sid::parse("S-1-5-18").expect("system");
    let r = peer::verify_peer(std::process::id(), &system, None);
    assert!(matches!(r, Err(PeerError::SidMismatch)), "got {r:?}");
}

#[test]
fn peer_check_fails_closed_for_pid_4() {
    let me = Sid::current_user().expect("me");
    assert!(peer::peer_identity(4).is_err());
    let r = peer::verify_peer(4, &me, None);
    assert!(matches!(r, Err(PeerError::Platform(_))), "got {r:?}");
    assert!(peer::verify_peer(0xFFFF_FFF0, &me, None).is_err());
}

#[test]
fn wide_rejects_interior_nul_and_builds_verbatim_paths() {
    assert!(matches!(
        wide::to_wide_nul("a\0b"),
        Err(PlatformError::InvalidInput(_))
    ));
    let w = wide::to_wide_nul("a\u{1F600}").expect("wide");
    assert_eq!(w, vec![0x61, 0xD83D, 0xDE00, 0]);
    assert_eq!(wide::to_wide_nul("").expect("empty"), vec![0]);

    let v = wide::verbatim_path(Path::new(r"C:\Users\x\file.bin")).expect("disk");
    assert_eq!(v, PathBuf::from(r"\\?\C:\Users\x\file.bin"));
    let v = wide::verbatim_path(Path::new("C:/Users/x/file.bin")).expect("forward slashes");
    assert_eq!(v, PathBuf::from(r"\\?\C:\Users\x\file.bin"));
    let v = wide::verbatim_path(Path::new(r"\\srv\share\d\f.bin")).expect("unc");
    assert_eq!(v, PathBuf::from(r"\\?\UNC\srv\share\d\f.bin"));
    let already = Path::new(r"\\?\C:\a\b");
    assert_eq!(wide::verbatim_path(already).expect("verbatim"), already);

    for bad in [
        r"relative\x",
        r"C:\a\..\b",
        r"C:rel",
        "C:\\a\0b",
        r"\rooted",
    ] {
        assert!(
            wide::verbatim_path(Path::new(bad)).is_err(),
            "{bad:?} must be rejected"
        );
    }
    let w = wide::verbatim_wide(Path::new(r"C:\a")).expect("wide path");
    assert_eq!(w.last(), Some(&0));
    assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), r"\\?\C:\a");
}

#[test]
fn verbatim_path_preserves_unpaired_surrogates() {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let mut units: Vec<u16> = r"C:\tmp\a".encode_utf16().collect();
    units.push(0xD800);
    units.extend("b.bin".encode_utf16());
    let input = PathBuf::from(OsString::from_wide(&units));
    let out = wide::verbatim_path(&input).expect("lone surrogate is preserved");
    let got: Vec<u16> = out.as_os_str().encode_wide().collect();
    let mut want: Vec<u16> = r"\\?\".encode_utf16().collect();
    want.extend(&units);
    assert_eq!(got, want);
    assert!(!got.contains(&0xFFFD));
}

#[test]
fn verbatim_path_passes_component_names_through_unvalidated() {
    // Contract: names are the sanitiser's job (FS-01); MOTW needs `:Zone.Identifier`.
    let v = wide::verbatim_path(Path::new(r"C:\d\x.part:Zone.Identifier")).expect("stream");
    assert_eq!(v, PathBuf::from(r"\\?\C:\d\x.part:Zone.Identifier"));
}
