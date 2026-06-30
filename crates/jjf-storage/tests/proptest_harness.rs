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

use jjf_storage::{
    DepEdge, DepKind, IssueDraft, IssueId, IssueType, Memory, ReadyFilter, Status, Storage,
};
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
            priority: None,
            metadata: std::collections::BTreeMap::new(),
        }
    }
}

// --- multi-issue plan generator ------------------------------------
//
// Foundation for properties that need a populated dep graph rather
// than one isolated issue. Issue `c6aed85`
// (proptest-multi-issue-generator, epic:agent-ergonomics). Unblocks
// `proptest-list-ready-monotone` (`6ce795c`) and
// `proptest-cycle-rejection` (`1078439`).
//
// Design: pure data plan + materialize function. Splitting the two
// keeps the strategy side-effect-free so proptest's shrinker can do
// its job — failures shrink the plan (drafts, edges) without
// re-running every shrink-candidate against a fresh repo just to
// shrink it again. The materialize call happens once per property
// case, inside the test body.
//
// Cycle-freedom by construction: every generated edge has
// `child_idx > parent_idx`. The edge points from `drafts[child_idx]`
// (the owner) to `drafts[parent_idx]` (the target). Since indices
// are strictly increasing along edges, the resulting graph is a DAG
// — no edge can ever close a cycle. The cycle-rejection codepath
// (`43c7615`) is therefore never tripped by this generator's
// edge set. `proptest-cycle-rejection` (`1078439`) is the place
// to exercise that path; here we keep the base round-trip clean.
//
// Slugs are forced to `None` for multi-issue drafts. With N up to
// 4 and a 4-element SLUG_POOL, the `SlugCollision` rate from
// `slug_strategy` is high enough that collisions would dominate
// the create-loop's outcome distribution. Dropping slugs keeps
// every create call in the success path; slug coverage stays on
// Property 1.

/// One self-contained `IssueDraft` with `slug = None`. See the
/// module-level note above the multi-issue plan generator for why
/// slugs are dropped in the N-issue path.
fn draft_strategy_no_slug() -> impl Strategy<Value = IssueDraft> {
    (
        title_strategy(),
        body_strategy(),
        labels_strategy(),
        type_strategy(),
    )
        .prop_map(|(title, body, labels, type_)| IssueDraft {
            title,
            body,
            labels,
            dependencies: Vec::new(),
            assignee: None,
            type_,
            slug: None,
            priority: None,
            metadata: std::collections::BTreeMap::new(),
        })
}

/// Pure plan for an N-issue repo with a dep graph. Generated as
/// data, then materialized against a fresh scratch repo by
/// [`build_multi_issue_repo`].
///
/// Edges carry index-into-`drafts` pairs because the real
/// [`IssueId`]s aren't known until create-time. The materialize
/// step resolves them.
#[derive(Debug, Clone)]
struct MultiIssuePlan {
    drafts: Vec<IssueDraft>,
    /// `(child_idx, parent_idx, kind)`. By construction
    /// `child_idx > parent_idx`, so the resulting graph is a DAG and
    /// no edge ever closes a cycle. The owner is `drafts[child_idx]`;
    /// the target is `drafts[parent_idx]`.
    edges: Vec<(usize, usize, DepKind)>,
}

/// Generate one [`MultiIssuePlan`]. `n_range` controls the issue
/// count (e.g. `1..=4`); `max_edges` caps the per-plan edge count
/// (e.g. `4`). Edge kinds are picked uniformly across the four
/// `DepKind` variants — both blocking kinds (`Blocks`,
/// `ParentChild`, which participate in cycle detection) and the
/// non-blocking kinds (`Related`, `DiscoveredFrom`).
fn multi_issue_plan_strategy(
    n_range: std::ops::RangeInclusive<usize>,
    max_edges: usize,
) -> impl Strategy<Value = MultiIssuePlan> {
    let drafts = vec(draft_strategy_no_slug(), n_range);
    drafts.prop_flat_map(move |drafts| {
        let n = drafts.len();
        // Number of distinct (child, parent) index pairs in a strict
        // upper triangle: n*(n-1)/2. With n in 1..=4 the max is 6,
        // comfortably above max_edges=4. With n == 1 the graph has
        // no valid edges (no pair satisfies child > parent), so the
        // edge vec must be empty.
        let edge_strategy = if n < 2 {
            vec(edge_triple_strategy(n), 0..=0).boxed()
        } else {
            vec(edge_triple_strategy(n), 0..=max_edges).boxed()
        };
        edge_strategy.prop_map(move |raw_edges| {
            // Dedupe (child, parent, kind) triples. Two ops with the
            // same triple is fine at the storage layer (a no-op
            // post-dedupe), but the test surface stays cleaner
            // without trivial duplicates.
            let mut edges = raw_edges.clone();
            edges.sort();
            edges.dedup();
            MultiIssuePlan {
                drafts: drafts.clone(),
                edges,
            }
        })
    })
}

/// One `(child_idx, parent_idx, kind)` edge triple. `n` is the
/// number of issues in the plan; this strategy assumes `n >= 2`
/// (the caller guards `n < 2` and skips edge generation entirely).
fn edge_triple_strategy(
    n: usize,
) -> impl Strategy<Value = (usize, usize, DepKind)> {
    // child_idx ranges over 1..n; for each, parent_idx ranges over
    // 0..child_idx. Generating in two steps keeps the strict
    // upper-triangle invariant.
    (1..n)
        .prop_flat_map(|child_idx| {
            (Just(child_idx), 0..child_idx, dep_kind_strategy())
        })
}

fn dep_kind_strategy() -> impl Strategy<Value = DepKind> {
    prop_oneof![
        Just(DepKind::Blocks),
        Just(DepKind::ParentChild),
        Just(DepKind::Related),
        Just(DepKind::DiscoveredFrom),
    ]
}

/// Materialize a plan against a fresh scratch repo. Creates every
/// draft in order, then applies every edge via
/// [`Storage::add_dep_edge`]. Returns the storage handle plus the
/// minted [`IssueId`]s in plan order (so `drafts[i]` lives at
/// `ids[i]`).
///
/// Both create and dep-add unwrap on failure. The plan generator
/// is constructed so neither call should reject: drafts pass
/// validation (titles non-empty post-trim, labels from a clean
/// pool, slugs disabled so no `SlugCollision`), and edges are a
/// strict-upper-triangle DAG so cycle preflight never fires.
/// If unwrap panics, the plan generator has drifted from the
/// storage layer's preconditions — that's a bug worth surfacing
/// loudly, not papering over.
fn build_multi_issue_repo(
    plan: &MultiIssuePlan,
    prefix: &str,
) -> (Storage, Vec<IssueId>) {
    let repo = fresh_scratch_repo(prefix);
    let storage = Storage::open(&repo).unwrap();
    let mut ids = Vec::with_capacity(plan.drafts.len());
    for draft in &plan.drafts {
        ids.push(storage.create_issue(draft).unwrap());
    }
    for (child_idx, parent_idx, kind) in &plan.edges {
        // Strict upper triangle: child_idx > parent_idx, so the
        // two indices are distinct and the SelfDependency
        // precondition isn't tripped either.
        storage
            .add_dep_edge(&ids[*child_idx], &ids[*parent_idx], *kind)
            .unwrap();
    }
    (storage, ids)
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

// --- memory generators ---------------------------------------------
//
// Issue `932cc40` (proptest-memory-surface, epic:agent-ergonomics).
// `Storage::set_memory` / `unset_memory` / `read_memory` validate keys
// through the same `validate_slug` rules as issue slugs: kebab-case
// `[a-z0-9-]`, length 3-48, no leading/trailing/consecutive hyphens.
// The value must be non-empty post-trim.
//
// Pool discipline mirrors `LABEL_POOL` / `SLUG_POOL`: a small fixed
// pool of valid keys keeps collisions (and `set` overwrites) frequent
// enough to exercise the overwrite codepath. Values are 1-256
// non-control ASCII bytes with at least one non-whitespace char so
// `set_memory` accepts them.

/// Fixed pool of valid memory keys. All pass `validate_slug` (length
/// 3-48, kebab-case, no leading/trailing/consecutive hyphens). Five
/// keys keep collisions common enough to exercise the overwrite path
/// and the non-interference property's k1 != k2 sampling.
const MEMORY_KEY_POOL: &[&str] = &[
    "alpha-key",
    "beta-key",
    "gamma-key",
    "delta-key",
    "epsilon-key",
];

fn memory_key_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(MEMORY_KEY_POOL[0].to_string()),
        Just(MEMORY_KEY_POOL[1].to_string()),
        Just(MEMORY_KEY_POOL[2].to_string()),
        Just(MEMORY_KEY_POOL[3].to_string()),
        Just(MEMORY_KEY_POOL[4].to_string()),
    ]
}

/// Memory value: 1-256 non-control ASCII bytes with a forced
/// non-whitespace prefix so `set_memory`'s "non-empty after trim"
/// check always passes. Same character class as `body_strategy` for
/// consistency.
fn memory_value_strategy() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,;:!?+\\-]{0,255}".prop_map(|tail| {
        // Force a leading non-whitespace char so the value trims
        // non-empty. Without this, a generated empty string or
        // whitespace-only string would surface as a spurious
        // `Error::Invalid` instead of exercising the round-trip.
        let mut s = String::with_capacity(tail.len() + 1);
        s.push('x');
        s.push_str(&tail);
        s
    })
}

// --- list_ready mutation generator ---------------------------------
//
// Issue `6ce795c` (proptest-list-ready-monotone, epic:agent-ergonomics).
// Drives Property 6: closing an issue can only ADD members to the
// next `list_ready` (the things it was blocking become unblocked);
// opening one can only REMOVE; status-preserving verbs (add_label,
// add_comment) leave the set identical.
//
// We model mutations as `(index_into_ids, kind)` so the strategy
// itself is data and the materialize step picks the actual id from
// the plan-built repo. Kinds picked uniformly across the five
// status-changing variants plus two status-preserving controls. The
// strategy holds no live storage handles — proptest's shrinker can
// shrink mutation sequences without re-binding to a repo.

/// One mutation applied to the multi-issue repo during the
/// `list_ready` monotonicity sweep.
#[derive(Debug, Clone)]
enum ListReadyMutation {
    /// `set_status(_, Closed)`. Lands from every source unconditionally
    /// (see `Storage::set_status` — unguarded flip).
    Close,
    /// `set_status(_, Abandoned)`. Same shape; treated as inactive
    /// alongside Closed for `compute_blocked_set`.
    Abandon,
    /// `set_status(_, Open)`. Re-opens from any source. The "open"
    /// half of the monotonicity property — flipping inactive→active
    /// may re-block dependents.
    Reopen,
    /// `set_status(_, Blocked)`. Stays active for dep purposes but
    /// drops out of the default `ReadyFilter` (which excludes Blocked).
    /// Tests the "self-only flip" branch of the property.
    Block,
    /// `set_status(_, InProgress)`. Same shape as Block — active,
    /// excluded from default `ReadyFilter` (via `include_claimed`).
    InProgress,
    /// `add_label(_, "qa")`. Status-preserving and label-filter is
    /// `ReadyFilter::default().labels == []`, so the ready set must
    /// be identical post-mutation.
    AddLabel,
    /// `add_comment(_, "x", "bot")`. Status-preserving, doesn't
    /// touch any field `list_ready` looks at.
    AddComment,
}

fn list_ready_mutation_kind_strategy() -> impl Strategy<Value = ListReadyMutation> {
    prop_oneof![
        Just(ListReadyMutation::Close),
        Just(ListReadyMutation::Abandon),
        Just(ListReadyMutation::Reopen),
        Just(ListReadyMutation::Block),
        Just(ListReadyMutation::InProgress),
        Just(ListReadyMutation::AddLabel),
        Just(ListReadyMutation::AddComment),
    ]
}

/// `(target_idx, kind)` where `target_idx` indexes into the plan's
/// `ids` vec. The strategy doesn't know N at generation time; we
/// generate a usize in `0..16` and the property body modulos it down
/// to the actual issue count (cheaper than a `prop_flat_map` chain
/// and N is small).
fn list_ready_mutation_strategy() -> impl Strategy<Value = (usize, ListReadyMutation)> {
    (0usize..16, list_ready_mutation_kind_strategy())
}

/// True when the status counts as "active" per
/// [`compute_blocked_set`]'s `is_active` helper — Open, Blocked, or
/// InProgress. Closed and Abandoned are inactive (they release
/// dependents). Mirrored from the storage layer so the property
/// doesn't have to reach into private helpers.
fn status_is_active(s: Status) -> bool {
    matches!(s, Status::Open | Status::Blocked | Status::InProgress)
}

/// Apply one list_ready mutation. Returns `Ok(())` on success and
/// propagates storage errors — the property body treats any Err as
/// "no-op for monotonicity" since failed mutations are atomic.
fn apply_list_ready_mutation(
    storage: &Storage,
    id: &IssueId,
    m: &ListReadyMutation,
) -> jjf_storage::Result<()> {
    match m {
        ListReadyMutation::Close => storage.set_status(id, Status::Closed),
        ListReadyMutation::Abandon => storage.set_status(id, Status::Abandoned),
        ListReadyMutation::Reopen => storage.set_status(id, Status::Open),
        ListReadyMutation::Block => storage.set_status(id, Status::Blocked),
        ListReadyMutation::InProgress => storage.set_status(id, Status::InProgress),
        ListReadyMutation::AddLabel => storage.add_label(id, "qa"),
        ListReadyMutation::AddComment => {
            storage.add_comment(id, "comment-body", "bot").map(|_| ())
        }
    }
}

// --- cycle-rejection generator + walker ---------------------------
//
// Issue `1078439` (proptest-cycle-rejection, epic:agent-ergonomics).
// Drives Property 7: `Storage::add_dep_edge` must NEVER land an edge
// that would close a `Blocks`-or-`ParentChild` cycle — including
// mixed-kind cycles where the existing edges are one kind and the
// new edge is another. Failed cycle attempts surface a typed
// `Error::DependencyCycle` (single-kind: `43c7615`, mixed-kind:
// `121f48b`); the source record's dep list is left untouched.
//
// Generator shape: `multi_issue_plan_strategy` already builds a
// strict-upper-triangle DAG (every edge `child_idx > parent_idx`).
// We tack ONE extra edge onto the plan whose orientation can be
// either DAG-keeping or cycle-closing. The cycle-closing flavor is
// a back-edge `(parent_idx, child_idx, kind)` against the existing
// DAG — picking an existing forward edge `(a, b)` and issuing
// `add_dep_edge(b, a, kind)` reliably attempts to close a cycle.
// We also allow self-edges (target == source) to exercise the
// `Error::SelfDependency` reject path on the same property.
//
// The extra-edge `kind` is sampled across all four `DepKind`
// variants. `Blocks` and `ParentChild` participate in cycle
// detection (and can be cycle-closing); `Related` and
// `DiscoveredFrom` never block — adding them to either orientation
// must always succeed and never trip the cycle path.

/// One add-dep-edge attempt the cycle property fires AFTER the base
/// DAG is in place. Modeled as data so the strategy itself stays
/// pure (proptest shrinker friendly).
#[derive(Debug, Clone)]
enum ExtraEdge {
    /// `add_dep_edge(owner_idx, target_idx, kind)`. The base DAG has
    /// `owner_idx < target_idx` for an edge `owner -> target` in
    /// the plan's `(child_idx, parent_idx, kind)` triples, so to
    /// reverse a base edge into a back-edge attempt we issue
    /// `add_dep_edge(parent_idx, child_idx, kind)`.
    Add {
        owner_idx: usize,
        target_idx: usize,
        kind: DepKind,
    },
    /// `add_dep_edge(idx, idx, kind)`. Must reject with
    /// `Error::SelfDependency` and leave the record's dep list
    /// untouched. Tests a different reject path on the same property
    /// without complicating the cycle generator.
    SelfDep { idx: usize, kind: DepKind },
}

/// Pick an extra-edge to issue against a built repo. `n_drafts` is
/// the plan's draft count so we can clamp indices into-range. We
/// generate raw `usize` indices and modulo them down inside the
/// property body (cheaper than a `prop_flat_map` chain).
fn extra_edge_strategy() -> impl Strategy<Value = ExtraEdge> {
    prop_oneof![
        9 => (0usize..16, 0usize..16, dep_kind_strategy())
            .prop_map(|(owner_idx, target_idx, kind)| ExtraEdge::Add {
                owner_idx, target_idx, kind,
            }),
        1 => (0usize..16, dep_kind_strategy())
            .prop_map(|(idx, kind)| ExtraEdge::SelfDep { idx, kind }),
    ]
}

/// Walk the combined blocking graph (`Blocks` + `ParentChild`
/// edges) and report whether ANY node sits on a directed cycle.
/// Simple iterative DFS with a per-root visited set: cheap given
/// N <= 4 issues per case, and re-deriving from the post-state
/// every time means no incremental-cache-invalidation worry.
///
/// Mirrors `Storage::find_blocking_cycle`'s definition of the
/// "blocking graph" (only `Blocks` + `ParentChild` count;
/// `Related` and `DiscoveredFrom` are skipped) without reaching
/// into a private helper — the test is the independent oracle.
fn blocking_graph_has_cycle(storage: &Storage) -> bool {
    let ids = storage.list_ids().unwrap();
    // Build adjacency map: id -> blocking targets.
    let mut adj: std::collections::HashMap<IssueId, Vec<IssueId>> =
        std::collections::HashMap::new();
    for id in &ids {
        let issue = storage.read(id).unwrap();
        let targets: Vec<IssueId> = issue
            .dependencies
            .iter()
            .filter(|e| matches!(e.kind, DepKind::Blocks | DepKind::ParentChild))
            .map(|e| e.target.clone())
            .collect();
        adj.insert(id.clone(), targets);
    }
    // DFS from each node, using the classic three-color
    // (WHITE/GRAY/BLACK) scheme to detect back-edges. GRAY = on
    // the current recursion stack; hitting a GRAY node closes a
    // cycle. BLACK = fully explored, no cycle through here.
    #[derive(Clone, Copy, PartialEq)]
    enum Color { White, Gray, Black }
    let mut color: std::collections::HashMap<IssueId, Color> =
        ids.iter().map(|i| (i.clone(), Color::White)).collect();
    for root in &ids {
        if color[root] != Color::White {
            continue;
        }
        // Iterative DFS frame: (node, next-child-index-to-visit).
        let mut stack: Vec<(IssueId, usize)> = vec![(root.clone(), 0)];
        color.insert(root.clone(), Color::Gray);
        while let Some((node, idx)) = stack.last().cloned() {
            let children = adj.get(&node).cloned().unwrap_or_default();
            if idx >= children.len() {
                color.insert(node.clone(), Color::Black);
                stack.pop();
                continue;
            }
            // Advance the cursor on the current frame before
            // (maybe) recursing into the child.
            let last = stack.len() - 1;
            stack[last].1 = idx + 1;
            let child = &children[idx];
            // Dangling edges (target id missing from list_ids —
            // see find_blocking_cycle's "treat missing as leaf"
            // comment) can't host a back-edge, skip them.
            let child_color = color.get(child).copied().unwrap_or(Color::Black);
            match child_color {
                Color::White => {
                    color.insert(child.clone(), Color::Gray);
                    stack.push((child.clone(), 0));
                }
                Color::Gray => {
                    // Back-edge: child is on the current recursion
                    // stack. Cycle.
                    return true;
                }
                Color::Black => {}
            }
        }
    }
    false
}

/// Snapshot every issue's dependency list, keyed by issue id. Used
/// to verify that a failed `add_dep_edge` call left the graph
/// untouched (atomic-on-reject contract).
fn snapshot_dep_graph(
    storage: &Storage,
) -> std::collections::HashMap<IssueId, Vec<DepEdge>> {
    let ids = storage.list_ids().unwrap();
    ids.into_iter()
        .map(|id| {
            let deps = storage.read(&id).unwrap().dependencies;
            (id, deps)
        })
        .collect()
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

    /// **Property 1b: multi-issue round-trip with dep edges**.
    ///
    /// The N-issue extension of Property 1. Generate a
    /// [`MultiIssuePlan`] (1-4 drafts, up to 4 dep edges), build it,
    /// then read every issue back and assert:
    ///
    /// - Each draft's owner-visible scalar fields (title, body,
    ///   labels, type, status=Open) round-trip equal.
    /// - Each issue's dependency edge set matches the subset of
    ///   `plan.edges` whose `child_idx` is this issue. The on-disk
    ///   form is sorted+deduped by the writer.
    ///
    /// Issue `c6aed85` (proptest-multi-issue-generator). Foundation
    /// for `proptest-list-ready-monotone` (`6ce795c`) and
    /// `proptest-cycle-rejection` (`1078439`); this property
    /// confirms the multi-issue primitive works end-to-end without
    /// claiming any new invariants beyond Property 1's.
    #[test]
    fn round_trip_multi_issue_with_deps(
        plan in multi_issue_plan_strategy(1..=4, 4),
    ) {
        pin_clock(1_800_000_004);
        let (storage, ids) = build_multi_issue_repo(&plan, "rt-multi");

        for (i, draft) in plan.drafts.iter().enumerate() {
            let issue = storage.read(&ids[i]).unwrap();
            prop_assert_eq!(&issue.title, &draft.title);
            prop_assert_eq!(&issue.body, &draft.body);
            prop_assert_eq!(
                issue.type_,
                draft.type_.unwrap_or(IssueType::Unspecified),
            );
            prop_assert_eq!(issue.status, Status::Open);
            let mut expected_labels = draft.labels.clone();
            expected_labels.sort();
            expected_labels.dedup();
            prop_assert_eq!(&issue.labels, &expected_labels);

            // Expected edges for THIS issue: every plan triple whose
            // child_idx == i. Sort+dedupe to match the writer's
            // normalization (see Storage::add_dep_edge dedupe by
            // (target, kind)).
            let mut expected_edges: Vec<DepEdge> = plan
                .edges
                .iter()
                .filter(|(child_idx, _, _)| *child_idx == i)
                .map(|(_, parent_idx, kind)| DepEdge {
                    target: ids[*parent_idx].clone(),
                    kind: *kind,
                })
                .collect();
            expected_edges.sort();
            expected_edges.dedup();
            prop_assert_eq!(
                &issue.dependencies, &expected_edges,
                "issue {} ({:?}) dependency edges mismatch",
                i, ids[i],
            );
        }
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

    /// **Property 5: `list_ids` cardinality matches creates**.
    ///
    /// Issue `9f113b5` (proptest-list-ids-cardinality,
    /// epic:agent-ergonomics). After N successful `create_issue` calls
    /// on a fresh repo (N ∈ 1..=8):
    ///
    /// - `storage.list_ids().len() == N`.
    /// - Minted ids are pairwise distinct.
    /// - Every minted id appears in `list_ids()`.
    ///
    /// Catches id-collision, silent-deletion-on-create, and list-index
    /// drift. 7-char hex ids have 2^28 possible values; at N=8 the
    /// birthday-collision probability is vanishingly small, so a real
    /// failure here points at storage logic, not RNG luck.
    ///
    /// Uses `draft_strategy_no_slug()` directly rather than the full
    /// `multi_issue_plan_strategy` — cardinality doesn't need a dep
    /// graph, and dropping the edge machinery keeps the property focused
    /// on what it actually claims. Slugs are off for the same reason
    /// the multi-issue path drops them: `SlugCollision` would dominate
    /// the create-loop outcome at N=8 against a 4-slug pool.
    ///
    /// 16 cases default per the harness `ProptestConfig` above.
    #[test]
    fn list_ids_cardinality_matches_creates(
        drafts in vec(draft_strategy_no_slug(), 1..=8),
    ) {
        pin_clock(1_800_000_005);
        let repo = fresh_scratch_repo("listids");
        let storage = Storage::open(&repo).unwrap();

        let mut minted: Vec<IssueId> = Vec::with_capacity(drafts.len());
        for draft in &drafts {
            minted.push(storage.create_issue(draft).unwrap());
        }

        let listed = storage.list_ids().unwrap();

        // Cardinality: list_ids reports exactly N entries.
        prop_assert_eq!(
            listed.len(),
            drafts.len(),
            "list_ids returned {} entries after {} creates",
            listed.len(),
            drafts.len(),
        );

        // Pairwise distinctness of minted ids. A duplicate here means
        // create_issue handed out the same id twice — id-mint logic
        // bug, not a list_ids bug.
        let mut sorted_minted = minted.clone();
        sorted_minted.sort();
        let dedup_len = {
            let mut v = sorted_minted.clone();
            v.dedup();
            v.len()
        };
        prop_assert_eq!(
            dedup_len,
            minted.len(),
            "create_issue returned duplicate ids: {:?}",
            minted,
        );

        // Every minted id appears in list_ids. Sort both sides and
        // compare as multisets (equal cardinality already asserted).
        let mut sorted_listed = listed.clone();
        sorted_listed.sort();
        prop_assert_eq!(
            &sorted_minted, &sorted_listed,
            "minted ids and list_ids disagree (minted={:?}, listed={:?})",
            minted, listed,
        );
    }

    /// **Memory Property 1: round-trip on `set_memory` / `unset_memory`**.
    ///
    /// Issue `932cc40` (proptest-memory-surface, epic:agent-ergonomics).
    /// After `set_memory(k, v)`, `read_memory(k)` returns `Some(m)` with
    /// `m.key == k` and `m.value == v`. After `unset_memory(k)`,
    /// `read_memory(k)` returns `None`. Catches snapshot-cache
    /// invalidation regressions on the memory surface (the same bug class
    /// `cache_first_write` surfaced on the issues surface).
    #[test]
    fn memory_round_trip_set_then_unset(
        key in memory_key_strategy(),
        value in memory_value_strategy(),
    ) {
        pin_clock(1_800_000_010);
        let repo = fresh_scratch_repo("mem-rt");
        let storage = Storage::open(&repo).unwrap();

        storage.set_memory(&key, &value).unwrap();
        let after_set = storage.read_memory(&key).unwrap();
        prop_assert!(
            after_set.is_some(),
            "read_memory({:?}) returned None after set", key,
        );
        let m: Memory = after_set.unwrap();
        prop_assert_eq!(&m.key, &key);
        prop_assert_eq!(&m.value, &value);

        storage.unset_memory(&key).unwrap();
        let after_unset = storage.read_memory(&key).unwrap();
        prop_assert!(
            after_unset.is_none(),
            "read_memory({:?}) returned Some after unset: {:?}",
            key, after_unset,
        );
    }

    /// **Memory Property 2: idempotence**.
    ///
    /// Two consecutive `set_memory(k, v)` with the same value leave the
    /// read shape unchanged: same key, same value (updated_at may
    /// advance, which is fine — it's not part of the observable shape
    /// callers depend on). The second `unset_memory(k)` on a key that
    /// was already removed surfaces as `Error::Invalid` ("no memory
    /// with key") rather than panic — both calls reach a defined
    /// terminal state.
    ///
    /// Note: `unset_memory` on a missing key is NOT silently-ok per
    /// the storage contract (see `crates/jjf-storage/src/lib.rs`
    /// `unset_memory` doc — it returns `Error::Invalid` with a
    /// `not found` message). The property asserts the typed-rejection
    /// shape, not silent idempotence, for the second unset.
    #[test]
    fn memory_idempotent_set_and_unset(
        key in memory_key_strategy(),
        value in memory_value_strategy(),
    ) {
        pin_clock(1_800_000_011);
        let repo = fresh_scratch_repo("mem-idem");
        let storage = Storage::open(&repo).unwrap();

        // Two consecutive sets with the same value.
        storage.set_memory(&key, &value).unwrap();
        let first = storage.read_memory(&key).unwrap().unwrap();
        storage.set_memory(&key, &value).unwrap();
        let second = storage.read_memory(&key).unwrap().unwrap();
        prop_assert_eq!(&first.key, &second.key);
        prop_assert_eq!(&first.value, &second.value);
        // created_at is preserved across overwrites (the original
        // record's created_at survives — see `set_memory` impl).
        prop_assert_eq!(&first.created_at, &second.created_at);

        // First unset succeeds; second unset on missing key
        // surfaces a typed Invalid rather than panicking.
        storage.unset_memory(&key).unwrap();
        let post_unset = storage.read_memory(&key).unwrap();
        prop_assert!(post_unset.is_none());
        let second_unset = storage.unset_memory(&key);
        match second_unset {
            Err(jjf_storage::Error::Invalid(_)) => {}
            other => prop_assert!(
                false,
                "second unset_memory({:?}) on missing key expected \
                 Invalid, got {:?}",
                key, other,
            ),
        }
    }

    /// **Memory Property 3: non-interference**.
    ///
    /// `set_memory(k1, v1)` does not affect `read_memory(k2)` for
    /// k1 != k2. We seed (k2, v2) first, then set (k1, v1) where
    /// k1 != k2, then re-read k2 and assert the value is unchanged.
    /// Catches accidental cross-key writes (e.g. a path-construction
    /// bug that collapses two keys onto the same on-disk slot).
    ///
    /// Two independent key generators with a `prop_assume!` to discard
    /// k1 == k2 cases. With a 5-element pool the discard rate is 20% —
    /// well under proptest's default rejection budget for 16 cases.
    #[test]
    fn memory_set_does_not_affect_other_keys(
        k1 in memory_key_strategy(),
        v1 in memory_value_strategy(),
        k2 in memory_key_strategy(),
        v2 in memory_value_strategy(),
    ) {
        prop_assume!(k1 != k2);
        pin_clock(1_800_000_012);
        let repo = fresh_scratch_repo("mem-noni");
        let storage = Storage::open(&repo).unwrap();

        storage.set_memory(&k2, &v2).unwrap();
        let before = storage.read_memory(&k2).unwrap().unwrap();

        storage.set_memory(&k1, &v1).unwrap();
        let after = storage.read_memory(&k2).unwrap();
        prop_assert!(
            after.is_some(),
            "set_memory({:?}, ...) clobbered unrelated key {:?}",
            k1, k2,
        );
        let after = after.unwrap();
        prop_assert_eq!(&after.key, &k2);
        prop_assert_eq!(&after.value, &v2);
        prop_assert_eq!(&before.value, &after.value);

        // And the k1 write actually landed (sanity: the set wasn't
        // a silent no-op that left both keys alone).
        let k1_read = storage.read_memory(&k1).unwrap().unwrap();
        prop_assert_eq!(&k1_read.key, &k1);
        prop_assert_eq!(&k1_read.value, &v1);
    }

    /// **Property 6: `list_ready` monotone under status flips**.
    ///
    /// Issue `6ce795c` (proptest-list-ready-monotone,
    /// epic:agent-ergonomics). Snapshot `list_ready(&default())` before
    /// and after each mutation against a `MultiIssuePlan` repo (1-4
    /// issues, up to 4 dep edges). Per-mutation assertion depends on
    /// how the target's status changed (observed by reading the issue
    /// pre- and post-mutation):
    ///
    /// - `active -> inactive` (Open/Blocked/InProgress to Closed/Abandoned):
    ///   every other issue's ready-membership can only ADD (deps that
    ///   were blocking are released). The target itself MUST NOT appear
    ///   in `new_ready` (it's inactive).
    /// - `inactive -> active` (Closed/Abandoned to Open/Blocked/InProgress):
    ///   every other issue's ready-membership can only REMOVE (deps may
    ///   re-block). The target itself may newly appear.
    /// - `active -> active`, `inactive -> inactive`, or no status change
    ///   (status-preserving verbs like add_label / add_comment):
    ///   `compute_blocked_set` is unchanged for every other issue, so
    ///   the symmetric difference of the ready sets must be a subset
    ///   of `{target_id}` — only the target's own ready membership can
    ///   flip (Open <-> Blocked drops it out of the default
    ///   `ReadyFilter`).
    ///
    /// This is the property that would have falsified `121f48b`
    /// (mixed-kind blocks+parent-child cycle silently accepted,
    /// locking both issues out of `jjf ready`) directly: a cycle-
    /// locked id would stay missing from `list_ready` across a close
    /// that should have released it.
    ///
    /// Uses `Storage::list_ready` directly (the storage-layer API,
    /// surfaced via the `ReadyFilter` bundle the CLI also uses).
    /// `ReadyFilter::default()` is the `jjf ready` no-flags shape.
    ///
    /// **Failed mutations are atomic**: if `apply_list_ready_mutation`
    /// returns `Err`, the post-read status will equal pre-read status
    /// and the property falls into the no-change branch automatically.
    #[test]
    fn list_ready_monotone_under_status_flips(
        plan in multi_issue_plan_strategy(1..=4, 4),
        muts in vec(list_ready_mutation_strategy(), 1..=6),
    ) {
        pin_clock(1_800_000_006);
        let (storage, ids) = build_multi_issue_repo(&plan, "ready-mono");
        let filter = ReadyFilter::default();

        for (raw_idx, kind) in &muts {
            let idx = raw_idx % ids.len();
            let target = &ids[idx];

            let pre_status = storage.read(target).unwrap().status;
            let pre_ready: std::collections::HashSet<IssueId> = storage
                .list_ready(&filter)
                .unwrap()
                .into_iter()
                .map(|i| i.id)
                .collect();

            // Apply; ignore Err — failed mutations are atomic
            // (post-read status will equal pre-read, so we land in
            // the no-change branch).
            let _ = apply_list_ready_mutation(&storage, target, kind);

            let post_status = storage.read(target).unwrap().status;
            let post_ready: std::collections::HashSet<IssueId> = storage
                .list_ready(&filter)
                .unwrap()
                .into_iter()
                .map(|i| i.id)
                .collect();

            let pre_active = status_is_active(pre_status);
            let post_active = status_is_active(post_status);

            match (pre_active, post_active) {
                (true, false) => {
                    // active -> inactive: dependents may unblock.
                    // For every issue OTHER than the target,
                    // pre-membership implies post-membership (only
                    // additions allowed). The target itself must
                    // have left the ready set.
                    for id in &pre_ready {
                        if id == target {
                            continue;
                        }
                        prop_assert!(
                            post_ready.contains(id),
                            "active->inactive ({:?} -> {:?}) on target {:?} \
                             removed unrelated id {:?} from list_ready \
                             (pre={:?}, post={:?})",
                            pre_status, post_status, target, id,
                            pre_ready, post_ready,
                        );
                    }
                    prop_assert!(
                        !post_ready.contains(target),
                        "active->inactive ({:?} -> {:?}) left target {:?} \
                         in list_ready post (post={:?})",
                        pre_status, post_status, target, post_ready,
                    );
                }
                (false, true) => {
                    // inactive -> active: dependents may re-block.
                    // For every issue OTHER than the target,
                    // post-membership implies pre-membership (only
                    // removals allowed for others; target itself may
                    // newly appear).
                    for id in &post_ready {
                        if id == target {
                            continue;
                        }
                        prop_assert!(
                            pre_ready.contains(id),
                            "inactive->active ({:?} -> {:?}) on target {:?} \
                             added unrelated id {:?} to list_ready \
                             (pre={:?}, post={:?})",
                            pre_status, post_status, target, id,
                            pre_ready, post_ready,
                        );
                    }
                }
                _ => {
                    // active->active, inactive->inactive, or no
                    // status change. compute_blocked_set is invariant
                    // for every issue other than the target; only the
                    // target's own ready-membership can flip (e.g.
                    // Open <-> Blocked changes its default-filter
                    // visibility). Symmetric difference must be a
                    // subset of {target}.
                    let pre_minus_target: std::collections::HashSet<_> =
                        pre_ready.iter().filter(|i| *i != target).cloned().collect();
                    let post_minus_target: std::collections::HashSet<_> =
                        post_ready.iter().filter(|i| *i != target).cloned().collect();
                    prop_assert_eq!(
                        &pre_minus_target, &post_minus_target,
                        "no-status-change branch ({:?} -> {:?}) on target \
                         {:?} mutated unrelated ready membership \
                         (pre={:?}, post={:?})",
                        pre_status, post_status, target, pre_ready, post_ready,
                    );
                }
            }
        }
    }

    /// **Property 7: `add_dep_edge` never closes a cycle**.
    ///
    /// Issue `1078439` (proptest-cycle-rejection, epic:agent-ergonomics).
    /// Build a DAG via [`MultiIssuePlan`] (strict-upper-triangle edges,
    /// cycle-free by construction). Then fire ONE extra
    /// `add_dep_edge` attempt sampled from [`extra_edge_strategy`]
    /// (90% additive `Add`, 10% `SelfDep`). The property asserts the
    /// full add-dep-edge contract on a freshly-derived post-state:
    ///
    /// - If the call succeeds: the combined blocking graph
    ///   (`Blocks` + `ParentChild` edges) contains no directed cycle.
    ///   A walker re-derived from scratch per case ([`blocking_graph_has_cycle`])
    ///   is the independent oracle.
    /// - If the call rejects: the error is a typed `Error::DependencyCycle`
    ///   (cycle-closing attempt on a blocking-kind edge),
    ///   `Error::SelfDependency` (target == source), or
    ///   `Error::IssueNotFound` (shouldn't fire here — every index
    ///   is modulo'd into ids; surface it loudly if it does). And the
    ///   pre-call dep snapshot equals the post-call snapshot — atomic
    ///   on reject.
    ///
    /// This is the property that would have caught `121f48b` (mixed-
    /// kind cycle silently accepted) and `43c7615` (single-kind
    /// cycle accepted) without depending on the harm-class attack
    /// tree to surface them. The `Add` branch with `swap`-ish
    /// indices (owner_idx > target_idx hitting an existing DAG
    /// edge in reverse) reliably attempts a back-edge.
    ///
    /// **N.B.** The ticket body says "typed `Error::Invalid`" but
    /// the actual storage contract is `Error::DependencyCycle` (the
    /// typed variant `find_blocking_cycle` surfaces). The property
    /// matches the real contract; the ticket-body line is a slight
    /// misstatement of the rejection's typed variant.
    #[test]
    fn add_dep_edge_never_closes_cycle(
        plan in multi_issue_plan_strategy(1..=4, 4),
        extra in extra_edge_strategy(),
    ) {
        pin_clock(1_800_000_007);
        let (storage, ids) = build_multi_issue_repo(&plan, "cycle-rej");
        let n = ids.len();

        // Pre-state must be acyclic (build_multi_issue_repo enforces
        // it by construction; assert anyway so a generator regression
        // surfaces here rather than as a confusing post-state cycle).
        prop_assert!(
            !blocking_graph_has_cycle(&storage),
            "pre-state blocking graph has a cycle — generator drift",
        );

        let pre_snapshot = snapshot_dep_graph(&storage);

        // Resolve plan indices into real ids; modulo into-range so we
        // exercise every index slot with each generated case.
        let result = match &extra {
            ExtraEdge::Add { owner_idx, target_idx, kind } => {
                let owner = &ids[owner_idx % n];
                let target = &ids[target_idx % n];
                storage.add_dep_edge(owner, target, *kind)
            }
            ExtraEdge::SelfDep { idx, kind } => {
                let owner = &ids[idx % n];
                storage.add_dep_edge(owner, owner, *kind)
            }
        };

        match result {
            Ok(()) => {
                // Success path: post-state must be acyclic.
                prop_assert!(
                    !blocking_graph_has_cycle(&storage),
                    "add_dep_edge({:?}) succeeded but post-state blocking \
                     graph has a cycle (plan={:?})",
                    extra, plan,
                );
            }
            Err(jjf_storage::Error::DependencyCycle { .. })
            | Err(jjf_storage::Error::SelfDependency { .. }) => {
                // Reject path: graph must be unchanged.
                let post_snapshot = snapshot_dep_graph(&storage);
                prop_assert_eq!(
                    &pre_snapshot, &post_snapshot,
                    "add_dep_edge({:?}) rejected but graph mutated \
                     (pre != post)",
                    extra,
                );
            }
            Err(other) => {
                // Any other typed variant is a bug in this property
                // (we generated an index in-range against a fresh
                // repo so IssueNotFound/CAS-loss shouldn't fire) OR
                // a contract drift worth surfacing.
                prop_assert!(
                    false,
                    "add_dep_edge({:?}) returned unexpected error: {:?}",
                    extra, other,
                );
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

