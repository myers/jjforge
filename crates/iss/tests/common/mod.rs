//! Shared helpers for integration tests. Not all helpers are used by
//! every consuming test binary — `dead_code` is allowed at module
//! scope so per-binary unused warnings stay quiet.
#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use iss_storage::IssueId;

pub(crate) const ISS_BIN: &str = env!("CARGO_BIN_EXE_iss");

pub(crate) fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

/// Create a scratch directory that is guaranteed to be OUTSIDE any git
/// repo. Used by tests that assert "not a git repo" preflight behavior.
/// Lives under `std::env::temp_dir()` so it cannot be inside the
/// jjforge source tree (which is itself a git repo).
pub(crate) fn scratch_non_git(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("jjf-tests").join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

pub(crate) fn make_jj_repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("git")
        .arg("init")
        .current_dir(&dir)
        .output()
        .expect("spawn git init");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Set repo-local identity so verbs that commit have an author.
    let out = Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.name");
    assert!(
        out.status.success(),
        "git config user.name failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(&dir)
        .output()
        .expect("spawn git config user.email");
    assert!(
        out.status.success(),
        "git config user.email failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

pub(crate) fn make_initialized_repo(name: &str) -> PathBuf {
    let repo = make_jj_repo(name);
    let out = Command::new(ISS_BIN)
        .arg("init")
        .current_dir(&repo)
        .output()
        .expect("spawn jjf init");
    assert!(
        out.status.success(),
        "jjf init in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

pub(crate) fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(ISS_BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

pub(crate) fn run_jjf_with_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(ISS_BIN)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn jjf");
    child
        .stdin
        .as_mut()
        .expect("stdin handle")
        .write_all(stdin_bytes)
        .expect("write stdin");
    child.wait_with_output().expect("wait for jjf")
}

pub(crate) fn parse_id_from_stdout(stdout: &[u8]) -> IssueId {
    let json_str = String::from_utf8_lossy(stdout);
    let json_obj: serde_json::Value = serde_json::from_str(&json_str)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nstdout: {json_str}"));
    let id_str = json_obj["id"]
        .as_str()
        .unwrap_or_else(|| panic!("JSON missing 'id' field: {json_obj}"));
    IssueId::parse(id_str)
        .unwrap_or_else(|e| panic!("stdout id {:?} is not a valid IssueId: {e}", id_str))
}

pub(crate) fn parse_envelope(stderr_bytes: &[u8]) -> serde_json::Value {
    let s = String::from_utf8_lossy(stderr_bytes);
    serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("envelope must be json; got {s:?}: {e}"))
}
