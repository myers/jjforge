//! Integration tests for `iss stale` — drive the compiled binary
//! against per-test scratch repos and assert exit code, stdout
//! (plain + `--json`), the filter matrix, age rendering, and the
//! limit cap. Pins the wall clock via `ISS_TEST_CLOCK_SECS` per
//! subprocess so age math is deterministic.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as `search.rs`
//! and `ls.rs`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

mod common;
use common::{make_initialized_repo, ISS_BIN};

fn run_jjf_with_env(cwd: &Path, args: &[&str], clock_secs: u64) -> Output {
    Command::new(ISS_BIN)
        .args(args)
        .env("ISS_TEST_CLOCK_SECS", clock_secs.to_string())
        .current_dir(cwd)
        .output()
        .expect("spawn jjf")
}

fn run_jjf_with_stdin_and_env(
    cwd: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
    clock_secs: u64,
) -> Output {
    let mut child = Command::new(ISS_BIN)
        .args(args)
        .env("ISS_TEST_CLOCK_SECS", clock_secs.to_string())
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

/// Create an issue at the given pinned wall-clock second. Returns
/// the resulting 7-char id.
fn create_issue_at(
    repo: &Path,
    title: &str,
    body: &[u8],
    extra_args: &[&str],
    clock_secs: u64,
) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra_args);
    let out = run_jjf_with_stdin_and_env(repo, &args, body, clock_secs);
    assert!(
        out.status.success(),
        "jjf new failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Parse `iss stale` plain-text rows into `(id, age, title, status)`.
fn parse_stale_rows(stdout: &str) -> Vec<(String, String, String, String)> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let parts: Vec<&str> = l.splitn(4, '\t').collect();
            assert_eq!(
                parts.len(),
                4,
                "expected 4 tab-separated columns, got {}: {l:?}",
                parts.len()
            );
            (
                parts[0].to_owned(),
                parts[1].to_owned(),
                parts[2].to_owned(),
                parts[3].to_owned(),
            )
        })
        .collect()
}

const DAY: u64 = 86_400;
const NOW: u64 = 1_800_000_000; // 2027-01-15-ish; far enough out the math is obvious.

// --- tests ---------------------------------------------------------

#[test]
fn stale_default_days_14_returns_old_issues() {
    let repo = make_initialized_repo("stale_default_14d");
    // Old issue: 30 days back.
    let old = create_issue_at(&repo, "very old", b"x", &[], NOW - 30 * DAY);
    // Fresh issue: same second as "now".
    let _fresh = create_issue_at(&repo, "fresh", b"x", &[], NOW);

    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    assert!(
        out.status.success(),
        "stale failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_stale_rows(&stdout);
    assert_eq!(rows.len(), 1, "stdout: {stdout:?}");
    assert_eq!(rows[0].0, old);
}

#[test]
fn stale_days_flag_widens_window() {
    let repo = make_initialized_repo("stale_days_flag");
    create_issue_at(&repo, "five day", b"x", &[], NOW - 5 * DAY);
    create_issue_at(&repo, "twenty day", b"x", &[], NOW - 20 * DAY);

    // Default (14d): only the 20-day-old issue.
    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, "twenty day");

    // --days 3: both.
    let out = run_jjf_with_env(&repo, &["stale", "--days", "3"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2);

    // --days 999: neither.
    let out = run_jjf_with_env(&repo, &["stale", "--days", "999"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert!(rows.is_empty());
}

#[test]
fn stale_json_shape_is_bare_array() {
    let repo = make_initialized_repo("stale_json_shape");
    let old = create_issue_at(&repo, "old one", b"x", &[], NOW - 30 * DAY);

    let out = run_jjf_with_env(&repo, &["--json", "stale"], NOW);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout is valid JSON");
    // Bare array, NOT an envelope. Mirrors `ls --json`.
    assert!(v.is_array(), "expected bare array, got {v}");
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let row = &arr[0];
    assert_eq!(row["id"], serde_json::Value::String(old.clone()));
    assert_eq!(row["title"], serde_json::Value::String("old one".into()));
    assert_eq!(row["status"], serde_json::Value::String("open".into()));
    assert_eq!(row["days_since_update"], serde_json::Value::Number(30.into()));
    assert!(row["updated_at"].is_string());
}

#[test]
fn stale_json_empty_result_is_empty_array() {
    let repo = make_initialized_repo("stale_json_empty");
    // One fresh issue — no stale rows.
    create_issue_at(&repo, "fresh", b"x", &[], NOW);

    let out = run_jjf_with_env(&repo, &["--json", "stale"], NOW);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v, serde_json::Value::Array(vec![]));
}

#[test]
fn stale_plain_empty_result_is_silent() {
    let repo = make_initialized_repo("stale_plain_empty");
    create_issue_at(&repo, "fresh", b"x", &[], NOW);

    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim().is_empty(), "expected silence, got {stdout:?}");
}

#[test]
fn stale_default_status_open_excludes_closed() {
    // Default `--status open` — a closed-but-old issue should NOT
    // appear under the default invocation. (Mirror `ls`'s default.)
    let repo = make_initialized_repo("stale_default_status");
    let id = create_issue_at(&repo, "old, will close", b"x", &[], NOW - 30 * DAY);
    let _other = create_issue_at(&repo, "old, stays open", b"x", &[], NOW - 30 * DAY);

    // Close at the old timestamp; that bumps updated_at to NOW-30d
    // (same second; cluster commit).
    let out = run_jjf_with_env(&repo, &["close", &id], NOW - 30 * DAY);
    assert!(out.status.success());

    // Default `stale` — status open only — should hide the closed one.
    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1, "closed issue should be hidden under default status filter");
    assert_eq!(rows[0].3, "open");

    // `--status closed` shows only the closed one.
    let out = run_jjf_with_env(&repo, &["stale", "--status", "closed"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].3, "closed");
    assert_eq!(rows[0].0, id);

    // `--status all` shows both.
    let out = run_jjf_with_env(&repo, &["stale", "--status", "all"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2);
}

#[test]
fn stale_label_filter_composes() {
    let repo = make_initialized_repo("stale_label_filter");
    let labeled = create_issue_at(
        &repo,
        "old labeled",
        b"x",
        &["-l", "epic:host-asterinas"],
        NOW - 30 * DAY,
    );
    let _unlabeled = create_issue_at(&repo, "old unlabeled", b"x", &[], NOW - 30 * DAY);

    let out = run_jjf_with_env(
        &repo,
        &["stale", "--label", "epic:host-asterinas"],
        NOW,
    );
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, labeled);
}

#[test]
fn stale_limit_truncates() {
    let repo = make_initialized_repo("stale_limit");
    // Three stale issues at distinct ages so the sort is unambiguous.
    let _a = create_issue_at(&repo, "a90", b"x", &[], NOW - 90 * DAY);
    let _b = create_issue_at(&repo, "b60", b"x", &[], NOW - 60 * DAY);
    let _c = create_issue_at(&repo, "c30", b"x", &[], NOW - 30 * DAY);

    // --limit 2: oldest two come back.
    let out = run_jjf_with_env(&repo, &["stale", "--limit", "2"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].2, "a90"); // oldest first
    assert_eq!(rows[1].2, "b60");

    // --limit 0: unlimited (mirrors `search`).
    let out = run_jjf_with_env(&repo, &["stale", "--limit", "0"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 3);
}

#[test]
fn stale_age_render_days_form() {
    // 19 days renders as `19d`, NOT `1w 5d` or `~3w`. The renderer
    // emits a single token at every boundary.
    let repo = make_initialized_repo("stale_age_19d");
    create_issue_at(&repo, "nineteen days", b"x", &[], NOW - 19 * DAY);

    let out = run_jjf_with_env(&repo, &["stale", "--days", "7"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "19d");
}

#[test]
fn stale_age_render_weeks_form() {
    // >= 30d && < 90d → weeks (floor of days/7).
    let repo = make_initialized_repo("stale_age_weeks");
    create_issue_at(&repo, "thirty five days", b"x", &[], NOW - 35 * DAY);

    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "5w"); // 35/7 = 5
}

#[test]
fn stale_age_render_months_form() {
    // >= 90d → months (floor of days/30).
    let repo = make_initialized_repo("stale_age_months");
    create_issue_at(&repo, "one twenty days", b"x", &[], NOW - 120 * DAY);

    let out = run_jjf_with_env(&repo, &["stale"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "4mo"); // 120/30 = 4
}

#[test]
fn stale_filters_by_meta() {
    let repo = make_initialized_repo("stale_meta_filter");
    // Both issues are 30 days old (stale under the default 14d threshold).
    let id_a = create_issue_at(&repo, "issue-a", b"x", &[], NOW - 30 * DAY);
    let id_b = create_issue_at(&repo, "issue-b", b"x", &[], NOW - 30 * DAY);

    // Tag issue A with metadata at a past clock so `updated_at` stays
    // old — `metadata set` is a mutating verb and bumps `updated_at`
    // to the clock at call time. Pinning to the same past-timestamp
    // keeps A stale from `stale`'s perspective.
    let set_out = run_jjf_with_env(
        &repo,
        &["metadata", "set", &id_a, "team", "infra"],
        NOW - 30 * DAY,
    );
    assert!(
        set_out.status.success(),
        "metadata set failed: code={:?} stderr={}",
        set_out.status.code(),
        String::from_utf8_lossy(&set_out.stderr)
    );

    // `stale --meta team=infra` should include A, exclude B.
    let out = run_jjf_with_env(&repo, &["stale", "--days", "14", "--meta", "team=infra"], NOW);
    assert!(
        out.status.success(),
        "stale --meta should exit 0; code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let rows = parse_stale_rows(&stdout);
    let ids: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert!(ids.contains(&id_a.as_str()), "should include id_a; got: {stdout}");
    assert!(!ids.contains(&id_b.as_str()), "should NOT include id_b; got: {stdout}");
}

#[test]
fn stale_type_filter_composes() {
    let repo = make_initialized_repo("stale_type_filter");
    let bug = create_issue_at(
        &repo,
        "old bug",
        b"x",
        &["--type", "bug"],
        NOW - 30 * DAY,
    );
    let _feature = create_issue_at(
        &repo,
        "old feature",
        b"x",
        &["--type", "feature"],
        NOW - 30 * DAY,
    );

    let out = run_jjf_with_env(&repo, &["stale", "--type", "bug"], NOW);
    let rows = parse_stale_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, bug);
}
