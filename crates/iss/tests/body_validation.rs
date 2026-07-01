//! Integration tests for the body-cap boundary (issue `679444a`,
//! QA red-team 2026-06-25 sub-pass 4 C3). Drives the compiled `jjf`
//! binary against per-test scratch repos and asserts the typed
//! `body_too_large` rejection on `iss new -F`, `iss update
//! --body-file`, and `iss comment -F`.
//!
//! Background: pre-fix, `iss new -F <bigfile>` silently accepted a
//! multi-megabyte body, landing a fat commit with no declared
//! contract. The new cap matches GitHub's documented issue-body
//! limit — 65,536 bytes of raw UTF-8.
//!
//! Style mirrors `tests/title_validation.rs`.

use std::fs;
use std::path::{Path, PathBuf};

mod common;
use common::*;

/// The cap pinned by `jjf-storage::BODY_MAX_BYTES`. Re-declared
/// here so the CLI test fixtures don't need an extra crate
/// dependency — the value is part of the public CLI contract and
/// must not drift independently.
const BODY_MAX_BYTES: usize = 65_536;

/// Write a file of exactly `n` ASCII bytes and return its path.
fn write_body_file(dir: &Path, name: &str, n: usize) -> PathBuf {
    let path = dir.join(name);
    let body = "a".repeat(n);
    fs::write(&path, body).unwrap();
    path
}

// --- `iss new -F <too-large>` ----------------------------------------------

#[test]
fn new_rejects_oversize_body_with_typed_envelope() {
    let repo = make_initialized_repo("body_new_oversize");
    // One byte over the cap. The boundary is byte-exact; we test
    // the boundary itself rather than a giant body in the middle
    // of the range.
    let path = write_body_file(&repo, "big.md", BODY_MAX_BYTES + 1);
    let out = run_jjf(
        &repo,
        &["--json", "new", "-t", "demo", "-F", path.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "body_too_large");
    // `limit` and `got` are integers, not strings — scripts branch
    // on them directly.
    assert_eq!(v["error"]["details"]["limit"], BODY_MAX_BYTES);
    assert_eq!(v["error"]["details"]["got"], BODY_MAX_BYTES + 1);
}

#[test]
fn new_accepts_at_cap_body() {
    // Boundary NEGATIVE: a body of exactly BODY_MAX_BYTES lands.
    let repo = make_initialized_repo("body_new_at_cap");
    let path = write_body_file(&repo, "at-cap.md", BODY_MAX_BYTES);
    let out = run_jjf(
        &repo,
        &["new", "-t", "demo", "-F", path.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "at-cap body must be accepted; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- `iss update --body-file <too-large>` -----------------------------------

#[test]
fn update_rejects_oversize_body_file_with_typed_envelope() {
    let repo = make_initialized_repo("body_update_oversize");
    let create = run_jjf(&repo, &["new", "-t", "baseline"]);
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_owned();

    let path = write_body_file(&repo, "big-update.md", BODY_MAX_BYTES + 1);
    let out = run_jjf(
        &repo,
        &[
            "--json",
            "update",
            &id,
            "--body-file",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["error"]["kind"], "body_too_large");
    assert_eq!(v["error"]["details"]["limit"], BODY_MAX_BYTES);
    assert_eq!(v["error"]["details"]["got"], BODY_MAX_BYTES + 1);
}

// --- `iss comment -F <too-large>` -------------------------------------------

#[test]
fn comment_rejects_oversize_body_file_with_typed_envelope() {
    let repo = make_initialized_repo("body_comment_oversize");
    let create = run_jjf(&repo, &["new", "-t", "baseline"]);
    assert!(create.status.success());
    let id = String::from_utf8_lossy(&create.stdout).trim().to_owned();

    let path = write_body_file(&repo, "big-comment.md", BODY_MAX_BYTES + 1);
    let out = run_jjf(
        &repo,
        &["--json", "comment", &id, "-F", path.to_str().unwrap()],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = parse_envelope(&out.stderr);
    assert_eq!(v["error"]["kind"], "body_too_large");
    assert_eq!(v["error"]["details"]["limit"], BODY_MAX_BYTES);
    assert_eq!(v["error"]["details"]["got"], BODY_MAX_BYTES + 1);
}
