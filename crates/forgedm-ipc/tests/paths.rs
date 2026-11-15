//! Shared pipe-name and data-root helpers (PHASE_0_1_PLAN S1.8, A.10).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test code

use std::path::{Path, PathBuf};

use forgedm_ipc::paths::{default_data_root, pipe_name_for, root_hash};
use forgedm_platform::known_folders;
use forgedm_platform::sid::Sid;
use sha2::{Digest, Sha256};

fn sid() -> String {
    Sid::current_user()
        .expect("current user")
        .to_sid_string()
        .expect("sid string")
}

fn plain_name() -> String {
    format!(r"\\.\pipe\ForgeDM-{}", sid())
}

/// The agent's former computation (`forgedm_agent::config` before S1.8), kept verbatim as
/// the reference the helpers must agree with.
mod former {
    use std::path::Path;

    use sha2::{Digest, Sha256};

    fn normalized_root(path: &Path) -> String {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let lower = canonical.to_string_lossy().to_lowercase();
        let stripped = if let Some(unc) = lower.strip_prefix(r"\\?\unc\") {
            format!(r"\\{unc}")
        } else {
            lower
                .strip_prefix(r"\\?\")
                .map_or_else(|| lower.clone(), str::to_owned)
        };
        let trimmed = stripped.trim_end_matches('\\');
        if trimmed.ends_with(':') {
            format!("{trimmed}\\")
        } else {
            trimmed.to_owned()
        }
    }

    fn root_hash(root: &Path) -> String {
        let bytes: Vec<u8> = normalized_root(root)
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut hex = hex::encode(Sha256::digest(bytes));
        hex.truncate(16);
        hex
    }

    pub fn pipe_name(sid: &str, data_dir: Option<&Path>, default_root: &Path) -> String {
        let non_default = match data_dir {
            Some(dir) if normalized_root(dir) != normalized_root(default_root) => Some(dir),
            _ => None,
        };
        match non_default {
            Some(root) => format!(r"\\.\pipe\ForgeDM-{sid}-{}", root_hash(root)),
            None => format!(r"\\.\pipe\ForgeDM-{sid}"),
        }
    }
}

#[test]
fn default_data_root_is_local_app_data_forgedm() {
    let expected = known_folders::local_app_data().unwrap().join("ForgeDM");
    assert_eq!(default_data_root().unwrap(), expected);
    assert!(default_data_root().unwrap().is_absolute());
}

#[test]
fn default_pipe_name_is_prefix_plus_sid_without_suffix() {
    assert_eq!(pipe_name_for(None).unwrap(), plain_name());
}

#[test]
fn explicit_default_root_gets_the_plain_name() {
    let default = default_data_root().unwrap();
    assert_eq!(pipe_name_for(Some(&default)).unwrap(), plain_name());
    let shouting = PathBuf::from(default.to_string_lossy().to_uppercase());
    assert_eq!(pipe_name_for(Some(&shouting)).unwrap(), plain_name());
}

#[test]
fn custom_root_appends_16_lowercase_hex() {
    let name = pipe_name_for(Some(Path::new(r"C:\Nope\Root"))).unwrap();
    let suffix = name.strip_prefix(&format!("{}-", plain_name())).unwrap();
    assert_eq!(suffix.len(), 16);
    assert!(
        suffix
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}

#[test]
fn root_hash_ignores_case_and_trailing_separator_but_not_the_root() {
    let a = root_hash(Path::new(r"C:\Nope\Root"));
    assert_eq!(a, root_hash(Path::new(r"c:\nope\ROOT\")));
    assert_ne!(a, root_hash(Path::new(r"C:\Nope\Other")));
}

#[test]
fn root_hash_is_sha256_of_lowercased_utf16le() {
    let bytes: Vec<u8> = "c:\\x".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let expected = hex::encode(Sha256::digest(bytes));
    assert_eq!(root_hash(Path::new(r"C:\X")), expected[..16]);
}

#[test]
fn helper_agrees_with_the_agents_former_computation() {
    let default = default_data_root().unwrap();
    let existing = tempfile::tempdir().unwrap();
    let missing = existing.path().join("not-created-yet");
    let cases: [Option<&Path>; 5] = [
        None,
        Some(&default),
        Some(existing.path()),
        Some(&missing),
        Some(Path::new(r"C:\Nope\Root")),
    ];
    for case in cases {
        assert_eq!(
            pipe_name_for(case).unwrap(),
            former::pipe_name(&sid(), case, &default),
            "{case:?}"
        );
    }
}

#[test]
fn distinct_roots_get_distinct_pipe_names() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    assert_ne!(
        pipe_name_for(Some(a.path())).unwrap(),
        pipe_name_for(Some(b.path())).unwrap()
    );
}

#[test]
fn missing_non_canonical_root_hashes_like_its_canonical_form_after_creation() {
    let base = std::env::temp_dir().join(format!("forgedm-paths-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("base");
    let base = std::fs::canonicalize(&base).expect("canonical base");
    let dotted = base.join("a").join("..").join(".").join("b");
    let slashed = PathBuf::from(base.join("c").to_string_lossy().replace('\\', "/"));

    let before_dotted = root_hash(&dotted);
    let before_slashed = root_hash(&slashed);
    let before_name = pipe_name_for(Some(&dotted)).expect("name");

    std::fs::create_dir_all(base.join("b")).expect("b");
    std::fs::create_dir_all(base.join("c")).expect("c");
    let after_dotted = root_hash(&dotted);
    let after_slashed = root_hash(&slashed);
    let after_name = pipe_name_for(Some(&dotted)).expect("name");
    let _ = std::fs::remove_dir_all(&base);

    assert_eq!(before_dotted, after_dotted);
    assert_eq!(before_slashed, after_slashed);
    assert_eq!(before_name, after_name);
    assert_eq!(before_dotted, root_hash(&base.join("b")));
}
