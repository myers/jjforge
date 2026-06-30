//! Integration tests for `jjf new` — drive the compiled binary against
//! per-test scratch repos and assert exit code, stderr, stdout (plain +
//! `--json`), and observable storage state via `Storage::read`.
//!
//! Mirrors `init.rs`: hermetic per-test scratch under `tests/.scratch/`,
//! wiped on each run, gitignored via `crates/**/tests/.scratch/`. No
//! `assert_cmd` dep — `CARGO_BIN_EXE_iss` + `std::process` is enough.

use std::fs;
use std::path::Path;
use std::process::Command;

use iss_storage::{DepEdge, DepKind, IssueId, Storage};

mod common;
use common::*;

/// Parse the stdout of a successful `jjf new` invocation (plain-text
/// mode: a single line containing the new bug's id) into a `IssueId`.
fn parse_id_from_plain_stdout(stdout: &[u8]) -> IssueId {
    let line = String::from_utf8_lossy(stdout);
    let trimmed = line.trim();
    IssueId::parse(trimmed)
        .unwrap_or_else(|e| panic!("stdout {:?} is not a valid IssueId: {e}", trimmed))
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
    let id = parse_id_from_plain_stdout(&out.stdout);

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
    let id = IssueId::parse(id_str).expect("`id` should be a valid IssueId");

    // Sanity: the id should be readable via the storage crate.
    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&id).expect("read bug");
    assert_eq!(bug.title, "shape pin");
}

#[test]
fn new_json_error_envelope_on_missing_bookmark() {
    // Fresh jj repo, no `jjf init` yet. With `--json` we expect the
    // documented `missing_issues_bookmark` envelope on stderr — the same
    // contract a script wrapping `jjf new` to file bugs would parse.
    let repo = make_jj_repo("new_json_err_no_bookmark");

    let out = run_jjf_with_stdin(
        &repo,
        &["--json", "new", "-t", "should fail", "-F", "-"],
        b"never written",
    );
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "stdout should be empty on error, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("missing_issues_bookmark"),
        "kind wrong: {stderr}"
    );
    // details.path should echo the repo dir so a caller can confirm
    // which working tree the failure came from.
    assert_eq!(
        v["error"]["details"]["path"].as_str(),
        Some(repo.to_string_lossy().as_ref()),
        "details.path wrong: {stderr}"
    );
}

#[test]
fn new_without_jjf_init_first_exits_two_with_run_jjf_init_first_message() {
    // Fresh jj repo, but no `issues` bookmark — we expect a typed
    // preflight error, NOT the raw jj stderr we'd get from trying to
    // write against an empty `bookmarks(issues)` revset.
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
        stderr.contains("`issues` bookmark") && stderr.contains("jjf init"),
        "stderr should tell the user to run `jjf init` first, got: {stderr}"
    );
}

#[test]
fn new_in_non_jj_directory_exits_two_with_not_a_jj_repo_message() {
    // Not a jj repo at all — the preflight should produce the same
    // `not a jj repo` signal `jjf init` produces in this situation.
    let dir = scratch_non_git("new_non_jj");

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
    // assert exit 2 and that NO issue ref was created.
    let refs_before = issue_ref_count(&repo);

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

    let refs_after = issue_ref_count(&repo);
    assert_eq!(
        refs_before, refs_after,
        "preflight failure must NOT create a new refs/jjf/issues/ ref"
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
    let first_id = parse_id_from_plain_stdout(&first.stdout);

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
    let second_id = parse_id_from_plain_stdout(&second.stdout);
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
    assert_eq!(bug.dependencies, vec![DepEdge::blocks(first_id)]);
    assert_eq!(bug.assignee.as_deref(), Some("alice"));
    assert!(bug.comments.is_empty());
}

#[test]
fn new_with_parent_flag_creates_parent_child_edge() {
    // Forgejo #3: `jjf new --dep <id>` hardcoded a `blocks` edge,
    // making child-of-epic creation a 3-step dance. `--parent <id>`
    // is the shorthand: one flag → one `parent-child` edge.
    let repo = make_initialized_repo("new_parent_flag");

    let epic = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "Epic: demo", "--type", "epic", "-F", "-"],
        b"epic body",
    );
    assert!(epic.status.success(), "epic create failed: {}", String::from_utf8_lossy(&epic.stderr));
    let epic_id = parse_id_from_plain_stdout(&epic.stdout);

    let child = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "child task", "--parent", epic_id.as_str(), "-F", "-"],
        b"child body",
    );
    assert!(
        child.status.success(),
        "child create failed: code={:?} stderr={}",
        child.status.code(),
        String::from_utf8_lossy(&child.stderr),
    );
    let child_id = parse_id_from_plain_stdout(&child.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&child_id).expect("read child");
    assert_eq!(
        bug.dependencies,
        vec![DepEdge::new(epic_id, DepKind::ParentChild)],
        "`--parent <id>` must land a single parent-child edge"
    );
}

#[test]
fn new_with_parent_and_dep_compose() {
    // `--parent` and `-d` compose into one dep list with mixed kinds.
    let repo = make_initialized_repo("new_parent_plus_dep");

    let epic = run_jjf_with_stdin(&repo, &["new", "-t", "Epic", "--type", "epic", "-F", "-"], b"");
    let epic_id = parse_id_from_plain_stdout(&epic.stdout);
    let blocker = run_jjf_with_stdin(&repo, &["new", "-t", "blocker", "-F", "-"], b"");
    let blocker_id = parse_id_from_plain_stdout(&blocker.stdout);

    let child = run_jjf_with_stdin(
        &repo,
        &[
            "new", "-t", "child", "-F", "-",
            "--parent", epic_id.as_str(),
            "-d", blocker_id.as_str(),
        ],
        b"",
    );
    assert!(
        child.status.success(),
        "compose failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let child_id = parse_id_from_plain_stdout(&child.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&child_id).expect("read child");
    // Storage sorts `dependencies` by (target, kind) — compare as a
    // multiset rather than assuming insertion order.
    let mut got = bug.dependencies.clone();
    got.sort();
    let mut want = vec![
        DepEdge::new(epic_id, DepKind::ParentChild),
        DepEdge::blocks(blocker_id),
    ];
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn new_with_bogus_parent_id_exits_two() {
    // `--parent` reuses the same id validator as `-d`. Bad id is exit 2.
    let repo = make_initialized_repo("new_bad_parent");
    let out = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "title", "-F", "-", "--parent", "not-hex!"],
        b"",
    );
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--parent") || stderr.contains("parent") || stderr.contains("not-hex!"),
        "stderr should mention which flag failed, got: {stderr}"
    );
}

#[test]
fn new_with_parent_slug_resolves_to_id() {
    // Forgejo b417864: `jjf new --parent <slug>` should resolve the slug
    // to a 7-hex id the same way `jjf ls`/`ready`/`search` do, instead
    // of rejecting with `bad_id`.
    let repo = make_initialized_repo("new_parent_slug");

    // Create an epic with a known slug.
    let epic = run_jjf_with_stdin(
        &repo,
        &[
            "new", "-t", "Epic: demo", "--type", "epic",
            "--slug", "agent-ergonomics", "-F", "-",
        ],
        b"epic body",
    );
    assert!(
        epic.status.success(),
        "epic create failed: {}",
        String::from_utf8_lossy(&epic.stderr)
    );
    let epic_id = parse_id_from_plain_stdout(&epic.stdout);

    // Now create a child using the slug — must succeed and land the
    // parent-child edge to the resolved id.
    let child = run_jjf_with_stdin(
        &repo,
        &["new", "-t", "child task", "--parent", "agent-ergonomics", "-F", "-"],
        b"child body",
    );
    assert!(
        child.status.success(),
        "child via slug failed: code={:?} stderr={}",
        child.status.code(),
        String::from_utf8_lossy(&child.stderr),
    );
    let child_id = parse_id_from_plain_stdout(&child.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&child_id).expect("read child");
    assert_eq!(
        bug.dependencies,
        vec![DepEdge::new(epic_id, DepKind::ParentChild)],
        "`--parent <slug>` must resolve to a parent-child edge to the slug's id"
    );
}

#[test]
fn new_with_unknown_parent_slug_exits_two_slug_not_found() {
    // Non-hex handle with no matching slug surfaces as `slug_not_found`
    // (exit 2), matching the resolver semantics used by `ls`/`ready`.
    let repo = make_initialized_repo("new_parent_slug_missing");
    let out = run_jjf_with_stdin(
        &repo,
        &[
            "new", "--json", "-t", "title", "-F", "-",
            "--parent", "no-such-slug",
        ],
        b"",
    );
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    // The `--json` error envelope goes to stderr (see `report_error`).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("slug_not_found"),
        "json envelope should report slug_not_found, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("no-such-slug"),
        "json envelope should include the bad handle, got stderr: {stderr}"
    );
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
    let id = parse_id_from_plain_stdout(&out.stdout);

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
    let id = parse_id_from_plain_stdout(&out.stdout);

    let storage = Storage::open(&repo).expect("open storage");
    let bug = storage.read(&id).expect("read bug");
    assert_eq!(bug.body, "from file, not stdin\n");
}

#[test]
fn new_meta_seeds_metadata_atomically() {
    // `jjf new --meta k=v` must seed metadata in the same create-time
    // multi-op commit so the issue arrives with metadata already
    // populated — no second mutation needed.
    let repo = make_initialized_repo("new_meta_seeds");
    let out = run_jjf(
        &repo,
        &[
            "new",
            "-t",
            "test meta",
            "--meta",
            "gc.routed_to=worker-1",
            "--meta",
            "team=infra",
            "-F",
            "/dev/null",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "new --meta should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    let id_str = envelope["id"].as_str().expect("envelope must have 'id'");

    // Verify metadata is present immediately via show --json, proving
    // it was seeded atomically at create time.
    let show = run_jjf(&repo, &["show", id_str, "--json"]);
    assert!(
        show.status.success(),
        "jjf show --json failed: {}",
        String::from_utf8_lossy(&show.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&show.stdout))
            .expect("show --json must be valid JSON");
    assert_eq!(
        parsed["metadata"]["gc.routed_to"],
        "worker-1",
        "gc.routed_to metadata missing or wrong"
    );
    assert_eq!(
        parsed["metadata"]["team"],
        "infra",
        "team metadata missing or wrong"
    );
}

#[test]
fn new_meta_duplicate_key_last_wins() {
    // `--meta k=v1 --meta k=v2` → k=v2 (BTreeMap last-wins semantics).
    let repo = make_initialized_repo("new_meta_dup");
    let out = run_jjf(
        &repo,
        &[
            "new",
            "-t",
            "dup meta",
            "--meta",
            "key=first",
            "--meta",
            "key=second",
            "-F",
            "/dev/null",
            "--json",
        ],
    );
    assert!(
        out.status.success(),
        "new --meta dup should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    let id_str = envelope["id"].as_str().expect("envelope must have 'id'");

    let show = run_jjf(&repo, &["show", id_str, "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&show.stdout))
            .expect("show --json must be valid JSON");
    assert_eq!(
        parsed["metadata"]["key"],
        "second",
        "duplicate key should resolve to last value"
    );
}

// --- helpers -------------------------------------------------------

/// Count the `refs/jjf/issues/*` refs in the repo. Used by tests that
/// assert preflight failures don't create a new issue ref.
/// Replaces the v2 `jj log bookmarks(issues)` tip-capture (J7: no jj).
fn issue_ref_count(repo: &Path) -> usize {
    let out = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/jjf/issues/"])
        .current_dir(repo)
        .output()
        .expect("spawn git for-each-ref");
    assert!(
        out.status.success(),
        "git for-each-ref failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
}
