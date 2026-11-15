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
