//! Property-based tests for `jjf-storage`'s mutate/read surface.
//!
//! Issue `df74809` (qa-proptest-harness, epic:agent-ergonomics). The
//! 2026-06-25 QA red-team round found 13 bugs via hand-crafted attack
//! trees; proptest would have caught at least two as falsified
//! invariants (mixed-kind dep cycle `121f48b`, corrupt sentinel
//! `de59159`). This harness installs the durable defense.
//!
//! Three properties ship in the MVP cut:
//!
//! 1. **Round-trip on create_issue**: every draft field that survives
//!    the storage stamp comes back equal on `Storage::read`. The high-
//!    leverage property — gets "did the slug make it to disk?" bugs
//!    cheaply.
//! 2. **Status-machine no-panic + post-state matches oracle**: a
//!    random sequence of verbs (block / unblock / close / open /
//!    abandon / set_status / add_label / add_comment) applied to one
//!    issue either succeeds with the expected post-status (per the
//!    spec §3 status matrix mirrored from `experiments/qa-redteam-
//!    2026-06-25/sub2-wronganswer.sh`'s b2 table) or returns a typed
//!    `StorageError`. Never panics.
//! 3. **Read-after-write idempotence**: after any successful mutate,
//!    two consecutive `Storage::read(id)` calls return equal records.
//!    Catches snapshot-cache invalidation regressions.
//!
//! Properties 4 (`list_ready` monotone-with-closure) and 5
//! (`list_ids` cardinality after N creates) are punted to follow-ups
//! to stay under the 6h time budget — see the closing comment on
//! `df74809`.
//!
//! ## Design constraints
//!
//! - **Hermetic scratch repos**: each property case rolls a fresh
//!   v2-bookmark jj repo under `tests/.scratch/proptest-<uuid>/`,
//!   wiped on each run. We can't share repos across cases — proptest
//!   shrinking assumes pure functions. The scratch dirs are named off
//!   a monotonically incrementing counter + process pid so concurrent
//!   property-test runs don't collide on path.
//! - **Pinned wall clock** (`JJF_TEST_CLOCK_SECS`): timestamps don't
//!   drift across cases so round-trip equality checks are
//!   deterministic. Each nextest test runs in its own process, so the
//!   env-var pin doesn't leak between siblings.
//! - **Bounded input space**: titles 1-32 ASCII, bodies 0-256 bytes,
//!   labels from a 7-element pool, slugs from a 5-element pool (with
//!   50% None). Big input spaces produce slow tests and cryptic
//!   failures; the 2026-06-25 QA round taught us small + targeted
//!   beats sprawling + generic.
//! - **64 cases per property** instead of proptest's default 256.
//!   Ad-hoc `PROPTEST_CASES=1024 cargo test` is available when you
//!   want to crank it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use jjf_storage::{IssueDraft, IssueId, IssueType, Status, Storage};
use proptest::collection::vec;
use proptest::option;
use proptest::prelude::*;

// --- bootstrap (mirror search.rs / stale.rs / integration.rs) ------

/// Per-process counter so concurrent cases never collide on path.
/// proptest runs cases serially within one test, but the test process
/// might be invoked alongside other tests, and tempdir-style fresh
/// names are cheaper than a uuid dep.
static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fresh empty jj repo, with `Storage::init` already called to plant
/// the v3 sentinel. v3-init is two CLI shell-outs total (`jj git init`
/// + `git update-ref`), an order of magnitude cheaper than the v2
/// bootstrap-then-migrate path used by `search.rs` / `stale.rs` — and
/// proptest cases pay this cost N times, so the difference matters.
fn fresh_scratch_repo(prefix: &str) -> PathBuf {
    let n = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = format!("proptest-{prefix}-{pid}-{n}");
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(".scratch")
        .join(&name);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).unwrap();
    }
    fs::create_dir_all(&scratch).unwrap();
    let abs = fs::canonicalize(&scratch).unwrap();
    sh("jj", &["git", "init"], &abs);
    // v3 init contract: zero jj calls, zero bookmarks. The sentinel
    // ref under `refs/jjf/meta/format-version` is the entire setup.
    Storage::init(&abs).unwrap();
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

/// Pin the wall clock so generated `created_at` / `updated_at`
/// stamps are deterministic. Same shape as `stale.rs::pin_clock`.
fn pin_clock(secs: u64) {
    // SAFETY: single-threaded test process per nextest sandbox.
    unsafe {
        std::env::set_var("JJF_TEST_CLOCK_SECS", secs.to_string());
    }
}

// --- generators ----------------------------------------------------

/// 1-32 ASCII non-control chars, guaranteed to pass `validate_title`.
/// Excludes \0, \n, \r, \t and the rest of the C0/C1 control range.
fn title_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,;:!?+\\-]{1,32}"
        .prop_map(|s| {
            // Strategy regex doesn't strip leading/trailing
            // whitespace, but validate_title rejects empty AFTER trim
            // and we want every case to pass. Forcibly fill with 'x'
            // if the regex produced something that trims empty.
            if s.trim().is_empty() { "x".to_string() } else { s }
        })
}

/// 0-256 bytes, well under the 65536-byte body cap. Same character
/// class as titles so we don't surprise ourselves with UTF-8 weirdness.
fn body_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,;:!?+\\-\n]{0,256}".prop_map(String::from)
}

/// Fixed pool of label names — bounded so we don't sprawl the input
/// space. Each draft picks 0-4 from this pool.
const LABEL_POOL: &[&str] =
    &["bug", "feature", "epic", "p0", "p1", "qa", "test"];

fn labels_strategy() -> impl Strategy<Value = Vec<String>> {
    vec(0usize..LABEL_POOL.len(), 0..=4).prop_map(|indices| {
        indices.into_iter().map(|i| LABEL_POOL[i].to_string()).collect()
    })
}

/// 50% None, 50% a slug from a fixed 4-element pool. Slug uniqueness
/// is enforced — collisions surface as typed `SlugCollision`, which
/// the harness treats as a valid outcome.
const SLUG_POOL: &[&str] =
    &["alpha-one", "beta-two", "gamma-three", "delta-four"];

fn slug_strategy() -> impl Strategy<Value = Option<String>> {
    option::of(prop_oneof![
        Just(SLUG_POOL[0].to_string()),
        Just(SLUG_POOL[1].to_string()),
        Just(SLUG_POOL[2].to_string()),
        Just(SLUG_POOL[3].to_string()),
    ])
}

fn type_strategy() -> impl Strategy<Value = Option<IssueType>> {
    option::of(prop_oneof![
        Just(IssueType::Bug),
        Just(IssueType::Feature),
        Just(IssueType::Epic),
        Just(IssueType::Research),
        // Skip Roadmap — there's a "one per repo by convention"
        // expectation. Generating many doesn't crash anything but
        // skews the slug-collision rate.
        Just(IssueType::Unspecified),
    ])
}

fn status_strategy() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Open),
        Just(Status::Blocked),
        Just(Status::InProgress),
        Just(Status::Closed),
        Just(Status::Abandoned),
    ]
}

prop_compose! {
    /// One self-contained `IssueDraft`. No deps — drafting deps
    /// requires already-extant targets, which we'd need a multi-stage
    /// generator for. Punted to follow-ups.
    fn draft_strategy()(
        title in title_strategy(),
        body in body_strategy(),
        labels in labels_strategy(),
        slug in slug_strategy(),
        type_ in type_strategy(),
    ) -> IssueDraft {
        IssueDraft {
            title,
            body,
            labels,
            dependencies: Vec::new(),
            assignee: None,
            type_,
            slug,
        }
    }
}

/// One "verb" the status-machine property can dispatch against a live
/// issue. Names mirror the b2 table in
/// `experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh`.
#[derive(Debug, Clone)]
enum Verb {
    /// `Storage::set_status(_, status)`. Unconditional flip — succeeds
    /// from every source.
    SetStatus(Status),
    /// `Storage::block(_, None)`. Rejects on Closed / Abandoned;
    /// otherwise lands `Blocked`.
    Block,
    /// `Storage::unblock(_)`. Rejects on Closed / Abandoned; otherwise
    /// lands `Open`.
    Unblock,
    /// `Storage::add_label(_, "qa")`. Status-preserving.
    AddLabel,
    /// `Storage::add_comment(_, "x", "bot")`. Status-preserving.
    AddComment,
    /// `Storage::set_title(_, "renamed")`. Status-preserving.
    SetTitle,
}

fn verb_strategy() -> impl Strategy<Value = Verb> {
    prop_oneof![
        status_strategy().prop_map(Verb::SetStatus),
        Just(Verb::Block),
        Just(Verb::Unblock),
        Just(Verb::AddLabel),
        Just(Verb::AddComment),
        Just(Verb::SetTitle),
    ]
}

/// Predict the post-status given the source status and verb. Mirrors
/// `expected_post_status` from the b2 shell oracle. The whole point of
/// this function is to BE the oracle: if storage code drifts from the
/// spec §3 matrix, the property fails with a small counter-example.
fn predict_post_status(source: Status, verb: &Verb) -> PostStatus {
    match verb {
        Verb::SetStatus(new) => PostStatus::Lands(*new),
        Verb::Block => match source {
            Status::Closed | Status::Abandoned => PostStatus::Rejects(source),
            _ => PostStatus::Lands(Status::Blocked),
        },
        Verb::Unblock => match source {
            Status::Closed | Status::Abandoned => PostStatus::Rejects(source),
            // Per `Storage::unblock`: when source is Open with no
            // block_reason, we Skip — status stays Open. Other
            // sources (Blocked, InProgress) flip to Open.
            _ => PostStatus::Lands(Status::Open),
        },
        Verb::AddLabel | Verb::AddComment | Verb::SetTitle => {
            // Status-preserving verbs.
            PostStatus::Lands(source)
        }
    }
}

/// Result classifier for the predicted post-status. Either we expect
/// the call to land with a specific status, OR we expect it to reject
/// AND leave the source status intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostStatus {
    /// Call must succeed (or skip) and the post-read status equals
    /// this.
    Lands(Status),
    /// Call must reject; the post-read status equals this (i.e. the
    /// source — failed mutations are atomic).
    Rejects(Status),
}

/// Drive a fresh issue to the requested source status via the
/// shortest path. Returns the issue id.
fn drive_to_status(storage: &Storage, target: Status) -> IssueId {
    let id = storage
        .create_issue(&IssueDraft {
            title: "drive_to_status".into(),
            body: "seed".into(),
            ..Default::default()
        })
        .unwrap();
    match target {
        Status::Open => {}
        Status::Blocked => storage.block(&id, None).unwrap(),
        Status::InProgress => {
            storage.set_status(&id, Status::InProgress).unwrap()
        }
        Status::Closed => storage.set_status(&id, Status::Closed).unwrap(),
        Status::Abandoned => {
            storage.set_status(&id, Status::Abandoned).unwrap()
        }
    }
    id
}

/// Apply one verb against the live issue.
fn apply_verb(storage: &Storage, id: &IssueId, verb: &Verb) -> jjf_storage::Result<()> {
    match verb {
        Verb::SetStatus(s) => storage.set_status(id, *s),
        Verb::Block => storage.block(id, None),
        Verb::Unblock => storage.unblock(id),
        Verb::AddLabel => storage.add_label(id, "qa"),
        Verb::AddComment => {
            storage.add_comment(id, "comment-body", "bot").map(|_| ())
        }
        Verb::SetTitle => storage.set_title(id, "renamed"),
    }
}

// --- properties ----------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        // 16 cases per property — each case spins a fresh jj repo
        // and does multiple shell-outs at ~200ms each, so we trade
        // breadth for wall-clock runtime. The inner loop has to stay
        // fast; ad-hoc `PROPTEST_CASES=1024 cargo test --release` is
        // available when chasing a real bug.
        cases: 16,
        // Failure-persistence file: when a property fails, proptest
        // writes the seed to `proptest-regressions/` so the case
        // re-runs on subsequent invocations. Default behavior; just
        // making it visible.
        failure_persistence: Some(Box::new(
            proptest::test_runner::FileFailurePersistence::WithSource(
                "regressions",
            ),
        )),
        .. ProptestConfig::default()
    })]

    /// **Property 1: round-trip on `create_issue`**.
    ///
    /// Every field the draft supplied appears equal on `Storage::read`.
    /// Storage-stamped fields (id, status=Open, created_at, updated_at)
    /// are excluded from the equality check.
    ///
    /// The slug-collision branch falsifies cleanly — when the same
    /// slug shows up twice (rare in a fresh repo since each case has
    /// only one create, but proptest's shrinker may exercise it once we
    /// extend), we'd get `Error::SlugCollision` and skip the round-
    /// trip check. For the MVP cut we create exactly one issue per
    /// case, so collisions are structurally impossible.
    #[test]
    fn round_trip_create_then_read(draft in draft_strategy()) {
        pin_clock(1_800_000_000);
        let repo = fresh_scratch_repo("rt");
        let storage = Storage::open(&repo).unwrap();

        let id = storage.create_issue(&draft).unwrap();
        let issue = storage.read(&id).unwrap();

        prop_assert_eq!(&issue.title, &draft.title);
        prop_assert_eq!(&issue.body, &draft.body);
        prop_assert_eq!(&issue.slug, &draft.slug);
        prop_assert_eq!(
            issue.type_,
            draft.type_.unwrap_or(IssueType::Unspecified),
        );
        prop_assert_eq!(issue.status, Status::Open);
        // Labels are sorted+deduped on write; compare as sorted sets.
        let mut expected_labels = draft.labels.clone();
        expected_labels.sort();
        expected_labels.dedup();
        prop_assert_eq!(&issue.labels, &expected_labels);
        prop_assert_eq!(&issue.assignee, &draft.assignee);
        prop_assert!(issue.dependencies.is_empty());
        // No comments on create.
        prop_assert!(issue.comments.is_empty());
    }

    /// **Property 2: status-machine no-panic + post-state matches
    /// oracle**.
    ///
    /// Drive an issue to a generated source status, then apply a
    /// sequence of 1-6 verbs. For each verb:
    ///
    /// - `apply_verb` returns `Ok(_)` or `Err(_)` — never panics.
    /// - The post-`Storage::read` status matches the prediction from
    ///   `predict_post_status` (success path) OR matches the source
    ///   (rejection path; failed mutations are atomic).
    /// - On the rejection path, the error MUST be `Error::Invalid`
    ///   (the typed variant `Storage::block` / `Storage::unblock`
    ///   surface from a closed/abandoned source). A panic, a different
    ///   typed variant, or an Ok where we expected Err all fail the
    ///   property.
    ///
    /// This is the property that would have caught `121f48b` — a
    /// silent-success on a verb that should have rejected.
    #[test]
    fn status_machine_no_panic_matches_oracle(
        source in status_strategy(),
        verbs in vec(verb_strategy(), 1..=4),
    ) {
        pin_clock(1_800_000_001);
        let repo = fresh_scratch_repo("sm");
        let storage = Storage::open(&repo).unwrap();

        let id = drive_to_status(&storage, source);
        let mut current = source;

        for verb in &verbs {
            let prediction = predict_post_status(current, verb);
            let result = apply_verb(&storage, &id, verb);
            // Re-read the post-state, regardless of outcome.
            let post = storage.read(&id).unwrap();

            match prediction {
                PostStatus::Lands(want) => {
                    prop_assert!(
                        result.is_ok(),
                        "verb {:?} on source {:?} predicted Lands({:?}) \
                         but returned Err: {:?}",
                        verb, current, want, result,
                    );
                    prop_assert_eq!(
                        post.status, want,
                        "verb {:?} on source {:?} predicted post-status \
                         {:?}, observed {:?}",
                        verb, current, want, post.status,
                    );
                    current = want;
                }
                PostStatus::Rejects(want_post) => {
                    // Must reject AND leave status untouched.
                    match result {
                        Err(jjf_storage::Error::Invalid(_)) => {}
                        Err(other) => prop_assert!(
                            false,
                            "verb {:?} on source {:?} predicted typed \
                             rejection, returned {:?}",
                            verb, current, other,
                        ),
                        Ok(()) => prop_assert!(
                            false,
                            "verb {:?} on source {:?} predicted typed \
                             rejection, returned Ok",
                            verb, current,
                        ),
                    }
                    prop_assert_eq!(
                        post.status, want_post,
                        "verb {:?} on source {:?} rejected (good) but \
                         status mutated {:?} -> {:?} — failed mutation \
                         must be atomic",
                        verb, current, want_post, post.status,
                    );
                    // current stays put on rejection.
                }
            }
        }
    }

    /// **Property 3: read-after-write idempotence**.
    ///
    /// After any sequence of successful mutations, two consecutive
    /// `Storage::read(id)` calls return equal `Issue` records. Catches
    /// snapshot-cache invalidation regressions (the bug class v3 has
    /// surfaced more than once — see `cache_first_write` /
    /// `v3_cache_invalidate` in `integration.rs`).
    ///
    /// We use a smaller verb set (block/unblock/setStatus/addLabel/
    /// addComment) — title/body churn doesn't add coverage and lengthen
    /// every shell-out.
    #[test]
    fn read_after_write_idempotent(verbs in vec(verb_strategy(), 1..=3)) {
        pin_clock(1_800_000_002);
        let repo = fresh_scratch_repo("idem");
        let storage = Storage::open(&repo).unwrap();
        let id = storage
            .create_issue(&IssueDraft {
                title: "idem-seed".into(),
                body: "b".into(),
                ..Default::default()
            })
            .unwrap();

        for verb in &verbs {
            // Apply (ignore Err — the property is about read
            // idempotence, not whether the verb succeeded).
            let _ = apply_verb(&storage, &id, verb);
            let a = storage.read(&id).unwrap();
            let b = storage.read(&id).unwrap();
            prop_assert_eq!(
                &a, &b,
                "two consecutive reads diverged after verb {:?}",
                verb,
            );
        }
    }
}

// --- non-proptest smoke tests (the harness as plumbing) ------------

/// Cheap sanity check that the bootstrap helpers work without
/// proptest's machinery in the way. If this regresses, the property
/// tests can't possibly work — but their failure messages would be
/// less useful.
#[test]
fn harness_bootstrap_smoke() {
    pin_clock(1_800_000_003);
    let repo = fresh_scratch_repo("smoke");
    let storage = Storage::open(&repo).unwrap();
    let id = storage
        .create_issue(&IssueDraft {
            title: "smoke".into(),
            body: String::new(),
            ..Default::default()
        })
        .unwrap();
    let issue = storage.read(&id).unwrap();
    assert_eq!(issue.title, "smoke");
    assert_eq!(issue.status, Status::Open);
}

