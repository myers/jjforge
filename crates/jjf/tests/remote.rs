//! Integration tests for `jjf remote add <name> <url>`, `jjf remote
//! ls`, and `jjf remote rm <name>` — drive the compiled binary
//! against per-test scratch jj repos and assert exit code, stdout
//! shape (plain + `--json`), the round-trip semantics (add → ls →
//! rm → ls), and the preflight / runtime error matrix:
//!
//! - happy add then `ls` lists it (plain + `--json`),
//! - `--json` envelope shape for each arm,
//! - `ls` on a fresh jj repo (no remotes) returns empty plain and
//!   `[]` under `--json`,
//! - `add` a name that already exists → exit 2 + `remote_already_exists`,
//! - `rm` a name that doesn't exist → exit 2 + `remote_not_found`,
//! - non-jj cwd → exit 2 + `not_a_jj_repo` (covers all three arms),
//! - `ls` works in a jj repo with NO `bugs` bookmark (preflight is
//!   jj-repo-only, not the full bugs probe),
//! - `--help` documents the three subcommands.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate. We deliberately use https:// URLs the
//! tests never reach — `remote add` doesn't talk to the URL, just
//! records it, so the test is fully offline.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

/// Per-test scratch root. Gitignored via the workspace-level rule.
fn scratch(name: &str) -> PathBuf {
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

fn make_jj_repo(name: &str) -> PathBuf {
    let dir = scratch(name);
    let out = Command::new("jj")
        .args(["git", "init"])
        .current_dir(&dir)
        .output()
        .expect("spawn jj");
    assert!(
        out.status.success(),
        "jj git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
    }

// --- tests ---------------------------------------------------------

#[test]
fn remote_add_happy_path_then_ls_lists_it() {
    let repo = make_jj_repo("remote_add_happy");

    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(
        out.status.success(),
        "remote add failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "remote origin added: https://example.com/x.git",
        "plain stdout shape wrong: {stdout}"
    );

    // ls should now list one remote.
    let out = run_jjf(&repo, &["remote", "ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim(),
        "origin\thttps://example.com/x.git",
        "ls plain shape wrong (must be tab-separated): {stdout}"
    );
}

#[test]
fn remote_add_and_ls_json_envelopes() {
    let repo = make_jj_repo("remote_add_ls_json");

    // add --json
    let out = run_jjf(
        &repo,
        &["remote", "--json", "add", "origin", "https://example.com/x.git"],
    );
    assert!(
        out.status.success(),
        "remote add --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("remote add --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok wrong: {stdout}");
    assert_eq!(v["name"].as_str(), Some("origin"), "name wrong: {stdout}");
    assert_eq!(
        v["url"].as_str(),
        Some("https://example.com/x.git"),
        "url wrong: {stdout}"
    );

    // ls --json — array of one object
    let out = run_jjf(&repo, &["remote", "--json", "ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("remote ls --json must be valid JSON");
    let arr = v.as_array().expect("ls --json must be a JSON array");
    assert_eq!(arr.len(), 1, "ls should have one entry: {stdout}");
    assert_eq!(arr[0]["name"].as_str(), Some("origin"));
    assert_eq!(arr[0]["url"].as_str(), Some("https://example.com/x.git"));
}

#[test]
fn remote_ls_empty_repo_silent_plain_and_empty_array_json() {
    let repo = make_jj_repo("remote_ls_empty");

    // plain text: silence (zero lines), exit 0.
    let out = run_jjf(&repo, &["remote", "ls"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(
        out.stdout.is_empty(),
        "ls of empty repo must be silent, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // --json: empty array.
    let out = run_jjf(&repo, &["remote", "--json", "ls"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let arr = v.as_array().expect("must be array");
    assert!(arr.is_empty(), "empty ls --json must be []: {stdout}");
}

#[test]
fn remote_add_then_rm_round_trip() {
    let repo = make_jj_repo("remote_add_rm_round_trip");

    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["remote", "rm", "origin"]);
    assert!(
        out.status.success(),
        "remote rm failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "remote origin removed", "rm plain wrong: {stdout}");

    // ls should be empty again.
    let out = run_jjf(&repo, &["remote", "ls"]);
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty(),
        "ls after rm must be empty: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn remote_rm_json_envelope_shape() {
    let repo = make_jj_repo("remote_rm_json");

    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["remote", "--json", "rm", "origin"]);
    assert!(
        out.status.success(),
        "remote rm --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("rm --json must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true));
    assert_eq!(v["name"].as_str(), Some("origin"));
}

#[test]
fn remote_add_duplicate_exits_two_with_typed_kind() {
    let repo = make_jj_repo("remote_add_duplicate");

    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Second add with the same name — exit 2, plain stderr mentions the name.
    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/y.git"]);
    assert!(!out.status.success(), "duplicate add should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "duplicate add must exit 2 (preflight), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("origin"),
        "stderr should mention the name `origin`, got: {stderr}"
    );

    // JSON envelope variant — kind must be `remote_already_exists`.
    let out = run_jjf(
        &repo,
        &["--json", "remote", "add", "origin", "https://example.com/z.git"],
    );
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("remote_already_exists"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["name"].as_str(),
        Some("origin"),
        "details.name wrong: {stderr}"
    );
}

#[test]
fn remote_rm_nonexistent_exits_two_with_typed_kind() {
    let repo = make_jj_repo("remote_rm_nonexistent");

    // Plain stderr — exit 2, mentions the missing name.
    let out = run_jjf(&repo, &["remote", "rm", "nope"]);
    assert!(!out.status.success(), "rm of absent remote should fail");
    assert_eq!(
        out.status.code(),
        Some(2),
        "rm of absent remote must exit 2 (preflight), got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nope"), "stderr should mention `nope`, got: {stderr}");

    // JSON envelope variant — kind must be `remote_not_found`.
    let out = run_jjf(&repo, &["--json", "remote", "rm", "nope"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("remote_not_found"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["name"].as_str(),
        Some("nope"),
        "details.name wrong: {stderr}"
    );
}

#[test]
fn remote_add_in_non_jj_directory_exits_two() {
    let dir = scratch("remote_add_non_jj");
    let out = run_jjf(&dir, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(!out.status.success(), "remote add in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn remote_ls_in_non_jj_directory_exits_two() {
    let dir = scratch("remote_ls_non_jj");
    let out = run_jjf(&dir, &["remote", "ls"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn remote_rm_in_non_jj_directory_exits_two() {
    let dir = scratch("remote_rm_non_jj");
    let out = run_jjf(&dir, &["remote", "rm", "origin"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn remote_verbs_do_not_require_bugs_bookmark() {
    // The whole point of using the `jj_repo`-only preflight (instead
    // of the full `bugs_bookmark` probe): `jjf remote *` must work in
    // a jj repo BEFORE `jjf init` has been run. Without this, you'd
    // have to init a bugs bookmark before you could even configure a
    // remote, which is backwards (`jjf push` is what first sends the
    // bookmark to a remote).
    let repo = make_jj_repo("remote_no_bugs_bookmark");

    // ls succeeds (empty).
    let out = run_jjf(&repo, &["remote", "ls"]);
    assert!(
        out.status.success(),
        "remote ls must work without `bugs` bookmark: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // add succeeds.
    let out = run_jjf(&repo, &["remote", "add", "origin", "https://example.com/x.git"]);
    assert!(
        out.status.success(),
        "remote add must work without `bugs` bookmark: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // rm succeeds.
    let out = run_jjf(&repo, &["remote", "rm", "origin"]);
    assert!(
        out.status.success(),
        "remote rm must work without `bugs` bookmark: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_help_documents_subcommands() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["remote", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf remote --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("add"), "remote --help should list `add`: {help}");
    assert!(help.contains("ls"), "remote --help should list `ls`: {help}");
    assert!(help.contains("rm"), "remote --help should list `rm`: {help}");
}

#[test]
fn remote_add_help_documents_positionals() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(["remote", "add", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf remote add --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<NAME>"), "add --help should document <NAME>: {help}");
    assert!(help.contains("<URL>"), "add --help should document <URL>: {help}");
}
