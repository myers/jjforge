//! Storage-level unit tests for `Storage::search` — case-insensitive
//! substring search across issue title/body/comment bodies, plus the
//! free-function `make_snippet` helper.
//!
//! Mirrors the hermetic-scratch style of `integration.rs`: a
//! per-test directory under `tests/.scratch/`, wiped on each run,
//! gitignored. The bootstrap helper plants a v3 sentinel via
//! `Storage::init`; `Storage::open` then opens the v3 repo directly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use iss_storage::{
    DEFAULT_SNIPPET_CONTEXT, IssueDraft, IssueType, MatchedField, Storage, make_snippet,
};

// --- bootstrap (mirror integration.rs) ------------------------------

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

// --- tests: Storage::search ----------------------------------------

#[test]
fn search_empty_query_returns_no_results() {
    // Empty query is match-nothing. The match-everything case is
    // `Storage::list_ids`'s job, not `search`'s — see the contract
    // doc-comment on `Storage::search`.
    let repo = make_scratch_repo("search_empty_query");
    let storage = Storage::open(&repo).unwrap();
    storage
        .create_issue(&IssueDraft {
            title: "anything".into(),
            body: "any body".into(),
            ..Default::default()
        })
        .unwrap();

    let hits = storage.search("", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert!(hits.is_empty(), "empty query must not match-everything");
}

#[test]
fn search_no_match_returns_empty() {
    let repo = make_scratch_repo("search_no_match");
    let storage = Storage::open(&repo).unwrap();
    storage
        .create_issue(&IssueDraft {
            title: "alpha".into(),
            body: "the body talks about alpha".into(),
            ..Default::default()
        })
        .unwrap();

    let hits = storage.search("nonexistent", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn search_title_only_hit() {
    let repo = make_scratch_repo("search_title_only");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "panic on segfault".into(),
            body: "unrelated text".into(),
            ..Default::default()
        })
        .unwrap();

    let hits = storage.search("segfault", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].issue.id, id);
    assert_eq!(hits[0].matched_field, MatchedField::Title);
    assert_eq!(hits[0].score, 1);
    assert!(hits[0].snippet.contains("segfault"));
}

#[test]
fn search_body_only_hit() {
    let repo = make_scratch_repo("search_body_only");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "totally unrelated".into(),
            body: "this body mentions the magic word: marshmallow".into(),
            ..Default::default()
        })
        .unwrap();

    let hits = storage.search("marshmallow", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].issue.id, id);
    assert_eq!(hits[0].matched_field, MatchedField::Body);
    assert_eq!(hits[0].score, 1);
}

#[test]
fn search_comments_only_with_flag_widens_results() {
    let repo = make_scratch_repo("search_comments_only");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "unrelated".into(),
            body: "unrelated body".into(),
            ..Default::default()
        })
        .unwrap();
    storage
        .add_comment(&id, "thoughts on widget X", "alice <a@x>")
        .unwrap();

    // Without the flag, the comment text is invisible to search.
    let hits_off = storage.search("widget", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert!(hits_off.is_empty(), "comments must be off by default");

    // With the flag, the same query hits.
    let hits_on = storage.search("widget", true, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits_on.len(), 1);
    assert_eq!(hits_on[0].issue.id, id);
    assert_eq!(hits_on[0].matched_field, MatchedField::Comments);
    assert_eq!(hits_on[0].score, 1);
    assert!(hits_on[0].snippet.contains("widget"));
}

#[test]
fn search_is_case_insensitive() {
    let repo = make_scratch_repo("search_case_insensitive");
    let storage = Storage::open(&repo).unwrap();
    storage
        .create_issue(&IssueDraft {
            title: "Foo bar baz".into(),
            body: "BODY HAS FOO IN ALLCAPS".into(),
            ..Default::default()
        })
        .unwrap();

    // Lowercase query against a Mixed/UPPER source.
    for q in ["foo", "FOO", "Foo"] {
        let hits = storage.search(q, false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
        assert_eq!(hits.len(), 1, "case-insensitive match for {q}");
        // Title beats body even though both hit.
        assert_eq!(hits[0].matched_field, MatchedField::Title);
        // Two hits total: one in title ("Foo") + one in body ("FOO").
        assert_eq!(hits[0].score, 2, "score counts both hits, got {q}");
    }
}

#[test]
fn search_multi_field_hit_picks_title_first_and_sums_score() {
    let repo = make_scratch_repo("search_multi_field");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "needle in the title".into(),
            body: "the body also has needle, and another needle".into(),
            ..Default::default()
        })
        .unwrap();
    storage
        .add_comment(&id, "comment with needle too", "alice <a@x>")
        .unwrap();

    // include_comments = true so all three fields participate.
    let hits = storage.search("needle", true, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits.len(), 1);
    // Title wins the priority race.
    assert_eq!(hits[0].matched_field, MatchedField::Title);
    // Score: 1 (title) + 2 (body) + 1 (comment) = 4.
    assert_eq!(hits[0].score, 4, "score = total occurrences across all fields");
    // Snippet comes from the matched field (title).
    assert!(hits[0].snippet.contains("needle"));
    assert!(
        hits[0].snippet.contains("title"),
        "snippet should preview the matched field (title), got: {:?}",
        hits[0].snippet
    );
}

#[test]
fn search_body_wins_when_no_title_hit() {
    let repo = make_scratch_repo("search_body_priority");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "unrelated".into(),
            body: "body mentions the lemming".into(),
            ..Default::default()
        })
        .unwrap();
    storage
        .add_comment(&id, "lemming again", "alice <a@x>")
        .unwrap();

    let hits = storage.search("lemming", true, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits.len(), 1);
    // No title hit → body wins priority over comments even though
    // comments also hit.
    assert_eq!(hits[0].matched_field, MatchedField::Body);
    assert_eq!(hits[0].score, 2);
}

// --- tests: make_snippet -------------------------------------------

#[test]
fn snippet_middle_of_body_shows_symmetric_window() {
    // The hit is well inside the body; window is ±N on either side,
    // ellipsis on both ends.
    let body = "aaaaaaaaaaaaaaaaaaaaaaaa needle bbbbbbbbbbbbbbbbbbbbbbbb";
    let snip = make_snippet(body, "needle", 10);
    assert!(snip.starts_with('…'), "leading ellipsis: {snip:?}");
    assert!(snip.ends_with('…'), "trailing ellipsis: {snip:?}");
    assert!(snip.contains("needle"));
    // The window proper is 10 chars before + "needle" (6) + 10 chars
    // after = 26 chars total.
    let inner: String = snip.chars().filter(|c| *c != '…').collect();
    assert_eq!(inner.chars().count(), 26, "window width: {inner:?}");
}

#[test]
fn snippet_near_start_clips_left_only() {
    let body = "needle is at the start of the body string";
    let snip = make_snippet(body, "needle", 10);
    // Hit at offset 0 → no leading ellipsis. The body has trailing
    // content beyond the window, so the trailing ellipsis fires.
    assert!(!snip.starts_with('…'), "no leading ellipsis: {snip:?}");
    assert!(snip.ends_with('…'), "trailing ellipsis: {snip:?}");
    assert!(snip.contains("needle"));
}

#[test]
fn snippet_near_end_clips_right_only() {
    let body = "the body trails to its end where we find a needle";
    let snip = make_snippet(body, "needle", 10);
    // Hit near end → no trailing ellipsis. Leading ellipsis fires.
    assert!(snip.starts_with('…'), "leading ellipsis: {snip:?}");
    assert!(!snip.ends_with('…'), "no trailing ellipsis: {snip:?}");
    assert!(snip.contains("needle"));
}

#[test]
fn snippet_handles_multibyte_utf8() {
    // Mix multibyte content with the ASCII needle. The window should
    // slice on char boundaries; no byte-slice panic.
    let body = "αβγδ-needle-εζηθ-αβγδ-εζηθ-αβγδ-εζηθ";
    let snip = make_snippet(body, "needle", 5);
    assert!(snip.contains("needle"));
    // No panic = passed. Also verify the snippet is valid UTF-8 (it
    // came from `format!` so this is structurally guaranteed, but
    // testing it pins the contract).
    assert!(snip.is_char_boundary(0));
    assert!(snip.is_char_boundary(snip.len()));
}

#[test]
fn snippet_normalizes_newlines_and_tabs_to_spaces() {
    // The CLI's plain-text row is tab-separated; the snippet must
    // not introduce stray tabs or newlines that would break column
    // count. The storage layer normalizes these to spaces.
    let body = "line one\nline two\twith tab\nline three with needle\nline four";
    let snip = make_snippet(body, "needle", 20);
    assert!(snip.contains("needle"));
    assert!(!snip.contains('\n'), "no embedded newlines: {snip:?}");
    assert!(!snip.contains('\t'), "no embedded tabs: {snip:?}");
}

#[test]
fn snippet_returns_empty_when_needle_absent() {
    let snip = make_snippet("nothing matches here", "absent", 10);
    assert!(snip.is_empty(), "empty when no hit: {snip:?}");
}

#[test]
fn snippet_case_insensitive_lookup() {
    // The caller is expected to pre-lowercase the needle; the snippet
    // function lowercases the haystack internally for matching but
    // returns the ORIGINAL casing in the rendered snippet.
    let body = "Needle in the haystack";
    let snip = make_snippet(body, "needle", 5);
    assert!(snip.contains("Needle"), "preserve original casing: {snip:?}");
}

// --- tests: filter + sort composition (storage layer only) ----------

#[test]
fn search_does_not_apply_status_or_label_filters() {
    // Filter composition is the CLI's job, not the storage layer's.
    // Verify the storage layer returns the raw set so the CLI can
    // compose status/label filters itself.
    let repo = make_scratch_repo("search_no_storage_filter");
    let storage = Storage::open(&repo).unwrap();
    let open_id = storage
        .create_issue(&IssueDraft {
            title: "open needle".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    let closed_id = storage
        .create_issue(&IssueDraft {
            title: "closed needle".into(),
            type_: Some(IssueType::Bug),
            ..Default::default()
        })
        .unwrap();
    storage.set_status(&closed_id, iss_storage::Status::Closed).unwrap();

    let hits = storage.search("needle", false, false, DEFAULT_SNIPPET_CONTEXT).unwrap();
    assert_eq!(hits.len(), 2, "storage layer returns every status");
    let ids: Vec<_> = hits.iter().map(|h| &h.issue.id).collect();
    assert!(ids.contains(&&open_id));
    assert!(ids.contains(&&closed_id));
}
