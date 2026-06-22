//! Integration tests for `jjf new` — drive the compiled binary against
//! per-test scratch repos and assert exit code, stderr, stdout (plain +
//! `--json`), and observable storage state via `Storage::read`.
//!
//! Mirrors `init.rs`: hermetic per-test scratch under `tests/.scratch/`,
//! wiped on each run, gitignored via `crates/**/tests/.scratch/`. No
//! `assert_cmd` dep — `CARGO_BIN_EXE_jjf` + `std::process` is enough.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use jjf_storage::{BugId, Storage};

/// Path to the compiled `jjf` binary. Cargo sets this env var for every
/// integration test in the same package as the `[[bin]]` target.
const JJF_BIN: &str = env!("CARGO_BIN_EXE_jjf");

/// Per-test scratch root. Excluded from git via the workspace-level
/// `.gitignore` rule for `crates/**/tests/.scratch/`.
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

/// Make a directory that's a fresh jj repo with no `bugs` bookmark.
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

/// Make a directory that's a fresh jj repo AND has `bugs` bookmarked
/// (so subsequent `jjf new` calls pass the preflight). The pattern most
/// of these tests want.
fn make_initialized_repo(name: &str) -> PathBuf {
    let repo = make_jj_repo(name);
    let out = Command::new(JJF_BIN)
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

/// Run `jjf <args...>` in `cwd` with no stdin, capture exit/stdout/stderr.
fn run_jjf(cwd: &Path, args: &[&str]) -> Output {
    Command::new(JJF_BIN)
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
    }

/// Run `jjf <args...>` in `cwd`, piping `stdin_bytes` into stdin.
fn run_jjf_with_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> Output {
    let mut child = Command::new(JJF_BIN)
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

/// Parse the stdout of a successful `jjf new` invocation (plain-text
/// mode: a single line containing the new bug's id) into a `BugId`.
fn parse_id_from_stdout(stdout: &[u8]) -> BugId {
    let line = String::from_utf8_lossy(stdout);
    let trimmed = line.trim();
    BugId::parse(trimmed)
        .unwrap_or_else(|e| panic!("stdout {:?} is not a valid BugId: {e}", trimmed))
}

// --- tests ---------------------------------------------------------

#[test]
fn new_happy_path_reads_stdin_and_round_trips_via_storage() {
    let repo = make_initialized_repo("new_happy");

    let body = "Reproduce by running `foo --bar`.\nExpected X, got Y.\n";
    let out = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "kernel panic on boot", "-F", "-"],
        body.as_bytes(),
    );
    assert!(
        out.status.success(),
        "jjf new failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let id = parse_id_from_stdout(&out.stdout);

    // Read back via the storage crate directly — this proves the bug
    // landed on the bookmark and that every field round-trips.
    let storage = Storage::open(&repo).expect("open storage on test repo");
    let bug = storage.read(&id).expect("read freshly-created bug");
    assert_eq!(bug.title, "kernel panic on boot");
    assert_eq!(bug.body, body);
    assert!(bug.labels.is_empty());
    assert!(bug.dependencies.is_empty());
    assert!(bug.assignee.is_none());
}

#[test]
fn new_json_emits_expected_object_and_id_parses() {
    let repo = make_initialized_repo("new_json");

    let out = run_jjf_with_stdin(
        &repo,
        &["new", "--json", "-t", "shape pin", "-F", "-"],
        b"body via stdin\n",
    );
    assert!(
        out.status.success(),
        "jjf new --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);

    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("new --json output should be valid JSON");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    let id_str = v["id"]
        .as_str()
        .expect("`id` field must be a string");
    let id = BugId::parse(id_str).expect("`id` should be a valid BugId");

    // Sanity: the id should be readable via the storage crate.
    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&id).expect("read bug");
    assert_eq!(bug.title, "shape pin");
}

#[test]
fn new_without_jjf_init_first_exits_two_with_run_jjf_init_first_message() {
    // Fresh jj repo, but no `bugs` bookmark — we expect a typed
    // preflight error, NOT the raw jj stderr we'd get from trying to
    // write against an empty `bookmarks(bugs)` revset.
    let repo = make_jj_repo("new_no_bookmark");

    let out = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "should fail", "-F", "-"],
        b"never written",
    );
    assert!(!out.status.success());
    assert_eq!(
        out.status.code(),
        Some(2),
        "missing-bookmark preflight should exit 2, got {:?}; stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`bugs` bookmark") && stderr.contains("jjf init"),
        "stderr should tell the user to run `jjf init` first, got: {stderr}"
    );
}

#[test]
fn new_in_non_jj_directory_exits_two_with_not_a_jj_repo_message() {
    // Not a jj repo at all — the preflight should produce the same
    // `not a jj repo` signal `jjf init` produces in this situation.
    let dir = scratch("new_non_jj");

    let out = run_jjf_with_stdin(&dir, &["new", "-t", "x", "-F", "-"], b"");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a jj repo"),
        "stderr should mention `not a jj repo`, got: {stderr}"
    );
}

#[test]
fn new_with_bogus_dep_id_exits_two_and_does_not_write() {
    let repo = make_initialized_repo("new_bad_dep");

    // -l a -l b are valid; the bogus dep id is the failure point. We
    // assert exit 2 and that NO bug was created (the bookmark stays
    // exactly where init left it).
    let bookmark_before = bookmark_tip_commit(&repo);

    let out = run_jjf_with_stdin(
        &repo,
        &[
            "new",
            "-t",
            "title",
            "-F",
            "-",
            "-l",
            "a",
            "-l",
            "b",
            "-d",
            "not-a-real-bug-id",
        ],
        b"body",
    );
    assert!(!out.status.success());
    assert_eq!(
        out.status.code(),
        Some(2),
        "bad dep id should exit 2; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--dep"),
        "stderr should mention which flag failed, got: {stderr}"
    );
    assert!(
        stderr.contains("not-a-real-bug-id"),
        "stderr should echo the bad value, got: {stderr}"
    );

    let bookmark_after = bookmark_tip_commit(&repo);
    assert_eq!(
        bookmark_before, bookmark_after,
        "preflight failure must NOT advance the `bugs` bookmark"
    );
}

#[test]
fn new_full_field_round_trip() {
    let repo = make_initialized_repo("new_full");

    // First: create a "real" bug so we have a valid id to use as a dep
    // on the second bug. Tests the chain: new + read-back + dep-link.
    let first = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "first", "-F", "-"],
        b"first body",
    );
    assert!(
        first.status.success(),
        "first jjf new failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_id = parse_id_from_stdout(&first.stdout);

    // Now: a bug with title, body via stdin, two labels, the first
    // bug as a dep, and an assignee. Every field should round-trip.
    let body = "multi-line\nbody\nwith trailing newline\n";
    let second = run_jjf_with_stdin(
        &repo,
        &[
            "new",
            "-t",
            "second with everything",
            "-F",
            "-",
            "-l",
            "bug",
            "-l",
            "p1",
            "-d",
            first_id.as_str(),
            "-a",
            "alice",
        ],
        body.as_bytes(),
    );
    assert!(
        second.status.success(),
        "second jjf new failed: code={:?} stderr={}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr),
    );
    let second_id = parse_id_from_stdout(&second.stdout);
    assert_ne!(
        second_id, first_id,
        "two distinct creates must yield distinct ids"
    );

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&second_id).expect("read second bug");
    assert_eq!(bug.title, "second with everything");
    assert_eq!(bug.body, body);
    // Storage sorts labels — the writer guarantees that, but we assert
    // on the (sorted) shape so a future reorder can't slip past.
    assert_eq!(bug.labels, vec!["bug".to_string(), "p1".to_string()]);
    assert_eq!(bug.dependencies, vec![first_id]);
    assert_eq!(bug.assignee.as_deref(), Some("alice"));
    assert!(bug.comments.is_empty());
}

#[test]
fn new_with_no_file_flag_creates_bug_with_empty_body() {
    // Per the epic's "no prompts ever" rule, omitting `-F` means an
    // empty body — NOT a launched editor.
    let repo = make_initialized_repo("new_no_file");

    let out = run_jjf(&repo, &["new", "-t", "empty body bug"]);
    assert!(
        out.status.success(),
        "jjf new with no -F failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let id = parse_id_from_stdout(&out.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&id).expect("read bug");
    assert_eq!(bug.title, "empty body bug");
    assert_eq!(bug.body, "");
}

#[test]
fn new_with_file_flag_reads_path_not_stdin() {
    // `-F <path>` should read the file's bytes, ignoring stdin (we
    // pipe garbage on stdin to prove it's ignored).
    let repo = make_initialized_repo("new_file_path");
    let body_path = repo.join("body.md");
    fs::write(&body_path, "from file, not stdin\n").unwrap();

    let out = run_jjf_with_stdin(
        &repo,
        &[
            "new",
            "-t",
            "file body",
            "-F",
            body_path.to_str().expect("utf-8 path"),
        ],
        b"this stdin should be ignored",
    );
    assert!(
        out.status.success(),
        "jjf new -F <path> failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let id = parse_id_from_stdout(&out.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&id).expect("read bug");
    assert_eq!(bug.body, "from file, not stdin\n");
}

// --- helpers -------------------------------------------------------

/// Capture the commit id of the `bugs` bookmark tip. Used by tests
/// that assert preflight failures don't advance the bookmark.
fn bookmark_tip_commit(repo: &Path) -> String {
    let out = Command::new("jj")
        .arg("--repository")
        .arg(repo)
        .args(["log", "-r", "bookmarks(bugs)", "-T", "commit_id ++ \"\\n\"", "--no-graph"])
        .output()
        .expect("spawn jj log");
    assert!(
        out.status.success(),
        "jj log failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}
