//! Storage-level unit tests for `Storage::stale` — issues whose
//! `updated_at` is older than a threshold.
//!
//! Mirrors the hermetic-scratch style of `search.rs` / `integration.rs`:
//! a per-test directory under `tests/.scratch/`, wiped on each run,
//! gitignored. Tests pin the wall clock via `JJF_TEST_CLOCK_SECS` so
//! age math is deterministic regardless of suite parallelism. Each
//! nextest test runs in its own process, so the env var here does
//! NOT leak to siblings — see the `read_history_walks_same_second_*`
//! test in `integration.rs` for the same pattern.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use iss_storage::{IssueDraft, Storage, UpdateFields};

// --- bootstrap (mirror search.rs / integration.rs) -----------------

fn make_scratch_repo(name: &str) -> PathBuf {
    let abs = make_empty_jj_repo(name);
    Storage::init(&abs).expect("Storage::init must plant the v3 sentinel");
    abs
}

fn make_empty_jj_repo(name: &str) -> PathBuf {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(name);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    let abs = fs::canonicalize(&scratch).unwrap();
    // J7: plain git init — no jj required.
    sh("git", &["init"], &abs);
    sh("git", &["config", "user.name", "jjforge test"], &abs);
    sh("git", &["config", "user.email", "test@jjforge.invalid"], &abs);
    abs
}

fn sh(prog: &str, args: &[&str], cwd: &Path) {
    let out = Command::new(prog).args(args).current_dir(cwd).output().unwrap();
    assert!(
        out.status.success(),
        "`{prog} {}` failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Pin the wall clock to a specific epoch-second value. Used by
/// every test in this file to make age math deterministic. Each
/// nextest test runs in its own process; the env var doesn't leak.
fn pin_clock(secs: u64) {
    // SAFETY: single-threaded test process; nextest gives each test
    // its own process so we don't race siblings on this env var.
    unsafe {
        std::env::set_var("JJF_TEST_CLOCK_SECS", secs.to_string());
    }
}

// One day in seconds; threshold conversions throughout the tests.
const DAY: u64 = 86_400;

// --- tests: Storage::stale -----------------------------------------

#[test]
fn stale_empty_repo_returns_empty() {
    pin_clock(1_800_000_000);
    let repo = make_scratch_repo("stale_empty_repo");
    let storage = Storage::open(&repo).unwrap();

    // No issues, no stale rows.
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn stale_only_old_issues_returned() {
    // Bedrock case: one fresh issue created at `now`, one old issue
    // whose `updated_at` lands 100 days back. With `--days 14`, only
    // the old one comes back.
    pin_clock(1_000_000_000);
    let repo = make_scratch_repo("stale_only_old");
    let storage = Storage::open(&repo).unwrap();

    // Create the "old" issue at the early clock.
    pin_clock(1_000_000_000 - 100 * DAY);
    let old = storage
        .create_issue(&IssueDraft {
            title: "old issue".into(),
            ..Default::default()
        })
        .unwrap();

    // Advance the clock; create the fresh issue.
    pin_clock(1_000_000_000);
    let fresh = storage
        .create_issue(&IssueDraft {
            title: "fresh issue".into(),
            ..Default::default()
        })
        .unwrap();

    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert_eq!(hits.len(), 1, "only the 100d-old issue should be stale");
    assert_eq!(hits[0].issue.id, old);
    assert_ne!(hits[0].issue.id, fresh);
    // Age is `now - updated_at`. updated_at was stamped at the
    // pinned-back clock; now is `1_000_000_000`.
    assert_eq!(hits[0].seconds_since_update, 100 * DAY);
}

#[test]
fn stale_boundary_is_strict_greater_than() {
    // `updated_at` exactly at the threshold tick is NOT stale.
    // Documented contract on `Storage::stale`.
    pin_clock(2_000_000_000 - 14 * DAY);
    let repo = make_scratch_repo("stale_boundary");
    let storage = Storage::open(&repo).unwrap();

    // Create at `now - 14 days`.
    storage
        .create_issue(&IssueDraft {
            title: "right at boundary".into(),
            ..Default::default()
        })
        .unwrap();

    // Jump forward exactly 14 days. Age == 14 days == threshold.
    pin_clock(2_000_000_000);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert!(
        hits.is_empty(),
        "boundary-aged issue must NOT be stale (strict > semantics)"
    );

    // One second past the threshold IS stale.
    pin_clock(2_000_000_000 + 1);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].seconds_since_update, 14 * DAY + 1);
}

#[test]
fn stale_sort_oldest_first() {
    // Sort contract: stale issues come back ascending by
    // `updated_at` (oldest first). Build three issues at distinct
    // pinned timestamps and confirm the order.
    let now = 3_000_000_000u64;

    // Oldest.
    pin_clock(now - 90 * DAY);
    let repo = make_scratch_repo("stale_sort");
    let storage = Storage::open(&repo).unwrap();
    let oldest = storage
        .create_issue(&IssueDraft {
            title: "ninety-day-old".into(),
            ..Default::default()
        })
        .unwrap();

    // Middle.
    pin_clock(now - 60 * DAY);
    let middle = storage
        .create_issue(&IssueDraft {
            title: "sixty-day-old".into(),
            ..Default::default()
        })
        .unwrap();

    // Newest of the stale trio (still stale at the 14-day window).
    pin_clock(now - 30 * DAY);
    let newest = storage
        .create_issue(&IssueDraft {
            title: "thirty-day-old".into(),
            ..Default::default()
        })
        .unwrap();

    pin_clock(now);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].issue.id, oldest);
    assert_eq!(hits[1].issue.id, middle);
    assert_eq!(hits[2].issue.id, newest);
    // Ascending by age in seconds means age values monotonically
    // DECREASE (oldest carries the largest age). Sanity check.
    assert!(hits[0].seconds_since_update > hits[1].seconds_since_update);
    assert!(hits[1].seconds_since_update > hits[2].seconds_since_update);
}

#[test]
fn stale_comment_bumps_updated_at_today() {
    // Snapshot the ACTUAL behavior of `add_comment` today: it bumps
    // `updated_at`. The `host-asterinas-stale` ticket spec asserts
    // the opposite ("comments don't bump `updated_at` today"), but
    // reading `Storage::add_comment_once` shows the bump landing on
    // the record (`record.updated_at = now_rfc3339()?` — see
    // `crates/jjf-storage/src/lib.rs` near the `comment-add` commit
    // dance).
    //
    // This test pins that observed behavior so a future change to
    // `add_comment` (intentional or accidental) trips the alarm.
    // The ticket's "Out of scope" stale-by-activity caveat is a
    // documentation mismatch with the running code, not a contract
    // change `jjf stale` should make on its own — surfacing in the
    // closing comment under Open follow-ups.
    let now = 4_000_000_000u64;

    pin_clock(now - 30 * DAY);
    let repo = make_scratch_repo("stale_comment_bumps");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "old issue".into(),
            ..Default::default()
        })
        .unwrap();

    // Advance clock; drop a comment. The comment-add bumps
    // `updated_at` to `now` — making the issue fresh again as far
    // as `jjf stale --days 14` is concerned.
    pin_clock(now);
    storage
        .add_comment(&id, "fresh comment", "alice <a@x>")
        .unwrap();

    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert!(
        hits.is_empty(),
        "today's behavior: add_comment bumps updated_at, so the issue is no longer stale. \
         If this assertion fires, `Storage::add_comment_once` may have changed — reconcile \
         with the `host-asterinas-stale` ticket's caveat before adjusting."
    );
}

#[test]
fn stale_update_bumps_updated_at() {
    // Symmetric to the comment-doesn't-bump test: confirm that a
    // mutating verb DOES bump `updated_at`, taking the issue out of
    // the stale set. This is the contract that makes `jjf stale`
    // useful — a recently-renamed issue isn't stale.
    let now = 5_000_000_000u64;

    pin_clock(now - 30 * DAY);
    let repo = make_scratch_repo("stale_update_bumps");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "old title".into(),
            ..Default::default()
        })
        .unwrap();

    // Advance clock; rename. `set_title` is a mutating verb so it
    // bumps `updated_at` to `now`.
    pin_clock(now);
    storage.set_title(&id, "new title").unwrap();

    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert!(
        hits.is_empty(),
        "set_title bumps updated_at; issue should not be stale"
    );
}

#[test]
fn stale_threshold_zero_returns_everything() {
    // `threshold_secs = 0` means "anything older than 0 seconds".
    // The `now - updated_at` delta on a just-created issue is some
    // positive integer (the function records `updated_at` then
    // re-reads `now` later when we call `stale`); they share the
    // same pinned-second though, so the delta is 0 — and 0 is NOT
    // > 0, so a same-second issue is not stale. Confirm with one
    // already-old issue.
    let now = 6_000_000_000u64;

    pin_clock(now - 1);
    let repo = make_scratch_repo("stale_threshold_zero");
    let storage = Storage::open(&repo).unwrap();
    storage
        .create_issue(&IssueDraft {
            title: "one second old".into(),
            ..Default::default()
        })
        .unwrap();

    pin_clock(now);
    let hits = storage.stale(0, &[]).unwrap();
    assert_eq!(hits.len(), 1, "1s-old issue must be stale at threshold 0s");
    assert_eq!(hits[0].seconds_since_update, 1);
}

#[test]
fn stale_future_updated_at_not_stale() {
    // Defensive: a peer with a fast clock may push an issue with a
    // future-dated `updated_at`. `saturating_sub` keeps the math
    // safe; the issue surfaces as `seconds_since_update = 0`, never
    // > threshold for any positive threshold — so it's not stale.
    // Carrying this contract in a test guards against an accidental
    // signed-subtraction regression.
    pin_clock(7_000_000_000);
    let repo = make_scratch_repo("stale_future_dated");
    let storage = Storage::open(&repo).unwrap();
    // Create at "now"; then jump backward, simulating the local
    // clock now being earlier than the issue's `updated_at`.
    storage
        .create_issue(&IssueDraft {
            title: "future-dated".into(),
            ..Default::default()
        })
        .unwrap();

    // Local clock is now BEFORE the issue's updated_at.
    pin_clock(7_000_000_000 - 100 * DAY);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert!(
        hits.is_empty(),
        "future-dated issue must never be stale (saturating_sub guard)"
    );
}

#[test]
fn stale_threshold_in_seconds_not_days() {
    // The storage API takes seconds, not days. Confirm the unit by
    // setting a tight threshold (one hour) and seeing that an issue
    // updated two hours ago surfaces, but one updated thirty
    // minutes ago does not.
    let now = 8_000_000_000u64;

    pin_clock(now - 2 * 3600);
    let repo = make_scratch_repo("stale_seconds_unit");
    let storage = Storage::open(&repo).unwrap();
    let two_hr_old = storage
        .create_issue(&IssueDraft {
            title: "two hours old".into(),
            ..Default::default()
        })
        .unwrap();

    pin_clock(now - 30 * 60);
    storage
        .create_issue(&IssueDraft {
            title: "half hour old".into(),
            ..Default::default()
        })
        .unwrap();

    pin_clock(now);
    let hits = storage.stale(3600, &[]).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].issue.id, two_hr_old);
}

#[test]
fn stale_returns_issue_record_intact() {
    // Sanity: the StaleHit carries the FULL `Issue` projection, same
    // shape `Storage::read` returns. Specifically, labels,
    // dependencies, comments etc. are preserved so the CLI can
    // compose filters without a second read pass.
    let now = 9_000_000_000u64;

    pin_clock(now - 30 * DAY);
    let repo = make_scratch_repo("stale_full_projection");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "labeled".into(),
            labels: vec!["epic:host-asterinas".into(), "p1".into()],
            ..Default::default()
        })
        .unwrap();

    pin_clock(now);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.issue.id, id);
    assert_eq!(hit.issue.title, "labeled");
    assert_eq!(hit.issue.labels.len(), 2);
    assert!(hit.issue.labels.iter().any(|l| l == "epic:host-asterinas"));
    assert!(hit.issue.labels.iter().any(|l| l == "p1"));
}

#[test]
fn stale_closed_issues_included_storage_layer() {
    // The storage layer doesn't filter by status — that's the CLI's
    // job (`--status open` is the default; closed-included needs
    // `--status all`). Confirm `Storage::stale` returns a closed
    // issue if it's old enough.
    let now = 10_000_000_000u64;

    pin_clock(now - 60 * DAY);
    let repo = make_scratch_repo("stale_closed_inc");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "got closed long ago".into(),
            ..Default::default()
        })
        .unwrap();

    // Close at the same time (mutating verb — bumps updated_at to
    // the same pinned tick).
    storage
        .update(
            &id,
            UpdateFields {
                status: Some(iss_storage::Status::Closed),
                ..Default::default()
            },
        )
        .unwrap();

    pin_clock(now);
    let hits = storage.stale(14 * DAY, &[]).unwrap();
    assert_eq!(
        hits.len(),
        1,
        "storage layer returns the closed issue; CLI does status filtering"
    );
    assert_eq!(hits[0].issue.id, id);
    assert_eq!(hits[0].issue.status, iss_storage::Status::Closed);
}
