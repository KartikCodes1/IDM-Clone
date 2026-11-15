//! Binary smoke tests: `hash` and `serve --port 0`.

use forgedm_fixture::content;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_forgedm-fixture");

#[test]
fn hash_subcommand_matches_generator() {
    let out = Command::new(BIN)
        .args(["hash", "--seed", "5", "--size", "123456"])
        .output()
        .expect("run");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert_eq!(text.trim(), hex::encode(content::sha256(5, 123_456)));
}

#[test]
fn serve_prints_loopback_address() {
    let mut child = Command::new(BIN)
        .args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read");
    child.kill().expect("kill");
    child.wait().expect("wait");
    assert!(line.trim().starts_with("http://127.0.0.1:"), "got {line:?}");
}

#[test]
fn verify_prefix_accepts_a_matching_prefix_and_rejects_a_flipped_byte() {
    let dir = std::env::temp_dir().join(format!("fixture-prefix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("p.bin");
    let mut data = vec![0u8; 100_000];
    content::fill(9, 0, &mut data);
    std::fs::write(&file, &data).expect("write");
    let run = |len: &str| {
        Command::new(BIN)
            .args(["verify-prefix", "--seed", "9", "--len", len, "--file"])
            .arg(&file)
            .output()
            .expect("run")
    };

    let ok = run("60000");
    data[50_000] ^= 0xFF;
    std::fs::write(&file, &data).expect("rewrite");
    let flipped = run("60000");
    let before_flip = run("50000");
    let too_long = run("100001");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(ok.status.success());
    assert!(
        !flipped.status.success(),
        "a flipped byte inside the prefix fails"
    );
    assert!(
        before_flip.status.success(),
        "bytes after the prefix are ignored"
    );
    assert!(
        !too_long.status.success(),
        "a file shorter than the prefix fails"
    );
}
