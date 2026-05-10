//! Integration tests for the `qa-title-validation` boundary fix
//! (issue `e4e483b`). Drives the compiled `jjf` binary against per-test
//! scratch repos and asserts the typed `invalid_title` rejection on
//! both `jjf new -t` and `jjf update --title`.
//!
//! Background: the QA red-team round 2026-06-23 found two
//! title-validation gaps:
//!
//! 1. `jjf new -t $'a\x00b' --slug t` silently truncated the title
//!    to `"a"` (data loss before storage).
//! 2. `jjf new -t $'foo\nbar' --slug t` succeeded, but the resulting
//!    ticket corrupted `jjf ls` text rows (tab-separated format has
//!    no escape rule for embedded newlines).
//!
//! Both rejections fire at the CLI boundary now (preflight, exit 2)
//! AND in `Storage::create_issue` / `Storage::update` (defense in
//! depth). These tests pin the JSON envelope shape pinned in
//! `docs/cli-json.md`.
//!
//! Style mirrors `tests/type_and_slug.rs` (the sibling slug-validation
//! integration suite).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

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
    assert!(out.status.success());
    dir
}

fn make_initialized_repo(name: &str) -> PathBuf {
    let repo = make_jj_repo(name);
    let out = Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .arg("init")
        .current_dir(&repo)
        .output()
        .expect("spawn jjf init");
    assert!(
        out.status.success(),
        "jjf init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    repo
}

fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .env("JJF_ALLOW_SELF_HOST", "1")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

/// Parse stderr as the JSON error envelope. Panics with a helpful
/// message if the bytes aren't valid JSON — useful when a test fails
/// because the rejection didn't fire (and stderr is a human-readable
/// error trace instead).
fn parse_envelope(stderr_bytes: &[u8]) -> serde_json::Value {
    let s = String::from_utf8_lossy(stderr_bytes);
    serde_json::from_str(s.trim())
        .unwrap_or_else(|e| panic!("envelope must be json; got {s:?}: {e}"))
}

// --- `jjf new -t <bad>` rejections ---------------------------------------

#[test]
fn new_rejects_embedded_newline_in_title() {
    let repo = make_initialized_repo("title_new_newline");
    let out = run_jjf(&repo, &["--json", "new", "-t", "foo\nbar"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "invalid_title");
    assert_eq!(v["error"]["details"]["reason"], "newline");
    assert_eq!(v["error"]["details"]["title"], "foo\nbar");
}

// NOTE: there is intentionally no CLI test for "embedded null byte
// in title" because POSIX argv is a NUL-terminated C string array
// — `std::process::Command::arg` refuses inputs containing `\0`
// (the kernel `execve(2)` can't carry them either). The original QA
// repro `jjf new -t $'a\x00b'` succeeded with `title="a"` because
// bash's `$'…\x00…'` truncates at the null BEFORE writing argv,
// not because jjf lost the bytes. The defense-in-depth for any
// programmatic / library caller (e.g. Python, a future MCP server)
// lives in `Storage::create_issue` and is pinned by the
// `create_issue_rejects_embedded_null_byte_in_title` integration
// test in `crates/jjf-storage/tests/integration.rs`.

#[test]
fn new_rejects_tab_in_title_as_control_char() {
    let repo = make_initialized_repo("title_new_tab");
    let out = run_jjf(&repo, &["--json", "new", "-t", "a\tb"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["error"]["kind"], "invalid_title");
    assert_eq!(v["error"]["details"]["reason"], "control_char");
    // Tab is U+0009.
    assert_eq!(v["error"]["details"]["codepoint"], 9);
}

#[test]
fn new_rejects_empty_title_with_typed_envelope() {
    let repo = make_initialized_repo("title_new_empty");
    let out = run_jjf(&repo, &["--json", "new", "-t", "   "]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["error"]["kind"], "invalid_title");
    assert_eq!(v["error"]["details"]["reason"], "empty");
}

// --- `jjf update --title <bad>` rejections -------------------------------

#[test]
fn update_rejects_embedded_newline_in_title() {
    let repo = make_initialized_repo("title_update_newline");
    let create = run_jjf(&repo, &["new", "-t", "baseline"]);
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_owned();

    let out = run_jjf(&repo, &["--json", "update", &id, "--title", "foo\nbar"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["error"]["kind"], "invalid_title");
    assert_eq!(v["error"]["details"]["reason"], "newline");
}

// (Same as for `new` above — `jjf update --title $'a\x00b'` can't
// reach the storage layer through POSIX argv. The defense-in-depth
// for null bytes via `Storage::update` is pinned in
// `update_with_invalid_title_is_rejected_before_commit` in
// `crates/jjf-storage/tests/integration.rs`.)

// --- non-regression for legitimate titles --------------------------------

#[test]
fn new_accepts_unicode_punctuation_em_dash_title() {
    // The asterinas migration carries titles like
    // "host-asterinas-migrate: import the upstream tree" and
    // "Why doesn't \"qux\" work? (it should)". The validator must
    // NOT bounce these.
    let repo = make_initialized_repo("title_new_accepts_unicode");
    let out = run_jjf(
        &repo,
        &["--json", "new", "-t", "rust/no_std — drop the alloc crate"],
    );
    assert!(
        out.status.success(),
        "should accept legit prose; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
