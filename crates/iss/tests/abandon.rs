//! Integration tests for `iss abandon <id>` — drive the compiled
//! binary against per-test scratch repos and assert exit code,
//! stdout (plain + `--json`), and the spec-mandated semantics:
//!
//! - `iss abandon` flips status to `abandoned` and emits the same
//!   envelope shape as `close`,
//! - `iss ls` (default `--status open`) does NOT show the
//!   abandoned issue,
//! - `iss ls --status all` DOES show it (with status `abandoned`),
//! - `iss ls --status abandoned` shows only abandoned ones,
//! - `iss ready` does NOT show it, even with
//!   `--include-blocked --include-claimed`,
//! - `iss show <id>` still works on abandoned issues,
//! - the JSON error envelope on `abandon <bad-id>` matches the
//!   shape `close` emits.
//!
//! Same hermetic-scratch / no-`assert_cmd` discipline as the other
//! test files in this crate; modeled on `close.rs`.

use std::path::Path;
use std::process::Command;

mod common;
use common::*;

/// Create an issue via `iss new`, return its id. `extra` is appended
/// after the title flag (e.g. `&["--slug", "junk"]`).
fn create_issue(repo: &Path, title: &str, extra: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", "-t", title, "-F", "-"];
    args.extend_from_slice(extra);
    let out = run_jjf_with_stdin(repo, &args, b"");
    assert!(
        out.status.success(),
        "jjf new failed during setup: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

// --- tests ---------------------------------------------------------

#[test]
fn abandon_happy_path_show_reports_abandoned() {
    let repo = make_initialized_repo("abandon_happy");
    let id = create_issue(&repo, "mis-filed", &[]);

    let out = run_jjf(&repo, &["abandon", &id]);
    assert!(
        out.status.success(),
        "jjf abandon failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Plain-text output mirrors `close`: `abandoned <id>`.
    assert_eq!(stdout.trim(), format!("abandoned {id}"));

    let out = run_jjf(&repo, &["show", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[abandoned]"),
        "show should report [abandoned] after jjf abandon: {stdout}"
    );
}

#[test]
fn abandon_json_envelope_shape() {
    let repo = make_initialized_repo("abandon_json");
    let id = create_issue(&repo, "json abandon", &[]);

    let out = run_jjf(&repo, &["abandon", "--json", &id]);
    assert!(
        out.status.success(),
        "jjf abandon --json failed: code={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("abandon --json output must be valid JSON");
    assert_eq!(v["ok"].as_bool(), Some(true), "ok field wrong: {stdout}");
    assert_eq!(
        v["id"].as_str(),
        Some(id.as_str()),
        "id field wrong: {stdout}"
    );
    assert_eq!(
        v["status"].as_str(),
        Some("abandoned"),
        "status field wrong: {stdout}"
    );

    // And the read path agrees.
    let out = run_jjf(&repo, &["show", "--json", &id]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(v["status"].as_str(), Some("abandoned"));
}

#[test]
fn ls_default_hides_abandoned_issues() {
    let repo = make_initialized_repo("abandon_ls_default_hides");
    let keep = create_issue(&repo, "keep me", &[]);
    let nuke = create_issue(&repo, "delete me", &[]);
    let out = run_jjf(&repo, &["abandon", &nuke]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ls"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&keep), "open issue must be listed: {stdout}");
    assert!(
        !stdout.contains(&nuke),
        "abandoned issue must NOT be listed by default: {stdout}"
    );
}

#[test]
fn ls_status_all_includes_abandoned() {
    let repo = make_initialized_repo("abandon_ls_status_all");
    let id = create_issue(&repo, "mis-filed", &[]);
    let out = run_jjf(&repo, &["abandon", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ls", "--status", "all"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&id),
        "abandoned issue must appear under --status all: {stdout}"
    );
    assert!(
        stdout.contains("abandoned"),
        "status column should read `abandoned`: {stdout}"
    );
}

#[test]
fn ls_status_abandoned_lists_only_abandoned() {
    let repo = make_initialized_repo("abandon_ls_status_abandoned");
    let keep = create_issue(&repo, "still open", &[]);
    let nuke = create_issue(&repo, "soft-deleted", &[]);
    let out = run_jjf(&repo, &["abandon", &nuke]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["ls", "--status", "abandoned"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&nuke),
        "abandoned issue must appear under --status abandoned: {stdout}"
    );
    assert!(
        !stdout.contains(&keep),
        "open issue must NOT appear under --status abandoned: {stdout}"
    );
}

#[test]
fn ready_excludes_abandoned_even_with_include_blocked_and_claimed() {
    // Abandoned has no `--include-abandoned` flag; it's gone for good.
    let repo = make_initialized_repo("abandon_ready_excludes");
    let nuke = create_issue(&repo, "delete me", &["--type", "feature"]);
    let keep = create_issue(&repo, "keep me", &["--type", "feature"]);
    let out = run_jjf(&repo, &["abandon", &nuke]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(
        &repo,
        &["ready", "--include-blocked", "--include-claimed"],
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&keep), "open issue must be ready: {stdout}");
    assert!(
        !stdout.contains(&nuke),
        "abandoned issue must NEVER appear in ready, regardless of flags: {stdout}"
    );
}

#[test]
fn show_still_works_on_abandoned_issue() {
    // We must be able to look at an abandoned issue (confirm what
    // was abandoned). Documented in the ticket body.
    let repo = make_initialized_repo("abandon_show_works");
    let id = create_issue(&repo, "what did I abandon?", &[]);
    let out = run_jjf(&repo, &["abandon", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["show", &id]);
    assert!(
        out.status.success(),
        "show on abandoned issue must succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("what did I abandon?"));
    assert!(stdout.contains("[abandoned]"));
}

#[test]
fn abandon_json_error_envelope_on_nonexistent_id() {
    // Same envelope shape as `close` / `open` — `run_set_status`
    // is shared, but pin the contract anyway so a future refactor
    // can't silently drift one verb.
    let repo = make_initialized_repo("abandon_json_err_missing");
    let nonexistent = "deadbee";

    let out = run_jjf(&repo, &["--json", "abandon", nonexistent]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
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
        Some("issue_not_found"),
        "kind wrong: {stderr}"
    );
    assert_eq!(
        v["error"]["details"]["id"].as_str(),
        Some(nonexistent),
        "details.id wrong: {stderr}"
    );
}

#[test]
fn abandon_bad_id_exits_two() {
    let repo = make_initialized_repo("abandon_bad_id");
    for bad in ["short", "GGGGGGG", "12345", "not-a-real-bug-id"] {
        let out = run_jjf(&repo, &["abandon", bad]);
        assert!(!out.status.success(), "abandon on {bad:?} should fail");
        assert_eq!(
            out.status.code(),
            Some(2),
            "bad id {bad:?} should exit 2, got {:?}; stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

#[test]
fn abandon_in_non_jj_directory_exits_two() {
    let dir = scratch_non_git("abandon_non_jj");
    let out = run_jjf(&dir, &["abandon", "abcdef0"]);
    assert!(!out.status.success(), "abandon in non-jj dir should fail");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn abandon_in_jj_repo_without_bugs_bookmark_exits_two_with_init_hint() {
    let repo = make_jj_repo("abandon_no_bookmark");
    let out = run_jjf(&repo, &["abandon", "abcdef0"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("`issues` bookmark") && stderr.contains("iss init"),
        "stderr should tell the user to run `iss init` first, got: {stderr}"
    );
}

#[test]
fn abandoning_does_not_release_slug() {
    // F-012 / spec §3.4: slug uniqueness spans every status,
    // including Abandoned. Creating a second issue with the
    // same slug must fail with `slug_collision`.
    let repo = make_initialized_repo("abandon_slug_kept");
    let id = create_issue(&repo, "first", &["--slug", "junk"]);
    let out = run_jjf(&repo, &["abandon", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Try to claim the slug again — must fail.
    let out = run_jjf_with_stdin(
        &repo,
        &["--json", "new", "-t", "second", "-F", "-", "--slug", "junk"],
        b"",
    );
    assert!(
        !out.status.success(),
        "second new with same slug must fail; stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let v: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("stderr must be valid JSON envelope");
    assert_eq!(v["ok"], serde_json::Value::Bool(false));
    assert_eq!(
        v["error"]["kind"].as_str(),
        Some("slug_collision"),
        "kind wrong: {stderr}"
    );
}

#[test]
fn update_status_open_revives_an_abandoned_issue() {
    // No `iss unabandon` inverse verb — the documented revive
    // path is `iss update <id> --status open`. Pin that.
    let repo = make_initialized_repo("abandon_revive_via_update");
    let id = create_issue(&repo, "revive me", &[]);
    let out = run_jjf(&repo, &["abandon", &id]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let out = run_jjf(&repo, &["update", &id, "--status", "open"]);
    assert!(
        out.status.success(),
        "update --status open must revive: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run_jjf(&repo, &["show", &id]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[open]"),
        "show should report [open] after revive: {stdout}"
    );
}

#[test]
fn abandon_help_documents_positional_and_json() {
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Command::new(ISS_BIN)
        .args(["abandon", "--help"])
        .current_dir(cwd)
        .output()
        .expect("spawn jjf abandon --help");
    assert!(out.status.success());
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("<ID>"), "abandon --help should document the <ID> positional: {help}");
    assert!(help.contains("--json"), "abandon --help should document --json: {help}");
}
