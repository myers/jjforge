//! Fixture-driven integration tests for `jjf-merge`.
//!
//! Fixtures are committed under `tests/fixtures/`. They are the
//! captured marker output from
//! `experiments/distributed-edit/runs/followup.transcript.txt`
//! (canonical concurrent-set-title scenario) plus a hand-built
//! pretty-printed v1 record per `docs/storage-format.md` §3.

use iss_merge::{MergeOptions, Side, resolve};
use serde_json::{Value, json};

#[test]
fn fixture_concurrent_title_resolves_to_chosen_side() {
    let text = include_str!("fixtures/concurrent_title.conflict");
    let opts = MergeOptions {
        prefer_side: Side::B,
        ..Default::default()
    };
    let out = resolve(text, &opts).expect("resolve");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["title"], json!("bob title"));
    assert_eq!(v["status"], json!("open"));
}

#[test]
fn fixture_concurrent_title_prefer_a() {
    let text = include_str!("fixtures/concurrent_title.conflict");
    let opts = MergeOptions {
        prefer_side: Side::A,
        ..Default::default()
    };
    let out = resolve(text, &opts).expect("resolve");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["title"], json!("alice title"));
}

#[test]
fn fixture_v1_record_pretty_full_policy() {
    let text = include_str!("fixtures/v1_record_pretty.conflict");
    let opts = MergeOptions {
        prefer_side: Side::B,
        ..Default::default()
    };
    let out = resolve(text, &opts).expect("resolve");
    let v: Value = serde_json::from_str(&out).expect("resolved is valid JSON");

    // Scalars: B wins under prefer_side=B.
    assert_eq!(v["title"], json!("bob title"));
    assert_eq!(v["status"], json!("closed"));
    assert_eq!(v["assignee"], json!("bob"));
    assert_eq!(v["updated_at"], json!("2026-06-21T15:00:00Z"));

    // Created_at is identical on both sides — idempotent.
    assert_eq!(v["created_at"], json!("2026-06-21T12:00:00Z"));

    // Labels: set-union ["bug", "p1"] ∪ ["bug", "regression"]
    //   = ["bug", "p1", "regression"]  (sorted by JSON serialization).
    assert_eq!(v["labels"], json!(["bug", "p1", "regression"]));

    // Dependencies: empty on both, stays empty.
    assert_eq!(v["dependencies"], json!([]));

    // Output ends with newline per docs/storage-format.md §3.
    assert!(out.ends_with('\n'));
}

#[test]
fn fixture_v1_record_pretty_prefer_a() {
    let text = include_str!("fixtures/v1_record_pretty.conflict");
    let opts = MergeOptions {
        prefer_side: Side::A,
        ..Default::default()
    };
    let out = resolve(text, &opts).expect("resolve");
    let v: Value = serde_json::from_str(&out).expect("resolved is valid JSON");
    assert_eq!(v["title"], json!("alice title"));
    assert_eq!(v["status"], json!("open"));
    assert_eq!(v["assignee"], json!("alice"));

    // Set-union policy ignores prefer_side — A or B doesn't matter.
    assert_eq!(v["labels"], json!(["bug", "p1", "regression"]));
}

#[test]
fn no_conflict_input_is_canonicalized() {
    let text = "{\"version\":1,\"id\":\"abc1234\",\"title\":\"x\"}\n";
    let out = resolve(text, &MergeOptions::default()).expect("resolve");
    // Pretty-printed output is multi-line.
    assert!(out.contains('\n'));
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["title"], json!("x"));
}
