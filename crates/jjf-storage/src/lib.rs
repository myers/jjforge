//! `jjf-storage` — the **write path** of jjforge's on-disk storage
//! layer.
//!
//! This crate implements the 4-CLI working-copy dance that lands a
//! single issue mutation as one commit on the `issues` bookmark, with a
//! `Jjf-Op:` trailer (and an accompanying `Jjf-Issue:` trailer)
//! recording what changed. The on-disk schema is pinned by
//! `docs/storage-format.md` v2.
//!
//! # The dance
//!
//! For one mutation:
//!
//! ```text
//! jj new bookmarks(issues) -m '<msg with trailers>'
//! <edit issues/<id>.json (and issues/<id>.comments.jsonl if applicable)>
//! jj bookmark set issues -r @ --allow-backwards
//! jj new root()
//! ```
//!
//! Step 4 steps `@` off the bookmark so the next mutation's `jj new
//! bookmarks(issues)` doesn't snapshot the previous edit into a stale
//! working copy. Lifted directly from `experiments/jj-shellout-hello/`.
//!
//! # v1 → v2 migration
//!
//! v1 spelled this bookmark `bugs` and put records under `bugs/`. v2
//! uses `issues` and `issues/`. The trailer field that names the issue
//! id was `Jjf-Bug:` in v1 and is `Jjf-Issue:` in v2. The parser in
//! [`crate::trailer`] reads either spelling so existing op chains
//! continue to replay; the writer here only emits the v2 spelling.
//!
//! Storage automatically detects a v1-shape repo (the `bugs` bookmark
//! exists; `issues` does not) and runs an inline migration on the
//! next [`Storage::open`] or [`Storage::init`] call. The migration
//! lands one commit on the new `issues` bookmark that renames every
//! `bugs/<id>.json` → `issues/<id>.json` (and the `.comments.jsonl`
//! sibling), then deletes the v1 `bugs` bookmark. See [`Storage::open`]
//! for the detection logic; the migration itself is in
//! [`Storage::migrate_v1_to_v2`].
//!
//! # Out of scope
//!
//! - The merge driver (`jjf-merge`).
//! - The `jjf` binary (mvp-cli).
//! - The `comments.jsonl` merge policy.
//!
//! # In scope (now landed)
//!
//! - Write path: [`Storage::create_issue`], the mutators, [`Storage::add_comment`].
//! - Read path: [`Storage::read`], [`Storage::read_history`].
//! - Bookmark bootstrap: [`Storage::init`] (idempotent; the `mvp-cli`
//!   `jjf init` verb is a thin wrapper).
//!
//! # Verdict pins
//!
//! - `2130de1` — shell out to `jj`; do not link `jj-lib`.
//! - `a60bb95` — `Jjf-Op:` trailers are the audit surface.
//! - `dcd4b57` — dedicated bookmark for issue data.

#![forbid(unsafe_code)]

mod cache;
mod git;
mod history;
mod id;
mod jj;
mod memory;
mod merge_ops;
mod migrate_v2_v3;
mod op;
mod read;
mod record;
mod sync_v3;
mod trailer;
mod v3_write;

use std::path::{Path, PathBuf};

pub use cache::UnreadableRef;
pub use git::GitError;
pub use history::HistoryEntry;
pub use id::{IdError, IssueId};
pub use jj::JjError;
pub use memory::slugify;
pub use merge_ops::{MergeReport, MergedIssue};
pub use op::Op;
pub use sync_v3::{PullReportV3, PushReportV3};

/// Free-function entry point for the v3 push transport. Bypasses
/// `Storage::open`'s mode dispatch so the CLI's `pull` verb can run on
/// a fresh clone (which has no v3 sentinel and no v2 bookmark — but
/// pull is precisely the verb that materializes those).
///
/// Wraps [`Storage::push_v3`] semantically — same refspec, same return
/// shape — but takes a repo-root path and skips the mode probe so
/// callers that want git-only transport without committing to a
/// Storage handle (the fresh-clone bootstrap case) can avoid the
/// preflight.
///
/// `repo_root` must be an absolute path to a colocated jj+git repo (or
/// any directory `git -C <root>` will accept as a repo root, including
/// a bare clone). See [`Storage::push_v3`] for the production-call
/// path; this is the bootstrap variant.
pub fn push_v3_bare(repo_root: &Path, remote: &str) -> Result<PushReportV3> {
    let git = git::GitRepo::open(repo_root.to_owned());
    sync_v3::push_v3(&git, remote)
}

/// Free-function entry point for the v3 pull transport. See
/// [`push_v3_bare`] for the design rationale.
///
/// Returns a [`PullReportV3`] with per-scenario counts. Notably, on a
/// fresh clone whose remote has a `refs/jjf/meta/format-version`
/// sentinel, this function will land that sentinel locally as part of
/// the reconcile (scenario 1 = new-local = copy), so a subsequent
/// `Storage::open` against the same root sees the repo as v3-shape and
/// the migrator skips.
pub fn pull_v3_bare(repo_root: &Path, remote: &str) -> Result<PullReportV3> {
    let git = git::GitRepo::open(repo_root.to_owned());
    sync_v3::pull_v3(&git, remote)
}
pub use record::{
    Comment, DepEdge, DepKind, Issue, IssueDraft, IssueRecord, IssueType, Memory, Status,
};

// `MatchedField`, `SearchHit`, `make_snippet`, and
// `DEFAULT_SNIPPET_CONTEXT` are declared inline below alongside
// `Storage::search`. We re-state the export here for discoverability;
// public types live at the crate root.

// `StaleHit` is declared inline below alongside `Storage::stale`.
// Same discoverability note as `SearchHit`.

// `ReadyFilter` is declared below alongside its helpers, but we
// re-state the export here for discoverability. Public types live
// at the crate root.

/// Field-update bundle for [`Storage::update`].
///
/// Each `Option` field is "do not touch" when `None`. A populated
/// variant means the corresponding scalar should be replaced with the
/// supplied value. The `assignee` field is double-wrapped on purpose:
/// `None` leaves the assignee alone, `Some(None)` clears it (writes
/// `null`), `Some(Some(name))` sets it to `name`. The two distinct
/// "set" intents (assign vs. unset) preserve the existing
/// `set_assignee(Option<&str>)` semantics without sacrificing the
/// "leave alone" outcome.
///
/// All populated fields land as ops in a single commit, per spec §5.5
/// (multi-op-per-commit). An `UpdateFields` with every field `None`
/// is a programming error and surfaces as
/// [`Error::Invalid`] — callers (notably the `jjf update` CLI) should
/// reject the empty-bundle case at their own layer with a more
/// targeted message before reaching here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpdateFields {
    pub title: Option<String>,
    pub slug: Option<Option<String>>,
    pub status: Option<Status>,
    pub type_: Option<Option<IssueType>>,
    pub body: Option<String>,
    pub assignee: Option<Option<String>>,
}

impl UpdateFields {
    /// `true` iff every field is `None` — i.e. there's nothing to do.
    /// Cheap helper so the CLI doesn't have to repeat the pattern.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.slug.is_none()
            && self.status.is_none()
            && self.type_.is_none()
            && self.body.is_none()
            && self.assignee.is_none()
    }
}

use jj::JjRepo;

/// What went wrong on the write path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("jj cli: {0}")]
    Jj(#[from] JjError),
    /// A `git` subprocess failure on the v3 write path. Distinct from
    /// [`Error::Jj`] so callers can tell "the jj dance failed" from
    /// "the git-only write path failed" — same shape (typed stderr +
    /// status), different CLI.
    ///
    /// Concurrent-write conflicts on the v3 path don't surface here;
    /// they get translated to [`Error::ConcurrentWrite`] before the
    /// raw [`GitError`] would bubble up. This variant only carries
    /// non-CAS git failures (network, parse, missing object, etc.).
    /// v3-storage (`docs/storage-out-of-tree.md`, ticket `eb42f50`).
    #[error("git cli: {0}")]
    Git(#[from] GitError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("issue not found in working copy: {0}")]
    IssueNotFound(IssueId),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("clock: {0}")]
    Clock(String),
    /// The repo root passed to `Storage::init` / `Storage::open` does not
    /// hold a jj repo. Distinct from `Jj` so callers can tell "not a
    /// repo at all" from "jj broke for some other reason" without
    /// string-matching stderr.
    #[error("not a jj repo: {0}")]
    NotAJjRepo(PathBuf),

    /// The v3 sentinel ref `refs/jjf/meta/format-version` exists but
    /// doesn't point at a commit (e.g. it was hand-wired with
    /// `git update-ref` to a blob, tree, or tag oid). The docstring on
    /// `detect_storage_mode` declares "Returns V3 iff the sentinel
    /// ref resolves to a commit"; this variant is the typed surface
    /// of that contract failing.
    ///
    /// Runtime failure (exit 1 via the CLI envelope), not preflight:
    /// the operator typed a well-formed command and the on-disk repo
    /// is in an inconsistent state. The `oid` and `kind` carry git's
    /// own classification (`blob` / `tree` / `tag`) so the operator
    /// can identify what was planted. Per ticket `de59159`.
    #[error(
        "format-version sentinel ref points at {kind} {oid}, expected commit"
    )]
    CorruptSentinel { oid: String, kind: String },

    /// A slug failed the v2.1 validation rules (charset, length,
    /// hyphen placement). Surfaced from `Storage::create_issue` /
    /// `Storage::update` whenever a non-`None` slug doesn't pass
    /// `validate_slug`.
    #[error("invalid slug {slug:?}: {reason}")]
    InvalidSlug {
        slug: String,
        reason: SlugInvalidReason,
    },

    /// A title contained a control character that would corrupt
    /// downstream surfaces (`jjf ls` text rows, JSON envelopes, the
    /// trailer payload), or was empty after trim. Surfaced from
    /// `Storage::create_issue`, `Storage::set_title`, and
    /// `Storage::update` whenever a candidate title doesn't pass
    /// `validate_title`. The `title` field carries the rejected
    /// value verbatim so a CLI error envelope can echo it back to
    /// the operator. v2.x (`qa-title-validation`).
    #[error("invalid title: {reason}")]
    InvalidTitle {
        title: String,
        reason: TitleInvalidReason,
    },

    /// A body (issue body or comment body) failed validation.
    /// Today the only failure mode is "too long" — the candidate
    /// body's raw UTF-8 byte length exceeded [`BODY_MAX_BYTES`]
    /// (65,536 bytes, matching GitHub's documented issue-body
    /// limit). Surfaced from `Storage::create_issue` (seed body),
    /// `Storage::set_body`, `Storage::update` (body field), and
    /// `Storage::add_comment` (comment body) whenever a candidate
    /// body doesn't pass [`validate_body`]. The CLI envelope kind
    /// is `body_too_large`. Issue `679444a` (QA red-team
    /// 2026-06-25 sub-pass 4).
    #[error("invalid body: {reason}")]
    InvalidBody { reason: BodyInvalidReason },

    /// Two open issues can't share a slug. Surfaced from
    /// `Storage::create_issue` / `Storage::update` when an attempted
    /// slug write collides with any existing issue's slug.
    /// `conflicts_with` carries the id of the issue already holding
    /// the slug — open, in-progress, blocked, OR closed — so the
    /// operator can disambiguate. Per spec v2.6 (issue `a105e0b`),
    /// closed issues retain their slug forever; a new ticket must
    /// pick a fresh one.
    #[error(
        "slug {slug:?} already in use by issue {conflicts_with}"
    )]
    SlugCollision {
        slug: String,
        conflicts_with: IssueId,
    },

    /// `Storage::resolve` was handed a string that isn't a valid id
    /// and doesn't match any open issue's slug. The handle is
    /// preserved so the CLI's `slug_not_found` error envelope can
    /// surface the operator-supplied value.
    ///
    /// Note: a 7-char hex handle never reaches this variant — it
    /// short-circuits via `IssueId::parse` and resolves to that id
    /// without an existence check. If the id doesn't actually exist
    /// on the bookmark, the subsequent `Storage::read` (or mutator)
    /// returns [`Error::IssueNotFound`] instead.
    #[error("no issue with handle {handle:?}")]
    SlugNotFound { handle: String },

    /// `Storage::claim` was called on an issue that's already
    /// `InProgress` with a DIFFERENT assignee. The `by` field is the
    /// existing assignee (per the read at the time of the claim).
    /// Surfaces from the storage-side preflight; the CLI's
    /// `already_claimed` error envelope carries it as `details.by`.
    /// v2.3 (`agent-claim-atomic`).
    #[error("issue already claimed by {by:?}")]
    AlreadyClaimed { by: String },

    /// `Storage::add_dep_edge` (and `create_issue` at draft-time)
    /// was asked to add an edge where the child and target are the
    /// same issue. A self-dep makes the issue permanently
    /// `blocks`-blocked by itself (a one-line DoS); the other dep
    /// kinds (parent-child, related, discovered-from) are nonsense
    /// applied to self. Reject all kinds at the boundary. The CLI
    /// envelope kind is `self_dependency`. v2.x
    /// (`qa-dep-validation`, issue `d1a01f0`).
    #[error("issue {id} cannot depend on itself")]
    SelfDependency { id: IssueId },

    /// `Storage::add_dep_edge` was asked to add a `blocks`-kind edge
    /// from `source` to `target` that would close a cycle in the
    /// blocks-graph. The check walks forward from `target` over
    /// existing `blocks` edges; if `source` is reachable, landing the
    /// new edge would create a back-edge. Issues caught in a `blocks`
    /// cycle are permanently invisible to `jjf ready` (every node in
    /// the cycle has at least one active blocks-dep), so the boundary
    /// rejects the write rather than land a silent landmine.
    ///
    /// `cycle` is the path that would close: `[target, ..., source]`.
    /// Adding `source -> target` would extend it to
    /// `[source, target, ..., source]`. The CLI's
    /// `dependency_cycle` envelope echoes it back so the operator
    /// can see which existing edges are involved.
    ///
    /// Closed issues are still nodes in the graph — the walk doesn't
    /// short-circuit on `status == closed`. Reasoning: closing one
    /// node in a cycle doesn't unblock the others (closed deps don't
    /// block in `list_ready`, but a CLOSED dep target is still a real
    /// edge that participates in any future cycle the operator might
    /// inadvertently extend). Detecting cycles among closed nodes is
    /// also harmless: the operator either re-opens them (and the
    /// cycle becomes active) or leaves them alone (no edge ever
    /// lands). Either way, refusing the write is the safe move.
    ///
    /// The CLI envelope kind is `dependency_cycle`. v2.6
    /// (`dep-cycle-undetected`, issue `43c7615`).
    #[error(
        "adding blocks-edge {from} -> {target} would close a dependency cycle"
    )]
    DependencyCycle {
        /// The new edge's source (the issue being given a new dep).
        /// Named `from` to side-step thiserror's reserved `source`
        /// field name (which it treats as the chained-cause field).
        from: IssueId,
        target: IssueId,
        cycle: Vec<IssueId>,
    },

    /// A concurrent jjforge writer landed first and the 4-CLI dance's
    /// `jj new bookmarks(issues)` snapshot is now stale, surfacing
    /// from jj as an "Internal error: Failed to check out commit …
    /// Caused by: Concurrent checkout" cascade. Translated at the
    /// storage layer to a clean typed error rather than the raw
    /// jj-internal 12-line vomit.
    ///
    /// Non-slug-claim mutations (comments, updates, status changes)
    /// auto-retry once with a fresh head-commit before this surfaces;
    /// if the retry also races, the loser sees this error. Slug-claim
    /// creates do NOT retry — retrying would re-race the same slot
    /// indefinitely — and the post-failure probe upgrades the more
    /// specific race-with-known-winner case to
    /// [`Error::SlugCollision`].
    ///
    /// The `hint` field is a one-line operator-facing message; the
    /// CLI's `concurrent_write` envelope renders it verbatim.
    /// v2.x (`qa-concurrent-write-ux`, issue `277f559`).
    #[error("concurrent write conflict; {hint}")]
    ConcurrentWrite { hint: String },
}

/// Predicate on a [`crate::Error`]: does the underlying cause look
/// like a concurrent-write conflict (jj's "Concurrent checkout"
/// fingerprint), as opposed to its translated
/// [`Error::ConcurrentWrite`] form?
///
/// Used by the commit-dance translation in
/// [`Storage::commit_record_change`] / [`Storage::commit_memory_change`]
/// to decide whether to map the underlying [`Error::Jj`] to a typed
/// [`Error::ConcurrentWrite`] for downstream callers.
///
/// Also used by the caller-side retry helpers
/// ([`Storage::mutate`], [`Storage::add_comment`],
/// [`Storage::create_issue`]) which match on
/// [`Error::ConcurrentWrite`] directly after translation.
fn is_concurrent_write(e: &Error) -> bool {
    match e {
        Error::Jj(je) => je.is_concurrent_write(),
        // The v3 write path translates CAS failures to
        // `Error::ConcurrentWrite` at the boundary in `v3_write.rs`,
        // so raw `Error::Git` here represents a non-CAS git failure.
        // Returning `false` keeps the translation symmetric with the
        // v2 path (only the jj-side cascade is auto-recognized as
        // concurrent on the inner Result).
        Error::Git(_) => false,
        _ => false,
    }
}

/// Predicate on a [`crate::Error`]: is it the typed
/// [`Error::ConcurrentWrite`] surfaced by the translation layer? This
/// is what the higher-level retry helpers match on (they never see the
/// raw [`Error::Jj`] form — the translation in `commit_record_change`
/// has already happened).
fn is_typed_concurrent_write(e: &Error) -> bool {
    matches!(e, Error::ConcurrentWrite { .. })
}

/// Retry policy for CAS-loss conflicts. Read once per retry-driver
/// call from env so tests can pin specific behavior (e.g.
/// `JJF_RETRY_BASE_MS=0` to skip the wall-clock wait;
/// `JJF_MAX_RETRIES=0` to force first-conflict-wins).
///
/// `max_retries` is the number of RETRY attempts after the initial
/// try, so total attempts = `1 + max_retries`. A `max_retries` of 5
/// (the default) means 6 total attempts before giving up.
///
/// `base_ms` is the leading-edge backoff (the delay before the FIRST
/// retry). Subsequent delays grow geometrically (~2.5×) so the
/// schedule for `base_ms = 10` is 10, 25, 60, 150, 350 ms. When
/// `base_ms = 0`, no sleeps happen at all — useful in tests that
/// want the retry budget without the wall-clock wait.
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    max_retries: u32,
    base_ms: u64,
}

impl RetryPolicy {
    fn from_env() -> Self {
        let max_retries = std::env::var("JJF_MAX_RETRIES")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .map(|n| n.min(20))
            .unwrap_or(5);
        let base_ms = std::env::var("JJF_RETRY_BASE_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|n| n.min(10_000))
            .unwrap_or(10);
        RetryPolicy { max_retries, base_ms }
    }

    /// Geometric schedule with ~25% jitter. Attempt index is 1-based
    /// (the FIRST retry is attempt 1). The base series at base_ms=10
    /// is roughly [10, 25, 60, 150, 350]; jitter is added per call
    /// so concurrent racers don't re-collide on the same wake tick.
    fn sleep_before(&self, attempt: u32) {
        if self.base_ms == 0 {
            return;
        }
        // Geometric multipliers ~2.5× per step. Computed as a small
        // table so we don't depend on libm and don't drift on f64
        // rounding for the small attempt counts we use.
        let multipliers: [u64; 6] = [1, 2, 6, 15, 35, 85];
        let idx = (attempt as usize).saturating_sub(1).min(multipliers.len() - 1);
        let base_delay = self.base_ms.saturating_mul(multipliers[idx]);
        // ~25% jitter, derived from a cheap nondeterministic source
        // (wall-clock nanos XOR pid) so we don't pull in `rand`.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        // jitter span = 25% of base_delay, half subtractive / half additive
        let span = (base_delay / 4).max(1);
        let jitter = (u64::from(nanos ^ pid)) % (2 * span + 1);
        let delay = base_delay.saturating_add(jitter).saturating_sub(span);
        std::thread::sleep(std::time::Duration::from_millis(delay));
    }
}

/// Human-friendly hint for the `ConcurrentWrite` error after all
/// retries are exhausted. Mentions the actual retry budget so the
/// message stays honest when `JJF_MAX_RETRIES` is overridden.
fn retries_exhausted_hint(max_retries: u32) -> String {
    format!(
        "another writer landed first; retried {max_retries} time{plural} and still raced. \
         Retry your command.",
        plural = if max_retries == 1 { "" } else { "s" },
    )
}

/// Drive a CAS-loss retry loop: call `attempt` until it succeeds, a
/// non-conflict error fires, or the configured retry budget is
/// exhausted. Between attempts, invalidate the snapshot memo and
/// sleep per the [`RetryPolicy`].
///
/// The closure is called once for the initial try (attempt 1) and
/// then up to `max_retries` more times. After each ConcurrentWrite
/// failure, `between` runs (snapshot invalidation) and the policy
/// sleeps for the configured backoff.
fn run_with_retry<T, A, B>(policy: RetryPolicy, mut attempt: A, mut between: B) -> Result<T>
where
    A: FnMut() -> Result<T>,
    B: FnMut(),
{
    let mut last: Result<T> = attempt();
    for retry_n in 1..=policy.max_retries {
        match last {
            Ok(_) => return last,
            Err(ref e) if is_typed_concurrent_write(e) => {
                between();
                policy.sleep_before(retry_n);
                last = attempt();
            }
            Err(_) => return last,
        }
    }
    match last {
        Err(e) if is_typed_concurrent_write(&e) => Err(Error::ConcurrentWrite {
            hint: retries_exhausted_hint(policy.max_retries),
        }),
        other => other,
    }
}

/// Outcome of [`Storage::claim`]. Distinguishes "I wrote the claim
/// commit just now" from "the issue was already claimed by me; no
/// commit landed (idempotent)."
///
/// Callers that propose a fresh claim against an issue they don't yet
/// own (notably `jjf ready --claim`, where the ready filter excluded
/// claimed issues) need to know which case fired: an `AlreadyOurs`
/// against a freshly-picked ready id means the racer beat us to the
/// CAS, NOT that we genuinely re-claimed our own issue. The CLI
/// surfaces this via the `claim_race_lost` error envelope so the
/// orchestrator can retry against the next ready id.
///
/// Single-process same-user re-claim is still a quiet success; the
/// distinction only matters in the parallel-claim scenario, and the
/// CLI flag (`ready --claim`) is what gates whether `AlreadyOurs` is
/// surfaced as a race or absorbed as idempotent. v2.3-fix
/// (`a6b8fb7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimResult {
    /// A commit landed; we are the new claimant.
    Claimed,
    /// Same user already owned the issue; no commit landed.
    AlreadyOurs,
}

/// What [`Storage::mutate`]'s closure returns on each read-mutate-commit
/// attempt. Tri-state because the closure is the only place that has the
/// freshly-read record in scope, so it's the only place that can make
/// the right call about whether a write is still needed.
///
/// The retry contract: on a CAS-loss retry, `mutate` re-reads the
/// record and calls the closure AGAIN with the new state. That second
/// call MUST re-evaluate any domain precondition (e.g. "is the issue
/// still Open?" for claim) against the fresh record — not blindly
/// re-apply the same mutation. Issue `a6b8fb7`: pre-`a6b8fb7`, the
/// closure was a `Fn(&mut IssueRecord) -> Result<Vec<Op>>` and
/// preconditions were checked ONCE before `mutate` was entered; the
/// retry would re-run the closure against a record that no longer
/// satisfied the precondition, lands a duplicate write, and the verb
/// returned Ok despite the racer having taken the slot.
enum MutateOutcome {
    /// The closure mutated the record and wants these ops persisted.
    Write(Vec<Op>),
    /// The fresh record already satisfies the post-condition the caller
    /// wanted (the racer beat us to the same end-state we'd have
    /// written). No commit lands; the verb returns `Ok(())`.
    ///
    /// Used for idempotent same-user re-claims, double-unclaim of an
    /// already-unassigned issue, double-unblock of an already-open
    /// issue, etc.
    Skip,
    /// The fresh record violates a domain precondition. Surface this
    /// typed error to the caller instead of writing.
    ///
    /// Used when a racer took the slot we wanted (claim raced and the
    /// other claimant won; unclaim raced and the issue got closed; etc.).
    Conflict(Error),
}

/// Filter bundle for [`Storage::list_ready`].
///
/// All filters AND with the implicit "active + unblocked + unclaimed
/// + not-parked" criteria; within each filter axis the semantics
/// match `jjf ls`:
///
/// - `labels`: AND — an issue must carry EVERY listed label.
/// - `types`: OR — an issue's type must equal AT LEAST ONE listed
///   type. Empty filter accepts every type.
/// - `limit`: truncate the returned vec after the priority sort.
///   `None` means unlimited.
/// - `include_claimed`: when `false` (default, v2.3
///   `agent-claim-atomic`), [`Status::InProgress`] issues are
///   excluded — they're claimed and another agent shouldn't see
///   them as ready work. When `true`, InProgress issues are
///   included so an operator can see "what's in flight."
/// - `include_blocked`: when `false` (default, v2.5
///   `agent-await-gates-impl`), [`Status::Blocked`] issues are
///   excluded — they're parked on an external signal and an idle
///   agent shouldn't see them as workable. When `true`, Blocked
///   issues are included so an operator can see "what's parked."
///
/// The default value (`ReadyFilter::default()`) is "no extra
/// filters" — equivalent to `jjf ready` with no flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyFilter {
    pub labels: Vec<String>,
    pub types: Vec<IssueType>,
    pub limit: Option<usize>,
    /// v2.3 (`agent-claim-atomic`). When `false` (default),
    /// `Storage::list_ready` excludes [`Status::InProgress`] issues.
    pub include_claimed: bool,
    /// v2.5 (`agent-await-gates-impl`). When `false` (default),
    /// `Storage::list_ready` excludes [`Status::Blocked`] issues.
    pub include_blocked: bool,
}

/// Priority weight for `Storage::list_ready`'s primary sort key.
/// Lower number = higher priority. The order pins the agreed type
/// priority: bug > feature > research > epic > unspecified.
/// `Roadmap` is unreachable here (filtered out before sorting), but
/// has a stable weight so the function is total.
fn ready_priority(t: IssueType) -> u8 {
    match t {
        IssueType::Bug => 0,
        IssueType::Feature => 1,
        IssueType::Research => 2,
        IssueType::Epic => 3,
        IssueType::Unspecified => 4,
        // Roadmap is excluded upstream; the weight is arbitrary but
        // distinct so a future caller that lifts the filter doesn't
        // see it tie with anything actionable.
        IssueType::Roadmap => 5,
    }
}

/// Label intersection helper shared between `list_ready` and any
/// future filter caller. Empty wanted = match every issue.
fn labels_match_all(issue_labels: &[String], wanted: &[String]) -> bool {
    wanted.iter().all(|w| issue_labels.iter().any(|l| l == w))
}

/// Compute the blocked set across all issues — the fixpoint
/// implementation for the v2.4 dep-edge model
/// (`agent-dep-types`). An issue X is blocked iff at least one of:
///
/// - X has a `blocks`-kind dep edge whose target is an OPEN or
///   IN-PROGRESS issue (closed and dangling targets don't block).
/// - X has a `parent-child`-kind dep edge whose target is OPEN/
///   IN-PROGRESS AND target itself is BLOCKED. (Closed parents
///   don't cascade; the cascade follows the parent's blocked-ness,
///   not just its status. An open-and-not-blocked parent does NOT
///   block its children.)
///
/// `related` and `discovered-from` edges are ignored entirely.
///
/// Cycle handling: a `parent-child` cycle (e.g. A→B and B→A where
/// neither is closed) treats every node in the cycle as NOT BLOCKED
/// via the cascade — the cascade rule "X is blocked iff parent is
/// blocked" only fires when the parent is independently blocked
/// (typically by a `blocks` edge). A pure cycle with no external
/// blocker has nothing to propagate; the fixpoint converges with
/// every node out of the cascade. Each node can still be blocked
/// independently via its own `blocks` edges. Documented in the
/// commit message and in `docs/storage-format.md` §3.x.
///
/// Returns the set of issue ids that are blocked.
fn compute_blocked_set(all: &[Issue]) -> std::collections::HashSet<IssueId> {
    use std::collections::{HashMap, HashSet};

    let by_id: HashMap<IssueId, &Issue> = all.iter().map(|i| (i.id.clone(), i)).collect();

    // Helper: is `target` active (open, blocked, or in-progress)?
    // Closed, Abandoned, and dangling targets are NOT active and
    // never block. A `Blocked` target (v2.5) is still ACTIVE — it's
    // parked on an external signal, not done. A dep on a blocked
    // issue still blocks the dependent (the work isn't complete).
    // An `Abandoned` target (v2.7) behaves like Closed: the work
    // will never be done so dependents are free of it. (Operators
    // reviving an abandoned dep via `jjf update --status open` will
    // see the dependent fall out of the ready set again.)
    let is_active = |target: &IssueId| -> bool {
        match by_id.get(target) {
            Some(i) => match i.status {
                Status::Open | Status::Blocked | Status::InProgress => true,
                Status::Closed | Status::Abandoned => false,
            },
            None => false, // dangling
        }
    };

    let mut blocked: HashSet<IssueId> = HashSet::new();

    // Seed: every issue with a `blocks` edge pointing at an active
    // target is blocked. (The `blocks`-edge rule is non-cascading;
    // we can resolve it in one pass.)
    for issue in all {
        for edge in &issue.dependencies {
            if edge.kind == DepKind::Blocks && is_active(&edge.target) {
                blocked.insert(issue.id.clone());
                break;
            }
        }
    }

    // Fixpoint: propagate via `parent-child` edges. X is blocked if
    // any parent (target of a `parent-child` edge) is BLOCKED and
    // active. Iterate until no changes — bounded by issue count.
    loop {
        let mut changed = false;
        for issue in all {
            if blocked.contains(&issue.id) {
                continue;
            }
            for edge in &issue.dependencies {
                if edge.kind == DepKind::ParentChild
                    && is_active(&edge.target)
                    && blocked.contains(&edge.target)
                {
                    blocked.insert(issue.id.clone());
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }

    blocked
}

/// Type union helper. Empty wanted = match every type.
fn types_match_any(issue_type: IssueType, wanted: &[IssueType]) -> bool {
    wanted.is_empty() || wanted.iter().any(|t| *t == issue_type)
}

/// Which field on an issue the search query first matched against.
/// Mirrors the priority order [`Storage::search`] uses when an issue
/// hits in more than one field: title beats body, body beats comments.
/// The CLI's `--json` envelope serializes this as a lowercase string
/// (`"title"` / `"body"` / `"comments"`); the plain-text row uses the
/// same spelling so the column is stable across both shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchedField {
    /// The issue's title carried the first hit.
    Title,
    /// The issue's body carried the first hit (and the title didn't).
    Body,
    /// One of the issue's comment bodies carried the first hit (and
    /// neither title nor body did). Only reachable when the search
    /// was invoked with `include_comments = true`.
    Comments,
}

impl MatchedField {
    /// Wire spelling used by the `jjf search` plain-text row and the
    /// JSON envelope's `matched_field` key. Lowercase to match
    /// [`Status::as_str`] / [`IssueType::as_str`]'s shape.
    pub fn as_str(self) -> &'static str {
        match self {
            MatchedField::Title => "title",
            MatchedField::Body => "body",
            MatchedField::Comments => "comments",
        }
    }
}

/// One hit returned by [`Storage::search`].
///
/// `score` is the total occurrence count of the (case-insensitive)
/// substring across every searched field on the issue — title, body,
/// and (when `include_comments = true`) every comment body. Used by
/// the CLI to rank multi-field hits above single-field hits without
/// bringing in a real BM25/TF-IDF surface.
///
/// `matched_field` is the field where the FIRST hit was discovered,
/// using the priority order `Title > Body > Comments`. When an issue
/// hits in more than one field, the more specific surface wins; e.g.
/// "foo" hitting both title and body reports `Title`. Single-field
/// hits report whichever field carried the match.
///
/// `snippet` is the rendered preview around the first hit on the
/// matched field. See [`Storage::search`] for the exact windowing
/// rules. Newlines and tabs in the source field are normalized to
/// single spaces so the CLI's tab-separated plain-text row stays one
/// line; leading/trailing `…` (U+2026) marks truncation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SearchHit {
    /// The matching issue (full read-side projection — same shape
    /// [`Storage::read`] returns).
    pub issue: Issue,
    /// Where the first hit was found. See type-level docs.
    pub matched_field: MatchedField,
    /// Total occurrence count across every searched field.
    pub score: usize,
    /// Preview of the matched field around the first hit.
    pub snippet: String,
}

/// Default half-width of the snippet window built by
/// [`Storage::search`] — `±40` chars around the first hit. Matches
/// the value the ticket calls out as the default.
pub const DEFAULT_SNIPPET_CONTEXT: usize = 40;

/// One row returned by [`Storage::stale`].
///
/// `seconds_since_update` is the wall-clock delta between `now` (the
/// pinned/system clock — see [`Storage::stale`]) and the issue's
/// `updated_at` field. Carried alongside the [`Issue`] so the CLI's
/// `--json` envelope can serialize `days_since_update` without a
/// second clock read at the render layer (any re-derivation would
/// race the pinned clock contract under `JJF_TEST_CLOCK_SECS`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleHit {
    /// The matching issue (full read-side projection — same shape
    /// [`Storage::read`] returns).
    pub issue: Issue,
    /// `now - updated_at` in whole seconds. Always positive when the
    /// issue was returned (the staleness filter excludes any issue
    /// whose `updated_at` is at-or-after `now - threshold_secs`).
    pub seconds_since_update: u64,
}

/// Count non-overlapping occurrences of `needle` (already lowercased
/// by the caller) in `haystack`, case-insensitively. Used by
/// [`Storage::search`] to compute per-field hit counts.
///
/// The implementation walks `haystack.to_lowercase()` byte by byte,
/// stepping past each match by `needle.len()`. Non-overlapping
/// counting matches the way a reader would tally "how many times does
/// X show up here" — overlapping matches (e.g. "aa" in "aaaa")
/// surface as 2, not 3.
///
/// Returns 0 on an empty needle (the caller in [`Storage::search`]
/// rejects empty queries up front; this is the defensive guard).
fn count_ci(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let h = haystack.to_lowercase();
    let mut count = 0;
    let mut start = 0;
    while let Some(off) = h[start..].find(needle) {
        count += 1;
        start += off + needle.len();
        if start > h.len() {
            break;
        }
    }
    count
}

/// Build the snippet preview around the FIRST case-insensitive hit of
/// `needle` (already lowercased) in `haystack`. The window is
/// `±context` chars on either side of the hit; bytes outside the
/// window are dropped, with a leading `…` if the window doesn't start
/// at the beginning and a trailing `…` if it doesn't end at the end.
///
/// Char-boundary safe: we walk char indices, not bytes, so a snippet
/// landing in the middle of a multibyte UTF-8 sequence still slices
/// at a valid boundary. The CLI tab-separated row stays one line —
/// embedded newlines and tabs in the source field are replaced with
/// single ASCII spaces before windowing (so column count is stable).
///
/// Returns the empty string if the needle isn't found (defensive —
/// the caller in [`Storage::search`] already verified `count_ci > 0`
/// before calling this).
pub fn make_snippet(haystack: &str, needle: &str, context: usize) -> String {
    if needle.is_empty() {
        return String::new();
    }
    // Normalize newlines/tabs before searching. Matching against the
    // lowercase-normalized form keeps offsets in step.
    let cleaned: String = haystack
        .chars()
        .map(|c| if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c })
        .collect();
    let lower = cleaned.to_lowercase();
    let Some(byte_idx) = lower.find(needle) else {
        return String::new();
    };

    // Translate the byte offset (into `lower`, which has the same
    // structure as `cleaned` byte-for-byte for the ASCII range we
    // care about for offsets) into a char index in `cleaned`. The
    // ASCII assumption is safe here only when matches stay ASCII;
    // for the multibyte case we fall through to a char-walk that
    // works regardless.
    let mut chars_iter = cleaned.char_indices();
    let mut hit_char_idx = 0usize;
    for (cidx, (bidx, _ch)) in (&mut chars_iter).enumerate() {
        if bidx == byte_idx {
            hit_char_idx = cidx;
            break;
        }
        if bidx > byte_idx {
            hit_char_idx = cidx.saturating_sub(1);
            break;
        }
    }

    // Compute the char-index window. Use char counts (not bytes) so
    // multibyte content gets the documented context width.
    let total_chars = cleaned.chars().count();
    let start_char = hit_char_idx.saturating_sub(context);
    let end_char = (hit_char_idx + needle.chars().count() + context).min(total_chars);

    // Materialize the substring via char_indices to stay safe on
    // multibyte boundaries.
    let mut start_byte = cleaned.len();
    let mut end_byte = cleaned.len();
    for (cidx, (bidx, _)) in cleaned.char_indices().enumerate() {
        if cidx == start_char {
            start_byte = bidx;
        }
        if cidx == end_char {
            end_byte = bidx;
            break;
        }
    }
    // If we never set end_byte (window runs to the end), keep the
    // default (cleaned.len()).
    let middle = &cleaned[start_byte..end_byte];

    let prefix = if start_char > 0 { "…" } else { "" };
    let suffix = if end_char < total_chars { "…" } else { "" };
    format!("{prefix}{middle}{suffix}")
}

/// Why a slug failed validation. Each variant maps to one of the
/// rules in `Storage::validate_slug`; the CLI's JSON error envelope
/// surfaces the variant name (lowercase snake_case) in
/// `details.reason` so scripts can branch without parsing the
/// human message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlugInvalidReason {
    /// Slug contained characters outside `[a-z0-9-]`.
    BadCharset,
    /// Slug was shorter than 3 characters.
    TooShort,
    /// Slug was longer than 48 characters.
    TooLong,
    /// Slug began with `-`.
    LeadingHyphen,
    /// Slug ended with `-`.
    TrailingHyphen,
    /// Slug contained `--` (two consecutive hyphens).
    ConsecutiveHyphens,
}

impl SlugInvalidReason {
    /// Stable lowercase snake_case name. Used by the CLI to surface
    /// the rejection reason in the JSON error envelope.
    pub fn as_str(self) -> &'static str {
        match self {
            SlugInvalidReason::BadCharset => "bad_charset",
            SlugInvalidReason::TooShort => "too_short",
            SlugInvalidReason::TooLong => "too_long",
            SlugInvalidReason::LeadingHyphen => "leading_hyphen",
            SlugInvalidReason::TrailingHyphen => "trailing_hyphen",
            SlugInvalidReason::ConsecutiveHyphens => "consecutive_hyphens",
        }
    }
}

impl std::fmt::Display for SlugInvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            SlugInvalidReason::BadCharset => {
                "slug must match [a-z0-9-]+ (no uppercase, no other punctuation)"
            }
            SlugInvalidReason::TooShort => "slug must be at least 3 characters",
            SlugInvalidReason::TooLong => "slug must be at most 48 characters",
            SlugInvalidReason::LeadingHyphen => "slug must not start with `-`",
            SlugInvalidReason::TrailingHyphen => "slug must not end with `-`",
            SlugInvalidReason::ConsecutiveHyphens => {
                "slug must not contain `--` (consecutive hyphens)"
            }
        };
        f.write_str(msg)
    }
}

/// Why a title failed validation. Each variant maps to one of the
/// rules in [`validate_title`]; the CLI's JSON error envelope
/// surfaces the variant name (lowercase snake_case) in
/// `details.reason` so scripts can branch without parsing the
/// human message.
///
/// Added in `qa-title-validation` (QA red-team 2026-06-23). The
/// goal is to reject — at the CLI/storage boundary — any title
/// that would corrupt downstream surfaces. Two of the rejections
/// are immediate data-loss / corruption defects: an embedded
/// `\0` is silently truncated by the JSON-as-C-string round-trip
/// somewhere in the write path; an embedded `\n` (or `\r`) breaks
/// the tab-separated `jjf ls` / `jjf ready` text row format. Tabs
/// (`\t`) ALSO break that text row format (the row separator IS a
/// tab), so we reject them under [`TitleInvalidReason::ControlChar`]
/// for consistency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleInvalidReason {
    /// Title was empty or whitespace-only after trim.
    Empty,
    /// Title contained `\n` (line feed) or `\r` (carriage return).
    /// The tab-separated `jjf ls` row format has no escape rule, so a
    /// newline splits one row across multiple lines and breaks
    /// downstream pipelines.
    Newline,
    /// Title contained a `\0` (null byte). Hits a silent-truncation
    /// path on the write side — the on-disk title would be the
    /// substring up to (but not including) the null. Reject at the
    /// boundary to prevent data loss.
    NullByte,
    /// Title contained any other control character (per
    /// `char::is_control`) that isn't already covered by
    /// [`TitleInvalidReason::Newline`] or
    /// [`TitleInvalidReason::NullByte`]. Tabs (`\t`, U+0009) land
    /// here because they break the `jjf ls` row format too. The
    /// `codepoint` field carries the offending Unicode scalar so
    /// the operator can tell which control char tripped the
    /// rejection.
    ControlChar { codepoint: u32 },
}

impl TitleInvalidReason {
    /// Stable lowercase snake_case name. Used by the CLI to surface
    /// the rejection reason in the JSON error envelope. The
    /// `ControlChar` variant exposes its codepoint via the
    /// `codepoint` key alongside `reason` in the envelope; the
    /// `as_str` mapping is just the variant tag.
    pub fn as_str(self) -> &'static str {
        match self {
            TitleInvalidReason::Empty => "empty",
            TitleInvalidReason::Newline => "newline",
            TitleInvalidReason::NullByte => "null_byte",
            TitleInvalidReason::ControlChar { .. } => "control_char",
        }
    }
}

impl std::fmt::Display for TitleInvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TitleInvalidReason::Empty => f.write_str("title must not be empty"),
            TitleInvalidReason::Newline => {
                f.write_str("title must not contain newline (\\n) or carriage return (\\r)")
            }
            TitleInvalidReason::NullByte => {
                f.write_str("title must not contain a null byte (\\0)")
            }
            TitleInvalidReason::ControlChar { codepoint } => {
                write!(f, "title must not contain control character U+{:04X}", codepoint)
            }
        }
    }
}

/// Validate a title per the rules pinned in `qa-title-validation`:
///
/// - Must be non-empty after `trim` (the existing "empty title"
///   rule, now folded into a typed reason).
/// - Must not contain `\n` (U+000A) or `\r` (U+000D) — these break
///   the tab-separated `jjf ls` / `jjf ready` text row format.
/// - Must not contain `\0` (U+0000) — embedded nulls hit a silent
///   truncation path between argv parsing and on-disk storage.
/// - Must not contain any other control character per
///   `char::is_control` (tabs included — `\t` is the row separator
///   in `jjf ls` text output, so it breaks parsing too).
///
/// Returns `Ok(())` if every rule passes; otherwise the first
/// failing rule's typed reason. The check order is: empty, then
/// scan characters left-to-right reporting the first control
/// character.
///
/// Exposed publicly so the CLI can pre-validate before calling
/// `Storage::create_issue` / `Storage::update` and surface a typed
/// `invalid_title` exit-2 error before any IO kicks off.
pub fn validate_title(title: &str) -> std::result::Result<(), TitleInvalidReason> {
    if title.trim().is_empty() {
        return Err(TitleInvalidReason::Empty);
    }
    for c in title.chars() {
        match c {
            '\n' | '\r' => return Err(TitleInvalidReason::Newline),
            '\0' => return Err(TitleInvalidReason::NullByte),
            c if c.is_control() => {
                return Err(TitleInvalidReason::ControlChar {
                    codepoint: c as u32,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Maximum byte length of an issue body or a comment body.
///
/// Matches GitHub's documented issue-body limit (65,536 characters).
/// GitHub's underlying MySQL column is a `mediumblob` (262,144 bytes
/// of capacity), but the public surface caps at 65,536 characters —
/// which for ASCII-only content is 65,536 bytes. We measure raw
/// UTF-8 byte length here so the cap is unambiguous across multi-
/// byte content (a body that's 65,537 bytes is rejected even if it's
/// fewer than 65,536 Unicode scalars). Picking the GitHub/Forgejo
/// number means every consumer of the prior art already knows what
/// the limit is; integrations don't have to learn a jjforge-specific
/// constant.
///
/// Applied at every entry point that mints a body: `create_issue`'s
/// seed body, `set_body`, `update`'s body field, and `add_comment`.
pub const BODY_MAX_BYTES: usize = 65_536;

/// Why a body failed validation. The enum is left open-ended so
/// future rules (e.g. control-character bans) can extend it without
/// changing the [`validate_body`] signature. As of issue
/// `679444a` the only variant is [`BodyInvalidReason::TooLong`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyInvalidReason {
    /// Body exceeded [`BODY_MAX_BYTES`] in raw UTF-8 byte length.
    /// `limit` is the configured cap (always `BODY_MAX_BYTES` today);
    /// `got` is the actual measured length of the offending body.
    /// Both are exposed in the CLI error envelope's `details` for
    /// scripted callers.
    TooLong { limit: usize, got: usize },
}

impl BodyInvalidReason {
    /// Stable lowercase snake_case name. Used by the CLI to surface
    /// the rejection reason in the JSON error envelope's
    /// `details.reason` slot when richer typing is needed; today the
    /// `body_too_large` kind has only one reason so the CLI flattens
    /// `limit` and `got` directly into `details`.
    pub fn as_str(self) -> &'static str {
        match self {
            BodyInvalidReason::TooLong { .. } => "too_long",
        }
    }
}

impl std::fmt::Display for BodyInvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BodyInvalidReason::TooLong { limit, got } => {
                write!(f, "body exceeds {limit} bytes (got {got})")
            }
        }
    }
}

/// Validate an issue body or a comment body against the
/// [`BODY_MAX_BYTES`] cap. Returns `Ok(())` if the body fits;
/// otherwise [`BodyInvalidReason::TooLong`] with the limit and the
/// measured byte length.
///
/// Measurement uses `body.len()` — raw UTF-8 byte length. Not
/// character count (would mean re-deciding what "character" means —
/// scalar, grapheme, displayed-width?), not grapheme cluster count
/// (the public cap is a byte-storage budget, not a typographic one),
/// not after-trim (whitespace-only padding is still bytes on disk).
///
/// Exposed publicly so the CLI can pre-validate before calling
/// `Storage::create_issue` / `Storage::set_body` / `Storage::update`
/// / `Storage::add_comment` — letting the CLI surface a typed exit-2
/// error before any IO kicks off. Storage re-validates at the
/// boundary so programmatic callers (a future MCP server, a Python
/// client) can't bypass the cap.
pub fn validate_body(body: &str) -> std::result::Result<(), BodyInvalidReason> {
    let got = body.len();
    if got > BODY_MAX_BYTES {
        return Err(BodyInvalidReason::TooLong {
            limit: BODY_MAX_BYTES,
            got,
        });
    }
    Ok(())
}

/// Reject `\n` (U+000A) and `\r` (U+000D) in a free-form string
/// that lands in a `Jjf-*:` trailer payload. Used as a write-boundary
/// guard for fields the spec treats as opaque text but the trailer
/// block treats as line-delimited: every embedded newline would split
/// the value into a new trailer line, opening an op-injection vector
/// (`qa-trailer-injection`, issue `a902492`).
///
/// Fields covered by this guard:
///
/// - `assignee` (Jjf-Assignee trailer payload)
/// - `label` (Jjf-Label trailer payload on `label-add` / `label-rm`)
///
/// `block-reason` and `title` have their own (slightly different)
/// validators; slug and memory-key already constrain charset to
/// `[a-z0-9-]+` so newlines are structurally impossible. Memory value
/// is sanitized at the writer (lf → space, plus trunc); the bare
/// trailer-block parser is line-oriented (Rust's `str::lines()` only
/// breaks on `\n` and `\r\n`), but we hold the line at "no `\n` or
/// `\r` at all" so a future permissive parser, a hand-grep, or a third-
/// party tool can't be fooled.
///
/// Returns `Ok(())` if neither `\n` nor `\r` appears; otherwise
/// `Err(())` — the caller maps it to its preferred typed error (per
/// field; the storage layer wraps with [`Error::Invalid`] today). The
/// helper deliberately doesn't reject other control characters: a
/// label like `wip:\t` is ugly but doesn't inject a trailer, and the
/// assignee field is free-form by spec. Title-style strictness is too
/// strong here; trailer-injection is the specific defense we want.
pub fn validate_no_newlines(s: &str) -> std::result::Result<(), ()> {
    if s.contains('\n') || s.contains('\r') {
        return Err(());
    }
    Ok(())
}

/// Minimum slug length (inclusive). Spec v2.1 §3.1.
pub const SLUG_MIN_LEN: usize = 3;

/// Maximum slug length (inclusive). Spec v2.1 §3.1.
pub const SLUG_MAX_LEN: usize = 48;

/// Validate a slug per spec v2.1 §3.1 rules:
///
/// - Charset: `[a-z0-9-]+`.
/// - Length: `[SLUG_MIN_LEN, SLUG_MAX_LEN]`.
/// - No leading hyphen.
/// - No trailing hyphen.
/// - No two consecutive hyphens.
///
/// Returns `Ok(())` if every rule passes; otherwise
/// `Err(SlugInvalidReason)` for the first rule that fails. Order
/// of checks: length-too-short (so empty strings get the right
/// reason rather than the catchall length one), then charset, then
/// hyphen placement, then length-too-long.
///
/// Exposed publicly so the CLI can pre-validate before calling
/// `Storage::update` — letting the CLI surface a typed exit-2
/// error rather than the (correct but generic) storage-side bounce.
pub fn validate_slug(slug: &str) -> std::result::Result<(), SlugInvalidReason> {
    if slug.len() < SLUG_MIN_LEN {
        return Err(SlugInvalidReason::TooShort);
    }
    if slug.len() > SLUG_MAX_LEN {
        return Err(SlugInvalidReason::TooLong);
    }
    if !slug.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-')) {
        return Err(SlugInvalidReason::BadCharset);
    }
    if slug.starts_with('-') {
        return Err(SlugInvalidReason::LeadingHyphen);
    }
    if slug.ends_with('-') {
        return Err(SlugInvalidReason::TrailingHyphen);
    }
    if slug.contains("--") {
        return Err(SlugInvalidReason::ConsecutiveHyphens);
    }
    Ok(())
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// The name of the bookmark issue data lives on. See
/// `docs/storage-format.md` §1.
pub const ISSUES_BOOKMARK: &str = "issues";

/// The revset that resolves to the tip of the `issues` bookmark. We use
/// the function form (`bookmarks(issues)`) rather than the bare
/// bookmark name so a stray collision with another revset never bites.
pub const ISSUES_BOOKMARK_REVSET: &str = "bookmarks(issues)";

/// Description on the empty seed commit that anchors the `issues`
/// bookmark in a fresh repo. Spec §1.1 pins the exact text: no
/// `Jjf-Op:` trailer (the seed predates any op chain), human-readable,
/// stable across versions.
pub const ISSUES_SEED_DESCRIPTION: &str = "jjf: seed issues bookmark";

/// The v1 bookmark name. Detected by [`Storage::init`] /
/// [`Storage::open`] so the inline-detect migration can rename it.
/// Storage never *writes* to this bookmark; it only checks for its
/// presence so the migration knows when to run.
const V1_BUGS_BOOKMARK: &str = "bugs";

/// One node in a `parent-child` dependency tree (spec v2.4 §3.x).
/// Returned by [`Storage::dep_tree`]; rendered by the CLI's
/// `jjf dep tree` verb. The `children` list is sorted by id for
/// determinism. The `cycle` flag is `true` if the node was reached
/// via a cycle (the second time through), in which case recursion
/// stops and `children` is empty.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DepTreeNode {
    pub id: IssueId,
    pub title: String,
    pub status: Status,
    pub children: Vec<DepTreeNode>,
    /// `true` if this node was encountered a second time during the
    /// DFS walk — the parent-child edges form a cycle here. The
    /// renderer surfaces this in plain-text output ("(cycle)") and
    /// the JSON envelope carries the same flag.
    pub cycle: bool,
}

/// The `parent-child` tree returned by [`Storage::dep_tree`]. Rooted
/// at a single issue id; depth-first walk through every node
/// reachable via the parent-child edges in their CHILD direction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DepTree {
    pub root: DepTreeNode,
}

/// On-disk storage shape this `Storage` is operating against.
///
/// Detected at [`Storage::open`] time and pinned for the life of the
/// handle. Drives the write-path dispatch in
/// [`Storage::commit_record_change`] / `commit_memory_change` and the
/// create-path probes (id collision, slug collision).
///
/// - [`StorageMode::V2`]: the on-disk shape pinned by
///   `docs/storage-format.md` v2. Issues live as `issues/<id>.json`
///   files on the `issues` jj bookmark; writes go through the 4-CLI
///   working-copy dance. This is the original shape; the bulk of the
///   v2 codebase is here.
/// - [`StorageMode::V3`]: the on-disk shape pinned by
///   `docs/storage-out-of-tree.md`. Each issue lives at
///   `refs/jjf/issues/<id>` with a commit-chain of op-packs; each
///   memory at `refs/jjf/memories/<key>`. Writes use git-only
///   subprocess calls (`hash-object`, `mktree`, `commit-tree`,
///   `update-ref`); the jj working copy is never touched.
///
/// **Detection.** v3 mode is selected iff
/// [`v3_write::refs::FORMAT_VERSION_REF`] resolves to a commit. The
/// presence of the ref is the marker; the ref's pointed-at tree
/// carries a self-describing `version` blob but reads don't inspect
/// it. A fresh-init repo today still gets v2 mode because
/// `Storage::init` writes the v2 bookmark (ticket `add0646` will
/// rewrite init for v3); a manually-planted sentinel ref upgrades
/// detection to v3 even on a v1/v2-shape repo. The integration tests
/// in this ticket use the planted-sentinel approach to exercise the
/// v3 write path before the migrator (ticket `c14e1c1`) lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StorageMode {
    V2,
    V3,
}

/// Probe `refs/jjf/meta/format-version`. Returns
/// [`StorageMode::V3`] iff the sentinel ref resolves to a commit;
/// otherwise [`StorageMode::V2`].
///
/// Failure modes:
/// - A real git failure (corrupt repo, missing object, IO error)
///   bubbles up as [`Error::Git`].
/// - A simply-absent ref is NOT a failure — it's the v2 case, so we
///   return `V2`.
/// - A present-but-wrong-type ref (sentinel hand-wired to a blob /
///   tree / tag rather than a commit) surfaces as
///   [`Error::CorruptSentinel`]. The sentinel's docstring claims
///   "iff the sentinel ref resolves to a commit"; this branch
///   enforces it instead of silently flipping to v3 mode on a
///   nonsense ref. Per ticket `de59159` (the QA red-team `c1`
///   attack).
///
/// This is the v3 vs v2 discriminator on every `Storage::open` and
/// `Storage::init`. Healthy-path cost: one `git rev-parse --verify
/// --quiet` for absent (v2) or that plus a `git cat-file -t` for
/// present (v3) — both cheap, both single-spawn.
fn detect_storage_mode(git: &git::GitRepo) -> Result<StorageMode> {
    match git.resolve_ref_with_type(v3_write::refs::FORMAT_VERSION_REF) {
        Ok(Some((_, kind))) if kind == "commit" => Ok(StorageMode::V3),
        Ok(Some((oid, kind))) => Err(Error::CorruptSentinel { oid, kind }),
        Ok(None) => Ok(StorageMode::V2),
        Err(e) => Err(Error::Git(e)),
    }
}

/// A handle to a repo whose issue storage is initialized. Use
/// [`Storage::init`] to create the storage (idempotent) in a fresh
/// repo, or [`Storage::open`] when you know it's already in place.
///
/// Carries a per-instance snapshot cache memo so multiple read calls
/// within one CLI invocation share the head-probe + cache load. The
/// memo is invalidated on writes (the mutator drops it), so the next
/// read sees fresh state.
///
/// Also carries a [`StorageMode`] discriminator that pins which write
/// path mutating methods take. Detected at `open` / `init` time and
/// stable for the life of the handle.
#[derive(Debug, Clone)]
pub struct Storage {
    repo: JjRepo,
    /// Git wrapper used by the v3 write path. Built alongside `repo`
    /// in `open` / `init`; coexists with the jj wrapper. v2-mode
    /// callers don't touch it.
    git: git::GitRepo,
    /// V2 vs V3 — pinned at open time, drives every write-path
    /// dispatch.
    mode: StorageMode,
    /// In-process snapshot cache memo. Lazily populated on first
    /// read; cleared by every mutator so the next read sees the
    /// post-write bookmark state. `Arc<Mutex<...>>` so clones share
    /// the memo and Storage stays Send/Sync.
    snapshot_memo: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<cache::SnapshotCache>>>>,
}

impl Storage {
    /// Open a storage handle at the given repo root. The path must be
    /// absolute; we don't resolve `~` or relative paths — that's the
    /// caller's job (mvp-cli, tests). The `issues` bookmark must
    /// already exist; call [`Storage::init`] first if you're not sure.
    ///
    /// **v1 → v2 inline migration.** If the v1 `bugs` bookmark is
    /// present but `issues` is not, `open` runs the migration in place
    /// (renames every `bugs/<id>.json` and `bugs/<id>.comments.jsonl`
    /// to its `issues/` sibling on a single commit, lands it on the
    /// new `issues` bookmark, and deletes the old `bugs` bookmark).
    /// Repos that only have `issues` (the post-migration steady state)
    /// fall through without changes; repos that have neither bookmark
    /// surface their bookmark probe failure unchanged.
    pub fn open(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let root = repo_root.into();
        if !root.is_absolute() {
            return Err(Error::Invalid(format!(
                "Storage::open requires an absolute path, got {}",
                root.display()
            )));
        }
        let git = git::GitRepo::open(root.clone());
        // Detect on-disk shape before running the v1→v2 migration
        // (which only fires on v2-shape data). If the v3 sentinel ref
        // exists, we're already on v3 and the v1→v2 dance must NOT
        // run (it would write a v2 bookmark on a v3 repo, scrambling
        // the source of truth). For now the only writer of the
        // sentinel is `v3_write::write_format_version_sentinel` —
        // used by tests in this ticket and (eventually) by the v2→v3
        // migrator in ticket `c14e1c1`.
        let mode = detect_storage_mode(&git)?;
        let storage = Self {
            repo: JjRepo::open(root),
            git: git.clone(),
            mode,
            snapshot_memo: Default::default(),
        };
        if storage.mode == StorageMode::V2 {
            // v1 → v2 first (renames `bugs/*` → `issues/*` on a
            // single commit if the v1 bookmark is present). Idempotent;
            // no-op on already-v2 repos.
            storage.maybe_migrate_v1_to_v2()?;
            // v2 → v3 second. This walks every issue's op chain off
            // `bookmarks(issues)` and re-lands each commit on
            // `refs/jjf/issues/<id>`, then deletes the `issues`
            // bookmark and plants the v3 sentinel ref. If the
            // bookmark doesn't exist (a hypothetical "no issues at
            // all" repo), the migrator's bookmark probe makes it a
            // no-op and we leave the repo without a sentinel —
            // matching the v2 behavior for that edge case.
            migrate_v2_v3::maybe_migrate_v2_to_v3(&storage.repo, &storage.git)?;
            // Re-detect mode after migration. If the sentinel
            // got planted, we're now V3 and subsequent reads
            // must use the v3 path. Storage carries mode as an
            // immutable field on the public type, so we rebuild
            // it here rather than mutating in place — clones
            // inherit the new mode.
            let new_mode = detect_storage_mode(&git)?;
            return Ok(Self {
                repo: storage.repo,
                git: storage.git,
                mode: new_mode,
                snapshot_memo: storage.snapshot_memo,
            });
        }
        Ok(storage)
    }

    /// Like [`Storage::open`] but skips the v2 → v3 auto-migration.
    /// Test-only — used by the v2 → v3 migration integration tests so
    /// they can plant v2-shape state and then re-open without the
    /// migrator running. Production code MUST NOT call this.
    ///
    /// Note: the v1 → v2 step still runs (the migration tests build
    /// only v2 or v1 shapes and need v1 → v2 to land before they can
    /// inspect or re-migrate to v3).
    #[doc(hidden)]
    pub fn open_skip_v2_to_v3_migration(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let root = repo_root.into();
        if !root.is_absolute() {
            return Err(Error::Invalid(format!(
                "Storage::open requires an absolute path, got {}",
                root.display()
            )));
        }
        let git = git::GitRepo::open(root.clone());
        let mode = detect_storage_mode(&git)?;
        let storage = Self {
            repo: JjRepo::open(root),
            git: git.clone(),
            mode,
            snapshot_memo: Default::default(),
        };
        if storage.mode == StorageMode::V2 {
            storage.maybe_migrate_v1_to_v2()?;
        }
        Ok(storage)
    }

    /// Open or create a storage handle, planting the v3
    /// `refs/jjf/meta/format-version` sentinel ref on a fresh repo.
    /// Idempotent: calling twice against the same repo is a no-op the
    /// second time.
    ///
    /// Four distinct outcomes:
    ///
    /// - `repo_root` is not a jj repo at all → [`Error::NotAJjRepo`].
    /// - `repo_root` is a v3-shape repo (the sentinel ref already
    ///   resolves) → no-op; return Storage with `mode = V3`.
    /// - `repo_root` is a v2-shape repo (the `issues` bookmark exists,
    ///   no v3 sentinel) → no-op; return Storage with `mode = V2`. The
    ///   v2 → v3 migrator (ticket `c14e1c1`) runs at the next
    ///   [`Storage::open`], not here.
    /// - `repo_root` is a v1-shape repo (the `bugs` bookmark exists,
    ///   no `issues` bookmark, no v3 sentinel) → run the v1 → v2
    ///   migration (matching today's behavior); the repo ends v2-shape
    ///   and a follow-up `Storage::open` will hand the v2 → v3
    ///   migration. `mode = V2` is returned.
    /// - `repo_root` is a brand-new jj+git repo (no `bugs`, no
    ///   `issues`, no v3 sentinel) → plant the v3 sentinel ref via
    ///   [`v3_write::write_format_version_sentinel`]. Zero jj calls,
    ///   zero bookmark mutations, zero working-copy mutations.
    ///   `mode = V3`.
    ///
    /// **v3 init contract.** A fresh init writes EXACTLY one ref
    /// (`refs/jjf/meta/format-version`) under `refs/jjf/`. No
    /// `issues` bookmark is created, no seed commit lands, `git HEAD`
    /// is unchanged, and the jj working copy is untouched. This is
    /// the fix for the colocated-repo HEAD-drift footgun
    /// (`docs/storage-out-of-tree.md` §"TL;DR").
    ///
    /// The `mvp-cli` `jjf init` verb is a thin wrapper over this.
    pub fn init(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let root = repo_root.into();
        if !root.is_absolute() {
            return Err(Error::Invalid(format!(
                "Storage::init requires an absolute path, got {}",
                root.display()
            )));
        }
        let repo = JjRepo::open(root.clone());
        let git = git::GitRepo::open(root.clone());

        // Probe: are we inside a jj repo at all? `jj workspace root`
        // is cheap and its failure mode is unambiguous (stderr starts
        // with `Error: There is no jj repo in`). We translate that one
        // specific failure into `NotAJjRepo`; anything else bubbles
        // up as a typed `Jj` error.
        if let Err(e) = repo.run(&["workspace", "root"]) {
            if let JjError::Cli { stderr, .. } = &e {
                if stderr.contains("no jj repo") {
                    return Err(Error::NotAJjRepo(root));
                }
            }
            return Err(Error::Jj(e));
        }

        // Detection: presence of the v3 sentinel ref ⇒ V3, otherwise
        // V2. Cheap (`git rev-parse --verify --quiet`). Idempotency
        // for v3 repos is implicit — `mode == V3` short-circuits the
        // bookmark probe below.
        let mode = detect_storage_mode(&git)?;
        let storage = Self {
            repo: repo.clone(),
            git: git.clone(),
            mode,
            snapshot_memo: Default::default(),
        };

        // V3 mode — sentinel already planted. Idempotent no-op; the
        // per-issue refs are owned by the write path, and creating a
        // v2 `issues` bookmark here would scramble the source of
        // truth.
        if mode == StorageMode::V3 {
            return Ok(storage);
        }

        // V2 mode. The repo can be shaped three ways here:
        //   1. v1 (bugs bookmark, no issues): run v1 → v2 migrator.
        //      After migration, issues bookmark exists ⇒ idempotent.
        //   2. v2 (issues bookmark already present): idempotent.
        //   3. fresh (no bugs, no issues): plant the v3 sentinel and
        //      flip mode to V3. This is the new v3 init contract.
        //
        // Step 1 first — the migrator is also called by Storage::open
        // and is a no-op when no v1 bookmark exists, so this is
        // safe to call unconditionally before the v2 probe.
        storage.maybe_migrate_v1_to_v2()?;

        // Probe: does the `issues` bookmark already exist? `jj bookmark
        // list -T 'name ++ "\n"' issues` prints just `issues\n` to
        // stdout when present and an empty stdout (with a stderr
        // warning) when absent. Exit status is 0 either way, so we
        // key off stdout content.
        let stdout = repo.run(&[
            "bookmark",
            "list",
            "-T",
            "name ++ \"\\n\"",
            ISSUES_BOOKMARK,
        ])?;
        if stdout.lines().any(|line| line.trim() == ISSUES_BOOKMARK) {
            // Already initialized as v2 (either pre-existing or just
            // migrated from v1). Idempotent no-op. The v2 → v3
            // migrator (ticket `c14e1c1`) will pick this up at the
            // next `Storage::open`.
            return Ok(storage);
        }

        // Brand-new repo: no bookmarks, no sentinel. Plant the v3
        // sentinel ref. This is the new init contract: zero jj calls,
        // zero bookmarks, zero working-copy mutations, zero HEAD
        // drift. Just one `git update-ref` under `refs/jjf/`.
        v3_write::write_format_version_sentinel(&git)?;

        Ok(Self {
            repo,
            git,
            mode: StorageMode::V3,
            snapshot_memo: storage.snapshot_memo,
        })
    }

    /// If a v1 `bugs` bookmark is present and the v2 `issues`
    /// bookmark is not, perform the inline rename. Safe to call from
    /// either [`Storage::init`] or [`Storage::open`]; idempotent on
    /// repos that have already migrated (or have neither bookmark).
    ///
    /// The migration lands a single commit on top of the `bugs`
    /// bookmark whose tree renames every `bugs/<id>.json` →
    /// `issues/<id>.json` (and the `.comments.jsonl` sibling), then
    /// creates the new `issues` bookmark pointing at that commit,
    /// then deletes the old `bugs` bookmark.
    ///
    /// The commit description is a fixed string (no `Jjf-Op:` trailer)
    /// so it doesn't appear in any per-issue op chain — the migration
    /// isn't an issue mutation, it's a structural repo-level rename.
    fn maybe_migrate_v1_to_v2(&self) -> Result<()> {
        // Skip if the new bookmark already exists — already migrated.
        if self.bookmark_exists(ISSUES_BOOKMARK)? {
            return Ok(());
        }
        // Skip if the v1 bookmark is absent — nothing to migrate.
        if !self.bookmark_exists(V1_BUGS_BOOKMARK)? {
            return Ok(());
        }

        // Enumerate every issue id present on the v1 bookmark, by
        // listing `bugs/*.json` files at that revision. We treat the
        // `bugs/<id>.comments.jsonl` sibling as part of the rename
        // — if a `.json` exists, its `.comments.jsonl` is also renamed
        // (the writer always emits an empty `.comments.jsonl` at
        // create time, so the sibling is always present).
        let v1_revset = format!("bookmarks({})", V1_BUGS_BOOKMARK);
        let listing = self.repo.run(&[
            "file",
            "list",
            "-r",
            &v1_revset,
            "-T",
            "path ++ \"\\n\"",
            "root:bugs/",
        ])?;
        let mut ids: Vec<IssueId> = Vec::new();
        for line in listing.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("bugs/") else {
                continue;
            };
            let Some(stem) = rest.strip_suffix(".json") else {
                continue;
            };
            if let Ok(id) = IssueId::parse(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        ids.dedup();

        // Build a new commit on top of the v1 bookmark and rewrite the
        // tree. We need the FILE BYTES from each old path so we can
        // write them into the new path; we read via `jj file show -r
        // bookmarks(bugs)` (the working copy at this point is on
        // root() or wherever the operator left it — we don't trust it).
        let summary = "jjf: migrate v1 bugs/ → v2 issues/";
        // `jj new <v1-bookmark>` creates a new commit on top of the v1
        // bookmark; we'll edit the working copy to remove the old
        // paths and add the new ones.
        self.repo.run(&["new", &v1_revset, "-m", summary])?;

        let wc_root = self.repo.root();
        // Drain the working copy of any existing `bugs/`/`issues/`
        // residue first — we want the renames to be authoritative.
        let bugs_dir = wc_root.join("bugs");
        let issues_dir = wc_root.join("issues");
        if bugs_dir.exists() {
            // jj has materialized the v1 tree here. We'll move files
            // one by one rather than removing the whole dir, so any
            // unexpected sibling under `bugs/` is preserved on the v1
            // bookmark (we delete the `bugs` bookmark at the end; any
            // stray content is unreachable but not silently deleted
            // from history).
        }
        std::fs::create_dir_all(&issues_dir)?;

        for id in &ids {
            let json_name = format!("{}.json", id);
            let comments_name = format!("{}.comments.jsonl", id);
            let v1_json = bugs_dir.join(&json_name);
            let v1_comments = bugs_dir.join(&comments_name);
            let v2_json = issues_dir.join(&json_name);
            let v2_comments = issues_dir.join(&comments_name);

            if v1_json.is_file() {
                std::fs::rename(&v1_json, &v2_json)?;
            }
            if v1_comments.is_file() {
                std::fs::rename(&v1_comments, &v2_comments)?;
            }
        }
        // After moving every recognized id, the bugs/ directory should
        // be empty for ids we know about. Remove if empty; leave alone
        // otherwise (defensive — a future schema-extension file under
        // bugs/ would otherwise vanish silently).
        if bugs_dir.is_dir() {
            // best-effort empty-dir removal; ignore the error if the
            // dir is non-empty (some stray file persists, which we
            // don't want to delete).
            let _ = std::fs::remove_dir(&bugs_dir);
        }

        // Land the new bookmark, delete the old one, step @ off.
        self.repo
            .run(&["bookmark", "create", ISSUES_BOOKMARK, "-r", "@"])?;
        self.repo.run(&["bookmark", "delete", V1_BUGS_BOOKMARK])?;
        self.repo.run(&["new", "root()"])?;

        // Snapshot memo: a fresh Storage instance built by
        // `Storage::open` / `init` has an empty memo, so dropping
        // is a no-op here. We still call it for symmetry / future
        // proofing (if `maybe_migrate_v1_to_v2` ever runs after a
        // `snapshot()` lazy-load).
        self.invalidate_snapshot_memo();

        Ok(())
    }

    /// Does the named bookmark exist on this repo? Used internally for
    /// the v1 → v2 migration detector.
    fn bookmark_exists(&self, name: &str) -> Result<bool> {
        let stdout = self
            .repo
            .run(&["bookmark", "list", "-T", "name ++ \"\\n\"", name])?;
        Ok(stdout.lines().any(|line| line.trim() == name))
    }

    /// Load the snapshot cache, sharing the result across multiple
    /// read calls in this Storage instance. Internal helper used by
    /// every read-path verb.
    ///
    /// First call probes the bookmark head and loads / rebuilds the
    /// cache as needed. Subsequent calls return the memoized cache
    /// without re-probing — they trust that no mutation has landed
    /// through THIS Storage instance since the last load. Mutators
    /// call [`Storage::invalidate_snapshot_memo`] to drop the memo
    /// so the next read re-probes.
    ///
    /// The memo IS shared across `Storage::clone()` instances by
    /// design — same `Arc<Mutex<...>>`. A caller that wants
    /// independent memos clones via [`Storage::open`] / [`init`]
    /// from scratch.
    fn snapshot(&self) -> Result<std::sync::Arc<cache::SnapshotCache>> {
        // Fast path: read the memo. If populated, return.
        {
            let guard = self
                .snapshot_memo
                .lock()
                .expect("snapshot memo mutex poisoned");
            if let Some(cached) = guard.as_ref() {
                return Ok(cached.clone());
            }
        }
        // Slow path: load / rebuild, then install in the memo.
        // We drop the mutex during the I/O so concurrent callers
        // (e.g. a multi-threaded CLI doing a `list_ready` + a
        // `list_memories` from different tasks) don't serialize.
        // Cost of a duplicate rebuild is bounded — second writer
        // just overwrites the first; net result is correct either
        // way.
        //
        // V3 dispatches to the ref-set-keyed cache; V2 keeps the
        // bookmark-tip-keyed path. Both write the same
        // `.jj/jjforge-cache.json` file — the `format_kind` field
        // discriminates and either side discards the other's cache
        // file on load.
        let snap = std::sync::Arc::new(if self.mode == StorageMode::V3 {
            cache::load_or_rebuild_v3(&self.git, self.repo.root())?
        } else {
            cache::load_or_rebuild(&self.repo, self.repo.root())?
        });
        let mut guard = self
            .snapshot_memo
            .lock()
            .expect("snapshot memo mutex poisoned");
        // If a concurrent caller raced us, take whichever finished
        // first. Both reflect the same bookmark tip.
        if let Some(existing) = guard.as_ref() {
            return Ok(existing.clone());
        }
        *guard = Some(snap.clone());
        Ok(snap)
    }

    /// Drop the in-process snapshot memo so the next read re-probes
    /// the bookmark head. Every mutator calls this after the 4-CLI
    /// dance lands. The on-disk cache (`.jj/jjforge-cache.json`)
    /// stays put; it'll be detected as stale on the next probe and
    /// rebuilt.
    fn invalidate_snapshot_memo(&self) {
        let mut guard = self
            .snapshot_memo
            .lock()
            .expect("snapshot memo mutex poisoned");
        *guard = None;
    }

    /// The repo root this storage handle is rooted at.
    pub fn repo_root(&self) -> &Path {
        self.repo.root()
    }

    /// Per-ref diagnostics for refs under `refs/jjf/issues/*` /
    /// `refs/jjf/memories/*` that the snapshot-cache rebuild could
    /// not parse into an [`Issue`] / [`Memory`].
    ///
    /// Returns an empty vec when the cache is clean. Each entry
    /// carries the full ref name and a one-line human-readable
    /// reason. Used by the CLI's `ls` / `ready` verbs to emit a
    /// stderr warning when one or more refs got dropped from the
    /// result set (ticket `4928ae6` — silent-corrupt-ref bug).
    ///
    /// Cheap: routes through the same snapshot memo as the read
    /// path, so calling this after a `list_ids` / `list_ready` is
    /// effectively free.
    pub fn unreadable_refs(&self) -> Result<Vec<UnreadableRef>> {
        let snapshot = self.snapshot()?;
        Ok(snapshot.unreadable_refs.clone())
    }

    /// True iff this handle was opened on a v3-shape repo (the
    /// `refs/jjf/meta/format-version` sentinel ref resolves). False
    /// means the repo is still v2-shape — only reachable in the test
    /// suite via [`Storage::open_skip_v2_to_v3_migration`]. Production
    /// callers see V3 unconditionally because the migrator runs at
    /// `Storage::open`.
    ///
    /// Used by the CLI's `push` / `pull` verbs to dispatch between the
    /// v3 git-transport path ([`Storage::push_v3`] /
    /// [`Storage::pull_v3`]) and the legacy v2 bookmark-transport path.
    pub fn is_v3(&self) -> bool {
        matches!(self.mode, StorageMode::V3)
    }

    /// Create a new issue from a draft. Returns the freshly-minted
    /// issue ID. Lands one commit on the `issues` bookmark with op
    /// vocabulary `create`.
    ///
    /// Validates `draft.slug` per spec v2.1 §3.1 if present
    /// (`Error::InvalidSlug`); rejects a slug already in use by
    /// ANY existing issue, regardless of status
    /// (`Error::SlugCollision`). Spec v2.6: closed issues retain
    /// their slug forever (issue `a105e0b`).
    pub fn create_issue(&self, draft: &IssueDraft) -> Result<IssueId> {
        if let Err(reason) = validate_title(&draft.title) {
            return Err(Error::InvalidTitle {
                title: draft.title.clone(),
                reason,
            });
        }

        // Seed body must fit the cap. Issue `679444a` (QA red-team
        // 2026-06-25 sub-pass 4 C3): pre-fix, a multi-MB body landed
        // silently with no documented bound. We now match GitHub's
        // 65,536-byte cap.
        if let Err(reason) = validate_body(&draft.body) {
            return Err(Error::InvalidBody { reason });
        }

        // Trailer-injection guards (`qa-trailer-injection`, issue
        // `a902492`): every free-form string the create-time multi-op
        // stanza interpolates into the trailer block must be single-
        // line. Title is already covered by `validate_title`; slug is
        // already constrained by `validate_slug`'s charset; block-
        // reason isn't a draft field. The remaining free-form payload
        // strings are assignee and each label.
        if let Some(a) = &draft.assignee {
            if validate_no_newlines(a).is_err() {
                return Err(Error::Invalid(
                    "assignee must not contain newlines".into(),
                ));
            }
        }
        for label in &draft.labels {
            if validate_no_newlines(label).is_err() {
                return Err(Error::Invalid(
                    "label must not contain newlines".into(),
                ));
            }
        }

        // Validate each dep target exists on the bookmark. The
        // child id isn't known yet (it's rerolled below), so
        // self-dep is structurally impossible at create time;
        // only phantom-target rejection applies. v2.x
        // (`qa-dep-validation`, issue `d1a01f0`): without this, `jjf
        // new -d <fake-id>` (and `jjf new --dep blocks:<fake-id>`)
        // silently lands a dangling edge in the new issue's record.
        for edge in &draft.dependencies {
            if !self.issue_exists_on_bookmark(&edge.target)? {
                return Err(Error::IssueNotFound(edge.target.clone()));
            }
        }

        // Pre-validate the slug, if any, BEFORE the (cheap) id reroll
        // and the (expensive) uniqueness probe. The first check is
        // purely local; the second is a list-and-read across every
        // open issue. The order keeps the cheap rejection cheap.
        if let Some(slug) = &draft.slug {
            if let Err(reason) = validate_slug(slug) {
                return Err(Error::InvalidSlug {
                    slug: slug.clone(),
                    reason,
                });
            }
            // Uniqueness across ALL issues. Spec v2.6: slug
            // collisions are forbidden across the full history —
            // closed issues retain their slug forever, so a new
            // ticket must pick a fresh one (issue `a105e0b`).
            // The probe is one HashMap lookup against the cache's
            // pre-built `slug_index`.
            if let Some(conflict) = self.find_slug_collision(slug, None)? {
                return Err(Error::SlugCollision {
                    slug: slug.clone(),
                    conflicts_with: conflict,
                });
            }
        }

        // Reroll on collision. The space is 2^28 ≈ 268M and a repo
        // typically has only a handful, so this loop never runs more
        // than once in practice. We probe the bookmark (not the
        // working copy) because the dance leaves @ on root() with no
        // issue files staged.
        let id = loop {
            let candidate = IssueId::random();
            if !self.issue_exists_on_bookmark(&candidate)? {
                break candidate;
            }
        };

        let now = now_rfc3339()?;
        let type_ = draft.type_.unwrap_or_default();
        let record = IssueRecord {
            version: 2,
            id: id.clone(),
            title: draft.title.clone(),
            slug: draft.slug.clone(),
            body: draft.body.clone(),
            status: Status::Open,
            block_reason: None,
            type_,
            labels: sorted_dedup(&draft.labels),
            dependencies: sorted_dedup_edges(&draft.dependencies),
            assignee: draft.assignee.clone(),
            created_at: now.clone(),
            updated_at: now,
        };

        // The `create` op trailer (spec §5.2) carries only title +
        // status. Anything else the draft seeds — body, labels,
        // dependencies, assignee, type, slug — is recorded as
        // additional ops in the same commit (spec §5.5 allows
        // multi-op commits). Without this, the audit chain (and the
        // read-path op-replay) would miss seed-time fields entirely,
        // and the v1-contract cross-check would fire on every
        // non-trivial create.
        //
        // Op order in the create-time multi-op stanza follows the
        // record's field-declaration order (spec §3.3): slug,
        // body, type, labels, dependencies, assignee.
        let summary = format!("jjf: issue {} - create", id);
        let mut ops: Vec<Op> = Vec::new();
        ops.push(Op::Create {
            issue_id: id.clone(),
            title: record.title.clone(),
            status: Status::Open,
        });
        if let Some(slug) = &record.slug {
            ops.push(Op::SetSlug {
                issue_id: id.clone(),
                slug: Some(slug.clone()),
            });
        }
        if !record.body.is_empty() {
            ops.push(Op::SetBody {
                issue_id: id.clone(),
                body_hash: sha256_hex(record.body.as_bytes()),
            });
        }
        if record.type_ != IssueType::Unspecified {
            ops.push(Op::SetType {
                issue_id: id.clone(),
                kind: record.type_,
            });
        }
        for label in &record.labels {
            ops.push(Op::LabelAdd {
                issue_id: id.clone(),
                label: label.clone(),
            });
        }
        for dep in &record.dependencies {
            ops.push(Op::DepAdd {
                issue_id: id.clone(),
                dep: dep.target.clone(),
                kind: dep.kind,
            });
        }
        if let Some(assignee) = &record.assignee {
            ops.push(Op::SetAssignee {
                issue_id: id.clone(),
                assignee: Some(assignee.clone()),
            });
        }
        let claimed_slug = record.slug.clone();
        let commit_result = if self.mode == StorageMode::V3 {
            // V3 write path: build the trailer block + commit, land
            // it on `refs/jjf/issues/<id>` via git-only calls. No
            // working-copy edits; no jj subprocess. Comments file is
            // omitted from the tree at create time (the design's "if
            // any" semantics) — the first `add_comment` will plant
            // the blob in the tree, not the create.
            self.commit_record_v3(&id, &record, None, &summary, &ops)
        } else {
            self.commit_record_change(&summary, &ops, |wc_root| {
                write_record_json(&wc_root.join(issue_json_relpath(&id)), &record)?;
                // Comments file: create empty so readers don't trip on
                // ENOENT for new issues. Spec §4 allows empty == no comments.
                write_comments_jsonl(&wc_root.join(issue_comments_relpath(&id)), &[])?;
                Ok(())
            })
        };

        match commit_result {
            Ok(()) => Ok(id),
            Err(e) if is_typed_concurrent_write(&e) => {
                // Slug-claim races MUST fail fast — a retry would
                // re-race the same slug indefinitely. Probe the
                // post-race bookmark: if the slug is now taken by
                // another open issue, the more-specific
                // [`Error::SlugCollision`] is the better surface for
                // the operator. Otherwise (no slug, or slug genuinely
                // free), the typed ConcurrentWrite is the right
                // signal to retry the command.
                self.invalidate_snapshot_memo();
                if let Some(slug) = claimed_slug.as_deref() {
                    if let Ok(Some(holder)) =
                        self.find_slug_collision(slug, None)
                    {
                        return Err(Error::SlugCollision {
                            slug: slug.to_owned(),
                            conflicts_with: holder,
                        });
                    }
                }
                Err(e)
            }
            Err(e) => Err(e),
        }
    }

    /// Replace the title.
    pub fn set_title(&self, id: &IssueId, title: &str) -> Result<()> {
        if let Err(reason) = validate_title(title) {
            return Err(Error::InvalidTitle {
                title: title.to_owned(),
                reason,
            });
        }
        let title = title.to_owned();
        self.mutate(id, &format!("jjf: issue {} - set-title", id), |rec| {
            rec.title = title.clone();
            MutateOutcome::Write(vec![Op::SetTitle {
                issue_id: rec.id.clone(),
                title: title.clone(),
            }])
        })
        .map(|_| ())
    }

    /// Replace the status.
    pub fn set_status(&self, id: &IssueId, status: Status) -> Result<()> {
        self.mutate(id, &format!("jjf: issue {} - set-status", id), |rec| {
            rec.status = status;
            MutateOutcome::Write(vec![Op::SetStatus {
                issue_id: rec.id.clone(),
                status,
            }])
        })
        .map(|_| ())
    }

    /// Replace the body.
    pub fn set_body(&self, id: &IssueId, body: &str) -> Result<()> {
        // Bound the body at the documented cap before any IO.
        // Issue `679444a` (QA red-team 2026-06-25 sub-pass 4 C3).
        if let Err(reason) = validate_body(body) {
            return Err(Error::InvalidBody { reason });
        }
        let body = body.to_owned();
        self.mutate(id, &format!("jjf: issue {} - set-body", id), |rec| {
            rec.body = body.clone();
            let hash = sha256_hex(body.as_bytes());
            MutateOutcome::Write(vec![Op::SetBody {
                issue_id: rec.id.clone(),
                body_hash: hash,
            }])
        })
        .map(|_| ())
    }

    /// Replace the assignee. `None` clears it.
    ///
    /// Rejects assignee values containing `\n` or `\r` with
    /// [`Error::Invalid`] — those would inject extra lines into the
    /// `Jjf-Assignee:` trailer, opening an op-injection vector
    /// (`qa-trailer-injection`, issue `a902492`).
    pub fn set_assignee(&self, id: &IssueId, assignee: Option<&str>) -> Result<()> {
        if let Some(a) = assignee {
            if validate_no_newlines(a).is_err() {
                return Err(Error::Invalid(
                    "assignee must not contain newlines".into(),
                ));
            }
        }
        let assignee = assignee.map(str::to_owned);
        self.mutate(id, &format!("jjf: issue {} - set-assignee", id), |rec| {
            rec.assignee = assignee.clone();
            MutateOutcome::Write(vec![Op::SetAssignee {
                issue_id: rec.id.clone(),
                assignee: assignee.clone(),
            }])
        })
        .map(|_| ())
    }

    /// Update one or more scalar fields in a single commit.
    ///
    /// Bundles every populated field of [`UpdateFields`] into one call
    /// to [`Storage::mutate`], which produces ONE new commit on the
    /// `issues` bookmark carrying N `Jjf-Op:` trailers (spec §5.5).
    /// This is the operator-facing "change title + status + body in one
    /// go" path that the per-field scalar mutators
    /// (`set_title` / `set_status` / `set_body` / `set_assignee`) can't
    /// express without fragmenting into N separate commits.
    ///
    /// Op order in the resulting commit follows the field-declaration
    /// order of `UpdateFields` (title, status, body, assignee). This
    /// matches the spec §5.7 convention used by `create_issue` —
    /// readers that op-replay see fields applied in the same order
    /// the on-disk record's schema declares them.
    ///
    /// Empty bundles (every field `None`) are rejected with
    /// [`Error::Invalid`]: the call would land an empty commit with no
    /// trailers, which would violate spec §5.4 (every jjforge commit
    /// carries at least one `Jjf-Op:` stanza). The CLI layer also
    /// rejects this case earlier with a more user-friendly hint, but
    /// the storage-side guard means programmatic callers can't trip the
    /// spec by accident either.
    ///
    /// Title validation matches `set_title` (delegated to
    /// `validate_title`: non-empty after trim AND no control
    /// characters); other fields accept any string.
    pub fn update(&self, id: &IssueId, fields: UpdateFields) -> Result<()> {
        if fields.is_empty() {
            return Err(Error::Invalid(
                "update called with no fields set".into(),
            ));
        }
        if let Some(title) = &fields.title {
            if let Err(reason) = validate_title(title) {
                return Err(Error::InvalidTitle {
                    title: title.clone(),
                    reason,
                });
            }
        }
        // Assignee newline-guard (`qa-trailer-injection`, issue
        // `a902492`): a multi-line assignee would inject extra trailer
        // lines into the commit description and forge a `Jjf-Op:`
        // stanza. Reject at the boundary.
        if let Some(Some(a)) = &fields.assignee {
            if validate_no_newlines(a).is_err() {
                return Err(Error::Invalid(
                    "assignee must not contain newlines".into(),
                ));
            }
        }
        // Pre-validate the slug syntactic shape (charset, length,
        // hyphen placement). The uniqueness probe is a domain
        // precondition vs. OTHER records and so MUST run inside the
        // closure to re-evaluate on each retry against the post-race
        // bookmark — see `a6b8fb7`.
        if let Some(Some(slug)) = &fields.slug {
            if let Err(reason) = validate_slug(slug) {
                return Err(Error::InvalidSlug {
                    slug: slug.clone(),
                    reason,
                });
            }
        }
        // Body cap. Issue `679444a` (QA red-team 2026-06-25
        // sub-pass 4 C3): match GitHub's 65,536-byte limit.
        if let Some(body) = &fields.body {
            if let Err(reason) = validate_body(body) {
                return Err(Error::InvalidBody { reason });
            }
        }
        let summary = format!("jjf: issue {} - update", id);
        let id_owned = id.clone();
        self.mutate(id, &summary, |rec| {
            // Slug uniqueness probe: a non-`None` slug write must not
            // collide with any OTHER issue's slug (Open, InProgress,
            // Blocked, or Closed — spec v2.6, issue `a105e0b`).
            // Probed on every retry against the fresh bookmark, so a
            // racer that claims the slug between our attempts surfaces
            // as a typed conflict instead of a duplicate write
            // (`a6b8fb7`).
            if let Some(Some(new_slug)) = &fields.slug {
                match self.find_slug_collision(new_slug, Some(&id_owned)) {
                    Ok(Some(conflict)) => {
                        return MutateOutcome::Conflict(Error::SlugCollision {
                            slug: new_slug.clone(),
                            conflicts_with: conflict,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => return MutateOutcome::Conflict(e),
                }
            }
            let mut ops: Vec<Op> = Vec::new();
            if let Some(title) = &fields.title {
                rec.title = title.clone();
                ops.push(Op::SetTitle {
                    issue_id: rec.id.clone(),
                    title: title.clone(),
                });
            }
            if let Some(slug) = &fields.slug {
                rec.slug = slug.clone();
                ops.push(Op::SetSlug {
                    issue_id: rec.id.clone(),
                    slug: slug.clone(),
                });
            }
            if let Some(status) = fields.status {
                rec.status = status;
                ops.push(Op::SetStatus {
                    issue_id: rec.id.clone(),
                    status,
                });
            }
            if let Some(type_outer) = fields.type_ {
                let new_type = type_outer.unwrap_or_default();
                rec.type_ = new_type;
                ops.push(Op::SetType {
                    issue_id: rec.id.clone(),
                    kind: new_type,
                });
            }
            if let Some(body) = &fields.body {
                rec.body = body.clone();
                ops.push(Op::SetBody {
                    issue_id: rec.id.clone(),
                    body_hash: sha256_hex(body.as_bytes()),
                });
            }
            if let Some(assignee) = &fields.assignee {
                rec.assignee = assignee.clone();
                ops.push(Op::SetAssignee {
                    issue_id: rec.id.clone(),
                    assignee: assignee.clone(),
                });
            }
            MutateOutcome::Write(ops)
        })
        .map(|_| ())
    }

    /// Atomically claim an issue: set its assignee to `who` and
    /// advance status to [`Status::InProgress`] in ONE multi-op
    /// commit (one `set-assignee` and one `set-status` trailer).
    /// v2.3 (`agent-claim-atomic`).
    ///
    /// Semantics:
    ///
    /// - Idempotent: if the issue is already InProgress and assigned
    ///   to `who`, this is a no-op (no commit lands). Same-user
    ///   re-claim is safe and cheap.
    /// - First-write-wins on the bookmark: two concurrent claims
    ///   race at the underlying `jj bookmark set` step; jj rejects
    ///   the loser as a non-fast-forward (the surface is a `Jj`
    ///   error from the shell-out). The loser re-reads `ready` and
    ///   tries again — duplicate-work avoidance falls out of
    ///   bookmark ordering.
    /// - Returns [`Error::AlreadyClaimed`] if the issue is already
    ///   InProgress with a DIFFERENT assignee. The CLI surfaces
    ///   this as the `already_claimed` envelope.
    /// - Returns [`Error::Invalid`] if the issue is closed —
    ///   claiming a closed issue is almost certainly a mistake (the
    ///   operator probably meant to reopen first).
    /// - Returns [`Error::Invalid`] if `who` is empty after trim.
    ///
    /// The commit lands one [`Op::SetAssignee`] and one
    /// [`Op::SetStatus`] in field-declaration order: assignee, then
    /// status. (Two ops, one commit; the op-space resolver's LWW
    /// projection lands on the same final state regardless of read
    /// order.)
    pub fn claim(&self, id: &IssueId, who: &str) -> Result<ClaimResult> {
        let who = who.trim();
        if who.is_empty() {
            return Err(Error::Invalid(
                "claim: assignee must not be empty".into(),
            ));
        }
        if validate_no_newlines(who).is_err() {
            return Err(Error::Invalid(
                "claim: assignee must not contain newlines".into(),
            ));
        }
        let who_owned = who.to_owned();
        let id_owned = id.clone();
        // Precondition check lives INSIDE the closure so it re-runs
        // on every retry against the freshly-read record. The pre-
        // `a6b8fb7` shape (check once, mutate-and-retry blindly)
        // produced duplicate claims when two `ready --claim` calls
        // raced — the loser's retry would blindly re-apply
        // `set-assignee + set-status InProgress` against a record
        // that the winner had already claimed, returning Ok and
        // silently double-claiming.
        self.mutate(id, &format!("jjf: issue {} - claim", id), |rec| {
            match rec.status {
                Status::Closed => {
                    return MutateOutcome::Conflict(Error::Invalid(format!(
                        "issue {id_owned} is closed; reopen before claiming"
                    )));
                }
                Status::Abandoned => {
                    // v2.7: soft-deleted. Claiming would silently
                    // resurrect the issue. Same shape as closed —
                    // force an explicit `jjf update --status open`
                    // to revive (the audit trail then carries
                    // the intent).
                    return MutateOutcome::Conflict(Error::Invalid(format!(
                        "issue {id_owned} is abandoned; reopen before claiming"
                    )));
                }
                Status::Blocked => {
                    // v2.5: parked on an external signal. Claiming a
                    // blocked issue would silently flip its status
                    // to in-progress AND drop the reason on the
                    // floor — confusing for the next reader. Force
                    // the operator to `jjf unblock` first; the
                    // explicit step preserves the audit trail.
                    return MutateOutcome::Conflict(Error::Invalid(format!(
                        "issue {id_owned} is blocked; unblock before claiming"
                    )));
                }
                Status::InProgress => {
                    // Already claimed. Same user → no-op (Skip).
                    // Different user → AlreadyClaimed.
                    match rec.assignee.as_deref() {
                        Some(existing) if existing == who_owned => {
                            return MutateOutcome::Skip;
                        }
                        Some(existing) => {
                            return MutateOutcome::Conflict(Error::AlreadyClaimed {
                                by: existing.to_owned(),
                            });
                        }
                        // InProgress without an assignee is a
                        // degenerate state (shouldn't happen via the
                        // normal claim path), but treat it as
                        // claimable rather than wedging.
                        None => {}
                    }
                }
                Status::Open => {}
            }
            rec.assignee = Some(who_owned.clone());
            rec.status = Status::InProgress;
            MutateOutcome::Write(vec![
                Op::SetAssignee {
                    issue_id: rec.id.clone(),
                    assignee: Some(who_owned.clone()),
                },
                Op::SetStatus {
                    issue_id: rec.id.clone(),
                    status: Status::InProgress,
                },
            ])
        })
        .map(|landed| {
            if landed {
                ClaimResult::Claimed
            } else {
                ClaimResult::AlreadyOurs
            }
        })
    }

    /// Atomically unclaim an issue: clear the assignee and set
    /// status back to [`Status::Open`] in ONE multi-op commit. v2.3
    /// (`agent-claim-atomic`). Inverse of [`Storage::claim`].
    ///
    /// Semantics:
    ///
    /// - Idempotent: if the issue is already Open and unassigned,
    ///   this is a no-op.
    /// - Returns [`Error::Invalid`] if the issue is closed (same
    ///   rationale as `claim`).
    ///
    /// Like `claim`, lands two ops (`SetAssignee None` and
    /// `SetStatus Open`) in field-declaration order.
    pub fn unclaim(&self, id: &IssueId) -> Result<()> {
        let id_owned = id.clone();
        // Precondition lives inside the closure so a CAS-loss retry
        // re-checks against the post-race record. Without this, a
        // racer closing the issue between our read and our retry
        // would let us silently re-open it (`a6b8fb7`).
        self.mutate(id, &format!("jjf: issue {} - unclaim", id), |rec| {
            if rec.status == Status::Closed {
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is closed; nothing to unclaim"
                )));
            }
            if rec.status == Status::Abandoned {
                // v2.7: abandoned terminal state. Unclaiming would
                // silently flip to Open. Force the operator to
                // `jjf update --status open` first.
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is abandoned; nothing to unclaim"
                )));
            }
            if rec.status == Status::Open && rec.assignee.is_none() {
                // No-op: already in the unclaimed state.
                return MutateOutcome::Skip;
            }
            rec.assignee = None;
            rec.status = Status::Open;
            MutateOutcome::Write(vec![
                Op::SetAssignee {
                    issue_id: rec.id.clone(),
                    assignee: None,
                },
                Op::SetStatus {
                    issue_id: rec.id.clone(),
                    status: Status::Open,
                },
            ])
        })
        .map(|_| ())
    }

    /// Park an issue: set status to [`Status::Blocked`] and record a
    /// free-text `reason` in ONE multi-op commit. v2.5
    /// (`agent-await-gates-impl`). The companion verb
    /// [`Storage::unblock`] flips it back to [`Status::Open`] and
    /// clears the reason.
    ///
    /// Semantics:
    ///
    /// - The commit lands one [`Op::SetStatus`] and one
    ///   [`Op::SetBlockReason`] in field-declaration order: status,
    ///   then reason. Two ops, one commit; the op-space resolver's
    ///   LWW projection lands on the same final state regardless of
    ///   read order.
    /// - `reason` is optional. `None` records `block_reason: null`;
    ///   `Some(text)` stores the text verbatim. Empty / whitespace-
    ///   only reasons are normalized to `None` so the on-disk shape
    ///   stays consistent.
    /// - Reasons must be single-line — newlines would corrupt the
    ///   `Jjf-Reason:` trailer. The storage layer rejects multi-line
    ///   reasons with [`Error::Invalid`]; callers (the CLI) should
    ///   pre-trim their input.
    /// - Returns [`Error::Invalid`] if the issue is already closed
    ///   — parking a closed issue doesn't compose; the operator
    ///   probably meant to reopen first.
    /// - Not idempotent at the commit level — re-blocking an
    ///   already-blocked issue with the same reason still lands a
    ///   fresh commit (audit-log discipline matches `set_status` /
    ///   `add_label`). The in-memory record stays consistent.
    pub fn block(&self, id: &IssueId, reason: Option<&str>) -> Result<()> {
        let normalized: Option<String> = match reason {
            Some(r) => {
                if r.contains('\n') || r.contains('\r') {
                    return Err(Error::Invalid(
                        "block reason must be single-line (no newlines)".into(),
                    ));
                }
                let t = r.trim();
                if t.is_empty() { None } else { Some(t.to_owned()) }
            }
            None => None,
        };
        let reason_owned = normalized;
        let id_owned = id.clone();
        // Precondition lives in the closure so a CAS-loss retry
        // re-checks the (possibly-changed) status against the fresh
        // record. Without this, a racer closing the issue between
        // our read and the retry would let us silently re-open-then-
        // block it (`a6b8fb7`).
        self.mutate(id, &format!("jjf: issue {} - block", id), |rec| {
            if rec.status == Status::Closed {
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is closed; reopen before blocking"
                )));
            }
            if rec.status == Status::Abandoned {
                // v2.7: abandoned terminal state. Same shape as
                // the closed rejection — force an explicit revive
                // (`jjf update --status open`) before blocking.
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is abandoned; reopen before blocking"
                )));
            }
            rec.status = Status::Blocked;
            rec.block_reason = reason_owned.clone();
            MutateOutcome::Write(vec![
                Op::SetStatus {
                    issue_id: rec.id.clone(),
                    status: Status::Blocked,
                },
                Op::SetBlockReason {
                    issue_id: rec.id.clone(),
                    reason: reason_owned.clone(),
                },
            ])
        })
        .map(|_| ())
    }

    /// Inverse of [`Storage::block`]: set status back to
    /// [`Status::Open`] and clear `block_reason` in ONE multi-op
    /// commit. v2.5 (`agent-await-gates-impl`).
    ///
    /// Semantics:
    ///
    /// - Lands one [`Op::SetStatus`] (`Open`) and one
    ///   [`Op::SetBlockReason`] (`None`) in field-declaration order.
    /// - Idempotent: if the issue is already Open with no
    ///   block_reason, returns `Ok(())` without writing.
    /// - Returns [`Error::Invalid`] if the issue is closed —
    ///   unblocking a closed issue doesn't compose.
    /// - Works regardless of current status (Blocked, InProgress,
    ///   or Open with a stale reason). The operator is asserting
    ///   "this is workable now"; flipping to Open is the right
    ///   semantics across all three.
    pub fn unblock(&self, id: &IssueId) -> Result<()> {
        let id_owned = id.clone();
        // Precondition + idempotent-skip live in the closure so a
        // CAS-loss retry re-checks the fresh record (`a6b8fb7`).
        self.mutate(id, &format!("jjf: issue {} - unblock", id), |rec| {
            if rec.status == Status::Closed {
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is closed; nothing to unblock"
                )));
            }
            if rec.status == Status::Abandoned {
                // v2.7: abandoned terminal state. Same shape as
                // closed — force an explicit revive before
                // touching block state.
                return MutateOutcome::Conflict(Error::Invalid(format!(
                    "issue {id_owned} is abandoned; nothing to unblock"
                )));
            }
            if rec.status == Status::Open && rec.block_reason.is_none() {
                // No-op: already in the unblocked state.
                return MutateOutcome::Skip;
            }
            rec.status = Status::Open;
            rec.block_reason = None;
            MutateOutcome::Write(vec![
                Op::SetStatus {
                    issue_id: rec.id.clone(),
                    status: Status::Open,
                },
                Op::SetBlockReason {
                    issue_id: rec.id.clone(),
                    reason: None,
                },
            ])
        })
        .map(|_| ())
    }

    /// Add a label. No-op (per spec §5.2) if already present, but the
    /// commit is still landed so the audit log records intent.
    pub fn add_label(&self, id: &IssueId, label: &str) -> Result<()> {
        if validate_no_newlines(label).is_err() {
            return Err(Error::Invalid(
                "label must not contain newlines".into(),
            ));
        }
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: issue {} - label-add", id), |rec| {
            if !rec.labels.iter().any(|l| l == &label) {
                rec.labels.push(label.clone());
                rec.labels.sort();
            }
            MutateOutcome::Write(vec![Op::LabelAdd {
                issue_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
        .map(|_| ())
    }

    /// Remove a label. No-op (spec §5.2) if not present.
    pub fn remove_label(&self, id: &IssueId, label: &str) -> Result<()> {
        if validate_no_newlines(label).is_err() {
            return Err(Error::Invalid(
                "label must not contain newlines".into(),
            ));
        }
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: issue {} - label-rm", id), |rec| {
            rec.labels.retain(|l| l != &label);
            MutateOutcome::Write(vec![Op::LabelRm {
                issue_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
        .map(|_| ())
    }

    /// Add a `blocks`-kind dependency. Convenience wrapper around
    /// [`Storage::add_dep_edge`] with `kind = DepKind::Blocks`. Kept
    /// stable across the v2.4 schema bump so existing callers keep
    /// working.
    pub fn add_dependency(&self, id: &IssueId, dep: &IssueId) -> Result<()> {
        self.add_dep_edge(id, dep, DepKind::Blocks)
    }

    /// Remove a `blocks`-kind dependency. Convenience wrapper around
    /// [`Storage::remove_dep_edge`] with `kind = DepKind::Blocks`.
    pub fn remove_dependency(&self, id: &IssueId, dep: &IssueId) -> Result<()> {
        self.remove_dep_edge(id, dep, DepKind::Blocks)
    }

    /// Add a typed dependency edge. Lands one `dep-add` op with the
    /// `Jjf-Dep-Kind:` trailer carrying `kind`. The same `(target,
    /// kind)` pair can't appear twice — the in-memory record dedupes —
    /// but a fresh `dep-add` op still lands so the audit log records
    /// intent. v2.4 (`agent-dep-types`).
    ///
    /// Validation (v2.x `qa-dep-validation`, issue `d1a01f0`;
    /// v2.6 `dep-cycle-undetected`, issue `43c7615`):
    ///
    /// - `child == target` is rejected with `Error::SelfDependency`
    ///   (self-deps make the issue permanently blocked by itself).
    /// - `target` is resolved against the bookmark; if it's not
    ///   present, return `Error::IssueNotFound { id: target }`. Both
    ///   checks happen before any mutating IO so a rejection leaves
    ///   the bookmark untouched.
    /// - For `kind` in `{Blocks, ParentChild}`, a forward DFS from
    ///   `target` over the combined blocking graph — every existing
    ///   `Blocks` *or* `ParentChild` edge. If `id` is reachable, the
    ///   new edge would close a cycle in the graph
    ///   `compute_blocked_set` walks at read time. Reject with
    ///   `Error::DependencyCycle` carrying the path
    ///   `[target, ..., id]`. Both kinds participate because the
    ///   ready-set computation propagates blocked-ness along both:
    ///   `A blocks B` plus `B parent-of A` makes both A and B
    ///   permanently blocked, even though neither single-kind walk
    ///   sees a cycle. The combined-graph check covers that
    ///   (`121f48b`). `Related` and `DiscoveredFrom` edges do not
    ///   participate in blocking and are not cycle-checked.
    pub fn add_dep_edge(
        &self,
        id: &IssueId,
        target: &IssueId,
        kind: DepKind,
    ) -> Result<()> {
        if id == target {
            return Err(Error::SelfDependency { id: id.clone() });
        }
        let target = target.clone();
        // Target-existence is a domain precondition vs. OTHER records
        // (the target's ref). Probed inside the closure so a CAS-loss
        // retry re-checks against the post-race bookmark — if another
        // writer deleted the target between attempts, we surface a
        // typed IssueNotFound instead of writing a dangling dep edge
        // (`a6b8fb7`).
        self.mutate(id, &format!("jjf: issue {} - dep-add", id), |rec| {
            match self.issue_exists_on_bookmark(&target) {
                Ok(true) => {}
                Ok(false) => {
                    return MutateOutcome::Conflict(Error::IssueNotFound(
                        target.clone(),
                    ));
                }
                Err(e) => return MutateOutcome::Conflict(e),
            }
            // Cycle preflight (v2.6, `43c7615`; v2.x mixed-kind
            // `121f48b`). Walks the combined blocking graph —
            // `Blocks` plus `ParentChild` — because
            // `compute_blocked_set` propagates blocked-ness along
            // BOTH kinds: a `blocks` cycle and a mixed
            // `blocks`+`parent-child` cycle both produce permanent
            // lockouts at read time. Probed inside the closure so a
            // CAS-loss retry re-walks the post-race graph: if
            // another writer landed a cycle-closing edge between
            // our attempts, we surface the cycle from the fresh
            // state instead of compounding it.
            //
            // De-dup short-circuit: if the same `(target, kind)` edge
            // is already on the record, the operation is a no-op
            // (matches the post-MutateOutcome::Write branch below)
            // and the cycle walk would falsely report the existing
            // edge as a cycle. Skip the walk in that case.
            //
            // `Related` and `DiscoveredFrom` don't participate in
            // blocking — they're skipped entirely.
            if matches!(kind, DepKind::Blocks | DepKind::ParentChild)
                && !rec
                    .dependencies
                    .iter()
                    .any(|d| d.target == target && d.kind == kind)
            {
                match self.find_blocking_cycle(&rec.id, &target) {
                    Ok(Some(cycle)) => {
                        return MutateOutcome::Conflict(Error::DependencyCycle {
                            from: rec.id.clone(),
                            target: target.clone(),
                            cycle,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => return MutateOutcome::Conflict(e),
                }
            }
            let edge = DepEdge {
                target: target.clone(),
                kind,
            };
            if !rec
                .dependencies
                .iter()
                .any(|d| d.target == edge.target && d.kind == edge.kind)
            {
                rec.dependencies.push(edge);
                rec.dependencies.sort();
            }
            MutateOutcome::Write(vec![Op::DepAdd {
                issue_id: rec.id.clone(),
                dep: target.clone(),
                kind,
            }])
        })
        .map(|_| ())
    }

    /// Walk the combined blocking graph forward from `target`. The
    /// "blocking graph" is the union of `Blocks` and `ParentChild`
    /// edges — the two edge kinds whose presence makes an issue
    /// blocked via `compute_blocked_set`. If `source` is reachable,
    /// returns the cycle path `[target, ..., source]` — the nodes
    /// that would, together with the new edge `source -> target`,
    /// form a back-edge.
    ///
    /// DFS with a visited set. Every issue on the bookmark is a node;
    /// closed issues are NOT excluded (see `Error::DependencyCycle`
    /// for the reasoning). Issues whose record is missing (a dangling
    /// edge — possible after a `dep add` followed by the target
    /// being deleted) are treated as leaves; we can't recurse without
    /// data, but they also can't host a back-edge so the cycle
    /// detection stays sound.
    ///
    /// `Related` and `DiscoveredFrom` edges have no blocking effect
    /// and are skipped during the walk.
    ///
    /// Reads via the snapshot cache. v2.6 generalized from
    /// `find_blocks_cycle` in `121f48b` to cover mixed-kind cycles.
    fn find_blocking_cycle(
        &self,
        source: &IssueId,
        target: &IssueId,
    ) -> Result<Option<Vec<IssueId>>> {
        let snapshot = self.snapshot()?;
        // DFS frame: (node, iterator-over-children-already-consumed).
        // We use an explicit stack so the chain can be reconstructed
        // from the stack when the back-edge is hit, without a
        // separate parent-pointer map.
        let mut stack: Vec<IssueId> = vec![target.clone()];
        let mut path: Vec<IssueId> = vec![target.clone()];
        let mut visited: std::collections::HashSet<IssueId> =
            std::collections::HashSet::new();
        visited.insert(target.clone());

        // Iterative DFS: pop a node, push every unvisited
        // blocking-target. Track the active path so we can echo the
        // cycle on hit. The stack-and-path split lets the path stay
        // a clean chain.
        while let Some(node) = stack.last().cloned() {
            // Find next unvisited blocking-child of `node` (either
            // `Blocks` or `ParentChild` kind). If none, pop the
            // frame and shrink the path.
            let next_child = match snapshot.issues.get(&node) {
                Some(issue) => issue
                    .dependencies
                    .iter()
                    .find(|e| {
                        matches!(e.kind, DepKind::Blocks | DepKind::ParentChild)
                            && !visited.contains(&e.target)
                    })
                    .map(|e| e.target.clone()),
                None => None, // dangling target: leaf node
            };
            match next_child {
                Some(child) => {
                    if &child == source {
                        // Found the back-edge: chain is
                        // [target, ..., node, source].
                        path.push(child);
                        return Ok(Some(path));
                    }
                    visited.insert(child.clone());
                    stack.push(child.clone());
                    path.push(child);
                }
                None => {
                    stack.pop();
                    path.pop();
                }
            }
        }
        Ok(None)
    }

    /// Remove a typed dependency edge. Symmetric to
    /// [`Storage::add_dep_edge`]; only edges with the matching
    /// `(target, kind)` are removed, leaving other-kind edges to the
    /// same target intact. v2.4 (`agent-dep-types`).
    pub fn remove_dep_edge(
        &self,
        id: &IssueId,
        target: &IssueId,
        kind: DepKind,
    ) -> Result<()> {
        let target = target.clone();
        self.mutate(id, &format!("jjf: issue {} - dep-rm", id), |rec| {
            rec.dependencies
                .retain(|d| !(d.target == target && d.kind == kind));
            MutateOutcome::Write(vec![Op::DepRm {
                issue_id: rec.id.clone(),
                dep: target.clone(),
                kind,
            }])
        })
        .map(|_| ())
    }

    /// Build the parent-child tree rooted at `root_id`. Walks the
    /// dependency edges in the OPPOSITE direction of how they're
    /// stored: for each issue X with `parent-child` edge pointing at
    /// `root_id`, X is a CHILD of `root_id`. We recurse depth-first
    /// from the root, collecting children at each level.
    ///
    /// Returns a [`DepTree`] whose nodes are issues and whose edges
    /// follow the parent-child relation. Cycles are detected via a
    /// visited set; a cycled node appears once in the tree, then
    /// recursion stops. Issues unreachable from `root_id` via the
    /// parent-child relation are NOT included.
    ///
    /// v2.4 (`agent-dep-types`). Implementation: O(N) over every
    /// issue on the bookmark per recursion level (we re-scan the
    /// dependency list of each candidate to find children). For the
    /// live planner's small N this is fine; if it gets slow, build
    /// the reverse-index map once.
    pub fn dep_tree(&self, root_id: &IssueId) -> Result<DepTree> {
        // Load every issue once. The tree we return only carries
        // (id, title, status) — the tree consumer can `read` for
        // more — but we need the full deps field of every candidate
        // to find children.
        //
        // Snapshot cache (per `docs/storage-index-design.md`)
        // replaces the prior N-spawn `read()` loop. V2 and V3 both
        // route through `snapshot()`; the cache module probes the
        // appropriate invalidation key and rebuilds on a miss.
        let snapshot = self.snapshot()?;
        let all: Vec<Issue> = snapshot.issues.values().cloned().collect();

        // Build a child index: for each `parent-child` edge X →
        // target, register X as a child of `target`. Iterating the
        // map gives a deterministic child order if we sort by
        // child-id before insert; we accumulate into a Vec keyed
        // by parent id and sort within each parent's child list.
        let mut children_of: std::collections::BTreeMap<IssueId, Vec<IssueId>> =
            std::collections::BTreeMap::new();
        for issue in &all {
            for edge in &issue.dependencies {
                if edge.kind == DepKind::ParentChild {
                    children_of
                        .entry(edge.target.clone())
                        .or_default()
                        .push(issue.id.clone());
                }
            }
        }
        for v in children_of.values_mut() {
            v.sort();
        }

        let issue_by_id: std::collections::HashMap<IssueId, &Issue> =
            all.iter().map(|i| (i.id.clone(), i)).collect();

        // DFS walk. The root is included even if it's not in
        // `issue_by_id` (defensive — a dangling id) but its children
        // will be empty.
        fn walk(
            root: &IssueId,
            issue_by_id: &std::collections::HashMap<IssueId, &Issue>,
            children_of: &std::collections::BTreeMap<IssueId, Vec<IssueId>>,
            visited: &mut std::collections::HashSet<IssueId>,
        ) -> DepTreeNode {
            let already_seen = !visited.insert(root.clone());
            let (title, status) = match issue_by_id.get(root) {
                Some(i) => (i.title.clone(), i.status),
                None => (String::new(), Status::Open),
            };
            if already_seen {
                // Cycle — don't recurse, but include the node so
                // the cycle is visible in the rendered tree.
                return DepTreeNode {
                    id: root.clone(),
                    title,
                    status,
                    children: Vec::new(),
                    cycle: true,
                };
            }
            let mut children_nodes: Vec<DepTreeNode> = Vec::new();
            if let Some(child_ids) = children_of.get(root) {
                for cid in child_ids {
                    children_nodes.push(walk(cid, issue_by_id, children_of, visited));
                }
            }
            DepTreeNode {
                id: root.clone(),
                title,
                status,
                children: children_nodes,
                cycle: false,
            }
        }

        let mut visited = std::collections::HashSet::new();
        let root = walk(root_id, &issue_by_id, &children_of, &mut visited);
        Ok(DepTree { root })
    }

    /// Append a comment. Generates a fresh 7-hex comment id and updates
    /// the issue record's `updated_at`. Returns the freshly-generated
    /// comment id so callers (notably `jjf comment`) can surface it in
    /// machine-readable output.
    ///
    /// On a concurrent-write race (another writer landed first), this
    /// auto-retries with bounded exponential backoff. Each retry
    /// re-reads the comments file so the racer's comment is preserved
    /// alongside ours (naïve retry with the stale `existing_comments`
    /// snapshot would clobber it).
    ///
    /// Retry budget is governed by [`RetryPolicy::from_env`] —
    /// defaults to 5 retries (6 total attempts) with a 10/25/60/150/350
    /// ms backoff. `JJF_MAX_RETRIES` and `JJF_RETRY_BASE_MS` env vars
    /// tune this for tests.
    pub fn add_comment(&self, id: &IssueId, body: &str, author: &str) -> Result<IssueId> {
        if author.trim().is_empty() {
            return Err(Error::Invalid("comment author must not be empty".into()));
        }
        // Comment body cap. Same shape (free-form markdown), same
        // on-disk surface (`<id>.comments.jsonl`), same risk
        // (unbounded resource on a per-write basis). Apply the same
        // 65,536-byte limit as issue bodies. Issue `679444a`.
        if let Err(reason) = validate_body(body) {
            return Err(Error::InvalidBody { reason });
        }
        let comment_id = IssueId::random();
        let policy = RetryPolicy::from_env();
        run_with_retry(
            policy,
            || self.add_comment_once(id, body, author, &comment_id),
            || self.invalidate_snapshot_memo(),
        )
        .map(|()| comment_id.clone())
    }

    /// Single attempt of [`Storage::add_comment`]: read the record and
    /// existing comments from the bookmark, append a fresh comment,
    /// and commit. Returns a typed [`Error::ConcurrentWrite`] if the
    /// commit dance races. The retry wrapper in `add_comment` re-runs
    /// this against the post-race bookmark state — crucial: both
    /// readers see the WINNER's comments now, so re-appending OUR
    /// comment preserves the racer's comment alongside ours.
    fn add_comment_once(
        &self,
        id: &IssueId,
        body: &str,
        author: &str,
        comment_id: &IssueId,
    ) -> Result<()> {
        let id = id.clone();
        let body = body.to_owned();
        let author = author.to_owned();
        // The issue record's update + the comments file edit are part of
        // one commit. We can't piggyback `add_comment` on `mutate()`
        // because the comments file isn't part of the JSON record.
        let mut record = self.read_record_from_bookmark(&id)?;
        let existing_comments = self.read_comments_from_bookmark(&id)?;
        record.updated_at = now_rfc3339()?;
        let comment = Comment {
            id: comment_id.clone(),
            author,
            created_at: record.updated_at.clone(),
            body,
        };
        let summary = format!("jjf: issue {} - comment-add", id);
        let mut all_comments = existing_comments;
        all_comments.push(comment);
        let ops = vec![Op::CommentAdd {
            issue_id: id.clone(),
            comment_id: comment_id.clone(),
        }];
        if self.mode == StorageMode::V3 {
            // V3: write the new record + the full comments stream to
            // the per-issue ref's tree in one commit. The comments
            // file is always present on a v3 commit-with-comments —
            // we read the existing stream above, appended ours, and
            // pass the full slice through.
            self.commit_record_v3(
                &id,
                &record,
                Some(&all_comments),
                &summary,
                &ops,
            )?;
        } else {
            self.commit_record_change(&summary, &ops, |wc_root| {
                write_record_json(&wc_root.join(issue_json_relpath(&id)), &record)?;
                write_comments_jsonl(
                    &wc_root.join(issue_comments_relpath(&id)),
                    &all_comments,
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Write a persistent memory keyed by `key`. Upsert semantics: if a
    /// memory with that key already exists, update its `value` and
    /// `updated_at`; otherwise create a fresh record. Spec v2.2 §10.
    ///
    /// Lands one commit on the `issues` bookmark with a single
    /// `Jjf-Op: set-memory` trailer. The file written is
    /// `memories/<key>.json`.
    ///
    /// Errors:
    /// - [`Error::Invalid`] if `key` is empty or contains characters
    ///   outside `[a-z0-9-]` or violates slug-shape rules.
    /// - [`Error::Invalid`] if `value` is empty after trim — an empty
    ///   memory is almost certainly an operator mistake.
    /// - `Jj` / `Io` from the underlying write dance.
    pub fn set_memory(&self, key: &str, value: &str) -> Result<()> {
        if value.trim().is_empty() {
            return Err(Error::Invalid("memory value must not be empty".into()));
        }
        // Memory keys reuse the slug validation rules: kebab-case,
        // `[a-z0-9-]`, length 3-48. (We deliberately reuse the slug
        // validator rather than introducing a parallel rule set; the
        // shape is intentionally identical.)
        if let Err(reason) = validate_slug(key) {
            return Err(Error::Invalid(format!(
                "invalid memory key {key:?}: {reason}"
            )));
        }
        let now = now_rfc3339()?;
        let existing = self.read_memory_from_bookmark(key)?;
        let record = match existing {
            Some(mut prev) => {
                prev.value = value.to_owned();
                prev.updated_at = now;
                prev
            }
            None => Memory {
                key: key.to_owned(),
                value: value.to_owned(),
                created_at: now.clone(),
                updated_at: now,
            },
        };
        let summary = format!("jjf: memory {} - set", key);
        let jjf_at = now_rfc3339_nanos()?;
        let msg = memory::build_set_memory_commit_message(&summary, key, value, &jjf_at);
        if self.mode == StorageMode::V3 {
            return self.commit_memory_v3(key, Some(&record), &msg);
        }
        let key_owned = key.to_owned();
        self.commit_memory_change(&msg, |wc_root| {
            write_memory_json(&wc_root.join(memory::memory_json_relpath(&key_owned)), &record)
        })
    }

    /// Remove a persistent memory by key. Lands one commit on the
    /// `issues` bookmark with a single `Jjf-Op: unset-memory` trailer
    /// and deletes the on-disk `memories/<key>.json` file.
    ///
    /// Errors:
    /// - [`Error::Invalid`] if `key` is malformed.
    /// - [`Error::Invalid`] (with a `not found` message) if no memory
    ///   with that key exists at the bookmark tip — the CLI translates
    ///   this into an exit-1 "no memory with key" error.
    pub fn unset_memory(&self, key: &str) -> Result<()> {
        if let Err(reason) = validate_slug(key) {
            return Err(Error::Invalid(format!(
                "invalid memory key {key:?}: {reason}"
            )));
        }
        if self.read_memory_from_bookmark(key)?.is_none() {
            return Err(Error::Invalid(format!(
                "no memory with key {key:?}"
            )));
        }
        let summary = format!("jjf: memory {} - unset", key);
        let jjf_at = now_rfc3339_nanos()?;
        let msg = memory::build_unset_memory_commit_message(&summary, key, &jjf_at);
        if self.mode == StorageMode::V3 {
            return self.commit_memory_v3(key, None, &msg);
        }
        let key_owned = key.to_owned();
        self.commit_memory_change(&msg, |wc_root| {
            let path = wc_root.join(memory::memory_json_relpath(&key_owned));
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        })
    }

    /// Read one memory by key from the `issues` bookmark tip. Returns
    /// `Ok(None)` if no memory with that key exists.
    pub fn read_memory(&self, key: &str) -> Result<Option<Memory>> {
        // Cache fast path: HashMap lookup by key. Cache rebuild is
        // the same cost as the prior single-key direct read worst
        // case, and amortizes across subsequent calls. Same shape
        // for V2 (bookmark-tip-keyed) and V3 (ref-set-keyed) —
        // see `cache.rs::load_or_rebuild_v3`.
        let snapshot = self.snapshot()?;
        Ok(snapshot.memories.get(key).cloned())
    }

    /// Enumerate every memory present in authoritative storage,
    /// sorted by key ascending. V2 reads each `memories/<key>.json`
    /// off the bookmark tip; V3 enumerates `refs/jjf/memories/*` and
    /// reads each ref's `memory.json` blob.
    pub fn list_memories(&self) -> Result<Vec<Memory>> {
        // Snapshot cache: every Memory in authoritative storage is
        // in `snapshot.memories` (built from `memories/<key>.json`
        // on V2 or from `refs/jjf/memories/*` on V3). Skip the
        // per-key direct read loop entirely. See
        // `cache.rs::load_or_rebuild_v3` and
        // `docs/storage-index-design.md`.
        let snapshot = self.snapshot()?;
        let mut out: Vec<Memory> = snapshot.memories.values().cloned().collect();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Read a single issue back from the `issues` bookmark tip. Returns
    /// the latest scalar field values plus the full chronological
    /// comment thread. Errors with `IssueNotFound` if `issues/<id>.json`
    /// is absent at the bookmark.
    ///
    /// Implementation cross-checks the file-read view against an
    /// op-replay view in debug builds — see `read.rs` for the rules.
    ///
    /// **Snapshot cache.** Per `docs/storage-index-design.md`, this
    /// consults the snapshot cache before falling back to per-id
    /// shell-outs. A cache hit returns the pre-projected `Issue`
    /// (skipping the debug cross-check; the rebuild path projected
    /// from the same files `read::read` would have read). A cache
    /// miss falls back to the per-id path, which still runs the
    /// cross-check in debug.
    pub fn read(&self, id: &IssueId) -> Result<Issue> {
        // Both V2 and V3 route through the snapshot cache — the cache
        // module probes the appropriate invalidation key (bookmark
        // tip on V2, ref-set sha on V3) and rebuilds when stale.
        // See `cache.rs::load_or_rebuild_v3` for the v3 specifics.
        let snapshot = self.snapshot()?;
        if let Some(issue) = snapshot.issues.get(id) {
            return Ok(issue.clone());
        }
        // Cache miss for this id (very unusual — either a race with
        // a concurrent writer between probe and lookup, OR the id
        // genuinely isn't in authoritative storage). Fall through to
        // the per-id read for a sharp `IssueNotFound` error.
        read::read(&self.repo, &self.git, self.mode, id)
    }

    /// Resolve a user-supplied handle to a concrete `IssueId`.
    ///
    /// - If `handle` parses as an `IssueId` (7 lowercase-hex), return
    ///   that id directly. No existence check — callers that need
    ///   one get it from the subsequent `Storage::read` / mutator
    ///   call, which surfaces [`Error::IssueNotFound`] if the id
    ///   isn't on the bookmark. Partial / prefix ids are NOT
    ///   accepted; the full 7-char id is the canonical handle.
    /// - Otherwise (non-id string), walk every issue on the bookmark
    ///   and return the id whose slug matches `handle` exactly.
    ///   Exact-match, case-sensitive (slugs are kebab-case lowercase
    ///   per validation).
    /// - If no issue's slug matches, return [`Error::SlugNotFound`].
    ///
    /// Implementation is read-all-then-match: O(N) over every
    /// issue in the snapshot cache. For v3's small N this is fine.
    /// The match scans both open AND closed issues (so the
    /// operator can `jjf show <slug>` against a closed handle).
    /// Per spec v2.6, slug uniqueness is now enforced across the
    /// full history at write time — but historical pre-v2.6 repos
    /// may carry duplicate slugs across an open/closed pair. In
    /// that case the resolver returns the ACTIVE holder (the
    /// `slug_index` puts active issues in first; see
    /// `cache::SnapshotCache::from_parts_with_kind`).
    pub fn resolve(&self, handle: &str) -> Result<IssueId> {
        // Id path: handle IS a full id. Existence check is the
        // caller's job via Storage::read.
        if let Ok(id) = IssueId::parse(handle) {
            return Ok(id);
        }
        // Slug path: HashMap lookup on the snapshot cache's
        // pre-built `slug_index`. V2 and V3 both route through
        // `snapshot()`; the cache module handles the appropriate
        // probe / rebuild. Before the cache, this was an O(N)
        // shell-out loop — see closing comment on `b9f628b`.
        let snapshot = self.snapshot()?;
        if let Some(id) = snapshot.slug_index.get(handle) {
            return Ok(id.clone());
        }
        // Defensive fallback: under spec v2.6 the slug_index
        // carries every issue's slug regardless of status, so the
        // HashMap lookup above should never miss. The linear scan
        // remains for resilience against an unexpectedly stale
        // index (e.g., a corrupt cache file the rebuild couldn't
        // detect) and to surface duplicate-slug pre-v2.6 holders
        // the HashMap might have de-duplicated.
        for issue in snapshot.issues.values() {
            if issue.slug.as_deref() == Some(handle) {
                return Ok(issue.id.clone());
            }
        }
        Err(Error::SlugNotFound {
            handle: handle.to_owned(),
        })
    }

    /// Probe for a slug collision across ALL issues — Open,
    /// InProgress, Blocked, AND Closed. Returns `Some(id)` for any
    /// issue holding this exact slug, `None` if the slug is free.
    /// `self_id` (if provided) is excluded from the probe — used by
    /// the update path so re-setting an issue's existing slug
    /// doesn't self-conflict.
    ///
    /// Spec v2.6 widened the scope to all statuses: closed issues
    /// retain their slug forever. A new ticket must pick a fresh
    /// one. The pre-v2.6 behavior (closed issues released their
    /// slug, opening it for re-use) silently shadowed the closed
    /// issue for slug-resolved discovery, which is the wrong
    /// default for an audit-trail planner (issue `a105e0b`).
    fn find_slug_collision(
        &self,
        slug: &str,
        self_id: Option<&IssueId>,
    ) -> Result<Option<IssueId>> {
        // Snapshot cache: the cache's slug_index carries EVERY
        // slug holder regardless of status (see
        // `cache::SnapshotCache::from_parts_with_kind`). One
        // HashMap lookup replaces the per-id direct-read loop.
        // V2 and V3 both route through here — the cache module
        // probes the appropriate invalidation key (bookmark head
        // vs ref-set sha) and rebuilds when stale.
        let snapshot = self.snapshot()?;
        if let Some(holder) = snapshot.slug_index.get(slug) {
            if Some(holder) == self_id {
                return Ok(None);
            }
            return Ok(Some(holder.clone()));
        }
        Ok(None)
    }

    /// Enumerate every issue id present at the `issues` bookmark tip.
    ///
    /// Shells out to `jj file list -r bookmarks(issues) root:issues/`,
    /// parses the `issues/<id>.json` filenames out, and returns the
    /// sorted-ascending unique id set. `<id>.comments.jsonl` siblings
    /// are ignored (they belong to issues that also have a `.json` and
    /// double-counting would mis-report issue counts); files that don't
    /// match the `issues/<7-hex>.json` pattern are skipped silently so a
    /// future seed-bookmark housekeeping file or stray artifact doesn't
    /// crash enumeration.
    ///
    /// Order: ascending by hex id. Stable, deterministic, cheap.
    /// Callers that want time order can `read` each one and sort by
    /// `created_at` afterward.
    ///
    /// This is the storage layer's first multi-issue enumeration
    /// primitive — `jjf ls` is the v1 caller, but `jjf log
    /// --issue-changes`, agent `ready` selection, and the PWA's home
    /// view will all sit on top of it.
    pub fn list_ids(&self) -> Result<Vec<IssueId>> {
        // Snapshot cache provides every id with one probe (bookmark
        // head on V2, ref-set sha on V3). On a miss, the rebuild
        // reads every record via the storage-mode-appropriate path —
        // one batched `jj file show` on V2, N `git cat-file blob`s on
        // V3. See `cache.rs` and `docs/storage-index-design.md`.
        let snapshot = self.snapshot()?;
        let mut ids: Vec<IssueId> = snapshot.issues.keys().cloned().collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Enumerate every issue whose dependencies are all closed —
    /// the agent-ready set.
    ///
    /// An issue is "ready" iff:
    ///
    /// - Its `status` is [`Status::Open`]. With
    ///   `filter.include_claimed = true`, [`Status::InProgress`]
    ///   issues are also included; otherwise they're excluded as
    ///   "claimed by another agent" (v2.3 `agent-claim-atomic`).
    /// - Every id in its `dependencies` field either points at a
    ///   [`Status::Closed`] issue OR at a non-existent issue id (a
    ///   dangling reference). Open deps block; closed and dangling
    ///   deps don't. (An `InProgress` dep blocks just like an Open
    ///   one — it's not done yet.)
    /// - It passes any `filter.labels` (AND across labels — same
    ///   semantics as `jjf ls --label`).
    /// - It passes any `filter.types` (OR across types — same
    ///   semantics as `jjf ls --type`).
    ///
    /// Return order is the agent priority:
    ///
    /// 1. Primary: type priority, in this order
    ///    [`IssueType::Bug`] > [`IssueType::Feature`] >
    ///    [`IssueType::Research`] > [`IssueType::Epic`] >
    ///    [`IssueType::Unspecified`]. [`IssueType::Roadmap`] is
    ///    excluded entirely — the roadmap ticket isn't work to do.
    /// 2. Secondary: `created_at` ascending (FIFO — agents grind the
    ///    oldest unblocked work down first).
    ///
    /// If `filter.limit` is `Some(n)`, the returned vec is truncated
    /// to `n` entries AFTER sorting. `None` returns every match.
    ///
    /// Implementation is read-all-then-filter: O(N) over every
    /// `issues/<id>.json` at the bookmark tip. For the live planner's
    /// small N this is fine; if it ever proves slow, a persistent
    /// index is the follow-up (out of scope per the ticket).
    pub fn list_ready(&self, filter: &ReadyFilter) -> Result<Vec<Issue>> {
        // Read every issue from authoritative storage. We need full
        // records (status, type_, dependencies, labels) for both the
        // candidate set and the dep-status lookup.
        //
        // Snapshot cache (per `docs/storage-index-design.md`): probe
        // the invalidation key (bookmark head on V2, ref-set sha on
        // V3), load `.jj/jjforge-cache.json` on a hit, rebuild via
        // one batched `jj file show` (V2) or N `git cat-file` calls
        // (V3) on a miss. See `cache.rs::load_or_rebuild_v3`.
        let snapshot = self.snapshot()?;
        let all: Vec<Issue> = snapshot.issues.values().cloned().collect();

        let blocked = compute_blocked_set(&all);

        // Roadmap-type issues are never returned. They're never work
        // to do; they're the planning surface itself. The ticket and
        // the closing comment on `7100b51` both pin this.
        //
        // v2.3 (`agent-claim-atomic`): InProgress issues are excluded
        // by default — they're claimed by another agent. The
        // `include_claimed` flag flips them back in for "what's in
        // flight" views.
        //
        // v2.4 (`agent-dep-types`): "blocked" is now computed via
        // [`compute_blocked_set`] — a fixpoint over `blocks` (hard
        // prereq) and `parent-child` (cascade-via-parent) edges.
        // `related` and `discovered-from` never affect blocked.
        let include_claimed = filter.include_claimed;
        let include_blocked = filter.include_blocked;
        let mut ready: Vec<Issue> = all
            .into_iter()
            .filter(|i| match i.status {
                Status::Open => true,
                Status::Blocked => include_blocked,
                Status::InProgress => include_claimed,
                // v2.7 (`abandon-verb`): Abandoned is excluded
                // unconditionally — abandoning means "this issue
                // should never come up again." No override flag
                // (unlike `include_blocked` / `include_claimed`).
                Status::Closed | Status::Abandoned => false,
            })
            .filter(|i| i.type_ != IssueType::Roadmap)
            .filter(|i| !blocked.contains(&i.id))
            .filter(|i| labels_match_all(&i.labels, &filter.labels))
            .filter(|i| types_match_any(i.type_, &filter.types))
            .collect();

        // Sort: type priority, then created_at ASC. Stable sort means
        // equal-priority entries fall back to the second key cleanly.
        ready.sort_by(|a, b| {
            ready_priority(a.type_)
                .cmp(&ready_priority(b.type_))
                .then_with(|| a.created_at.cmp(&b.created_at))
        });

        if let Some(n) = filter.limit {
            ready.truncate(n);
        }
        Ok(ready)
    }

    /// Case-insensitive substring search across every issue's title,
    /// body, and (when `include_comments` is set) comment bodies.
    /// Returns one [`SearchHit`] per issue that matched, unsorted —
    /// the caller is responsible for sort + limit + filter
    /// composition. The order in the returned vec is the snapshot
    /// HashMap's iteration order; callers shouldn't rely on it.
    ///
    /// Match semantics:
    ///
    /// - Substring, not regex. `q.to_lowercase().contains(...)` is the
    ///   primitive.
    /// - Empty query (`""`) returns an empty vec — match-everything is
    ///   `jjf ls`'s job, not `search`'s. Skipping early also keeps the
    ///   storage layer's contract honest (no surprise full-table scan
    ///   on the empty input).
    /// - Comments are searched only when `include_comments` is true.
    ///   The snapshot cache already materializes every comment inline
    ///   on the projected [`Issue`] (see [`cache::SnapshotCache`]), so
    ///   `include_comments` is a per-issue iteration toggle, not a
    ///   separate IO path.
    ///
    /// `MatchedField` priority: when an issue hits in more than one
    /// field, the more specific surface wins — `Title > Body >
    /// Comments`. This makes "is this title-relevant?" the dominant
    /// signal in mixed-hit cases, which matches how a human triages a
    /// search result.
    ///
    /// `score` is the total hit count across every searched field on
    /// the issue (title + body + every comment body when included).
    /// Multi-field hits dominate single-field hits naturally without
    /// bringing in BM25/TF-IDF — see the ticket's "Out of scope"
    /// section.
    ///
    /// `snippet` is a [`make_snippet`] preview of the matched field
    /// around the first occurrence; `snippet_context` is the half-
    /// width of the window. Pass [`DEFAULT_SNIPPET_CONTEXT`] when the
    /// caller has no opinion. See [`make_snippet`] for the windowing
    /// rules (char-boundary safe, newlines normalized, leading/
    /// trailing ellipsis on truncation).
    ///
    /// Implementation reads the snapshot cache (one probe + maybe one
    /// rebuild — same shape as [`Storage::list_ready`]) and runs the
    /// substring scan in memory. No new IO beyond the snapshot.
    pub fn search(
        &self,
        q: &str,
        include_comments: bool,
        snippet_context: usize,
    ) -> Result<Vec<SearchHit>> {
        // Empty query is match-nothing. Returning the snapshot's full
        // contents would silently make `search ""` an alias for `ls
        // --status all`, which is the wrong default.
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let needle = q.to_lowercase();
        let snapshot = self.snapshot()?;
        let mut out: Vec<SearchHit> = Vec::new();
        for issue in snapshot.issues.values() {
            // Per-field hit counts. Title and body always searched;
            // comments only when the toggle is on.
            let title_hits = count_ci(&issue.title, &needle);
            let body_hits = count_ci(&issue.body, &needle);
            let comments_hits: usize = if include_comments {
                issue
                    .comments
                    .iter()
                    .map(|c| count_ci(&c.body, &needle))
                    .sum()
            } else {
                0
            };
            let score = title_hits + body_hits + comments_hits;
            if score == 0 {
                continue;
            }
            // Priority resolution: which field carries the snippet
            // and the matched_field tag. Title > Body > Comments.
            let (matched_field, snippet) = if title_hits > 0 {
                (
                    MatchedField::Title,
                    make_snippet(&issue.title, &needle, snippet_context),
                )
            } else if body_hits > 0 {
                (
                    MatchedField::Body,
                    make_snippet(&issue.body, &needle, snippet_context),
                )
            } else {
                // include_comments must be true here (otherwise
                // comments_hits == 0 and we'd have continued above).
                // The first comment whose body matches carries the
                // snippet; comments are stored chronologically per
                // `Storage::read`'s contract, so "first match" is
                // deterministic across runs.
                let first = issue
                    .comments
                    .iter()
                    .find(|c| count_ci(&c.body, &needle) > 0)
                    .map(|c| make_snippet(&c.body, &needle, snippet_context))
                    .unwrap_or_default();
                (MatchedField::Comments, first)
            };
            out.push(SearchHit {
                issue: issue.clone(),
                matched_field,
                score,
                snippet,
            });
        }
        Ok(out)
    }

    /// Return every issue whose `updated_at` is strictly older than
    /// `now - threshold_secs`. Sort ascending by `updated_at` (oldest
    /// first) so the caller's render loop walks the highest-priority
    /// rows first.
    ///
    /// Boundary semantics: strict `>` against the threshold. An issue
    /// whose age in seconds is exactly `threshold_secs` is NOT stale.
    /// Reasoning: with second-resolution `updated_at` stamps a
    /// boundary-matching issue was touched right at the threshold tick
    /// — that's "just barely fresh", not "just barely stale". This
    /// matches a human reading of "issues older than N days".
    ///
    /// `now` reads through the same env-pinned clock path
    /// [`now_rfc3339`] uses, so tests can hold the clock steady via
    /// `JJF_TEST_CLOCK_SECS`. Production code never sets that env
    /// var.
    ///
    /// The threshold is in seconds (not days) so the storage layer
    /// stays time-unit-agnostic; the CLI converts at the
    /// `--days N -> threshold_secs = N * 86400` boundary.
    ///
    /// `updated_at` is bumped only by mutating verbs today (per the
    /// storage spec); commenting on an issue does NOT bump it. This
    /// fn uses the field as-is and inherits that contract — see the
    /// `host-asterinas-stale` ticket's "Out of scope" section. A
    /// "stale by activity" surface (comments-count-as-touches) is a
    /// separate decision tracked on the storage spec.
    ///
    /// Reads through the snapshot cache (one probe + maybe one
    /// rebuild — same shape as [`Storage::search`] / [`Storage::list_ready`]).
    /// No new IO.
    pub fn stale(&self, threshold_secs: u64) -> Result<Vec<StaleHit>> {
        let now_secs = current_epoch_secs()?;
        let snapshot = self.snapshot()?;
        let mut out: Vec<StaleHit> = Vec::new();
        for issue in snapshot.issues.values() {
            let updated_secs = match rfc3339_to_epoch_secs(&issue.updated_at) {
                Some(s) => s,
                // An unparseable stamp can't be reasoned about; skip
                // rather than panic. In practice every record carries
                // a stamp written by `now_rfc3339`, which always emits
                // the strict `YYYY-MM-DDTHH:MM:SSZ` shape this parser
                // accepts. Defensive against a future spec change.
                None => continue,
            };
            // Guard against future-dated `updated_at` (clock skew on a
            // peer that pushed). `saturating_sub` keeps the math safe;
            // a future-dated issue surfaces as `seconds_since_update
            // = 0`, which is never `> threshold_secs` for any
            // positive threshold — so future-dated issues are never
            // stale, which matches intuition.
            let age = now_secs.saturating_sub(updated_secs);
            if age > threshold_secs {
                out.push(StaleHit {
                    issue: issue.clone(),
                    seconds_since_update: age,
                });
            }
        }
        // Oldest first. `updated_at` is RFC 3339 with second-resolution
        // and a trailing `Z`, so lexicographic order == chronological
        // order — same trick `run_ls` uses on `created_at`.
        out.sort_by(|a, b| a.issue.updated_at.cmp(&b.issue.updated_at));
        Ok(out)
    }

    /// Enumerate the change_id shorts of every head of the `issues`
    /// bookmark. Normally returns exactly one entry; returns more than
    /// one only when the bookmark is in a divergent ("conflicted")
    /// state — typically right after `jj git fetch` against a local
    /// clone that made a concurrent edit. The `sync-push-pull` ticket
    /// uses this to decide whether `pull` needs to invoke the merge
    /// driver pass: count > 1 means yes.
    ///
    /// Empty result is impossible on a repo where `jjf init` has run
    /// (the seed commit guarantees at least one head); we treat an
    /// empty list as a `Jj` error from the caller's perspective rather
    /// than a typed variant, because hitting it means the bookmark
    /// vanished between probes.
    pub fn issues_heads(&self) -> Result<Vec<String>> {
        let text = self.repo.run(&[
            "log",
            "-r",
            "heads(bookmarks(issues))",
            "--no-graph",
            "-T",
            "change_id.short() ++ \"\\n\"",
        ])?;
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if !line.is_empty() {
                out.push(line.to_owned());
            }
        }
        Ok(out)
    }

    /// Land a merge commit that resolves a divergent `issues` bookmark.
    ///
    /// Takes the change_id shorts of the heads being merged (`heads` —
    /// typically two, but jj supports n-way merges so we accept any
    /// `>= 2`) and a list of `(issue_id, resolved_file_bytes)` pairs to
    /// write into the working copy of the merge commit. The bytes are
    /// what `jjf_merge::resolve` produced (or whatever the caller
    /// decided is the post-merge content); we write them verbatim into
    /// `issues/<id>.json` so what the file ends up containing on the
    /// bookmark matches exactly.
    ///
    /// Per `docs/storage-format.md` §5.2, the resulting commit's
    /// description carries one `Jjf-Op: merge` trailer per entry in
    /// `merged_issues` — that's the per-issue audit signal that the
    /// merge driver ran. Multi-issue merges land all trailers on one
    /// commit (spec §5.5).
    ///
    /// Sequence (variant of the standard 4-CLI dance):
    ///
    /// 1. `jj new <head_a> <head_b> [...] -m '<msg with merge trailers>'`
    ///    — creates the merge commit. jj materializes any conflicted
    ///    files with its textual conflict markers.
    /// 2. Write the resolved bytes into `issues/<id>.json` for each
    ///    entry. Overwrites the conflict markers jj just wrote.
    /// 3. `jj bookmark set issues -r @ --allow-backwards` — point the
    ///    bookmark at the merge commit.
    /// 4. `jj new root()` — step `@` off the bookmark.
    ///
    /// Errors with `Invalid` if `heads.len() < 2` (no merge needed) or
    /// `merged_issues` is empty (nothing to write — the caller should
    /// just `jj bookmark set` without going through the merge driver).
    pub fn record_merge(
        &self,
        heads: &[String],
        merged_issues: &[(IssueId, String)],
    ) -> Result<()> {
        if heads.len() < 2 {
            return Err(Error::Invalid(format!(
                "record_merge requires >= 2 heads, got {}",
                heads.len()
            )));
        }
        if merged_issues.is_empty() {
            return Err(Error::Invalid(
                "record_merge requires at least one merged issue".into(),
            ));
        }

        // 1. Build the merge commit. `jj new` with N positional
        // revisions creates an N-parent merge change.
        let summary = if merged_issues.len() == 1 {
            format!("jjf: issue {} - merge", merged_issues[0].0)
        } else {
            format!("jjf: merge {} issues", merged_issues.len())
        };
        let ops: Vec<Op> = merged_issues
            .iter()
            .map(|(id, _)| Op::Merge {
                issue_id: id.clone(),
            })
            .collect();
        let jjf_at = now_rfc3339_nanos()?;
        let msg = build_commit_message(&summary, &ops, &jjf_at);

        // `jj new <r1> <r2> ... -m <msg>` — args are owned `String`s so
        // we build a `Vec<&str>` before handing to `run`.
        let mut argv: Vec<&str> = vec!["new"];
        for h in heads {
            argv.push(h.as_str());
        }
        argv.push("-m");
        argv.push(&msg);
        self.repo.run(&argv)?;

        // 2. Write resolved bytes into the working copy. We don't read
        // the existing content; we overwrite. jj snapshots on the next
        // command.
        let wc_root = self.repo.root();
        for (id, bytes) in merged_issues {
            let path = wc_root.join(issue_json_relpath(id));
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, bytes)?;
        }

        // 3. Point the bookmark at the new merge commit.
        self.repo.run(&[
            "bookmark",
            "set",
            ISSUES_BOOKMARK,
            "-r",
            "@",
            "--allow-backwards",
        ])?;

        // 4. Step `@` off the bookmark so subsequent mutations don't
        // snapshot the merge commit again.
        self.repo.run(&["new", "root()"])?;

        // Drop the in-process snapshot memo — the bookmark moved.
        self.invalidate_snapshot_memo();

        Ok(())
    }

    /// Read the full op-by-op timeline for an issue, oldest first.
    ///
    /// Returns one [`HistoryEntry`] per `Jjf-Op:` stanza on the issue's
    /// commit chain. A commit with multiple ops (the create-time
    /// multi-op stanza of spec §5.7, or any other multi-op commit)
    /// emits one entry per op, all sharing `commit` / `author` /
    /// `timestamp`. Comment ops appear in the stream alongside scalar
    /// mutations — they're commits like any other.
    ///
    /// Errors with `IssueNotFound` if no commit on the `issues`
    /// bookmark touches this issue's files.
    pub fn read_history(&self, id: &IssueId) -> Result<Vec<HistoryEntry>> {
        // V3: walk the per-issue ref's commit chain via git, not jj's
        // bookmark-spanning revset. Same `HistoryEntry` shape, same
        // trailer parser; the only difference is the commit source.
        if self.mode == StorageMode::V3 {
            return history::read_history_at_v3(&self.git, id);
        }
        history::read_history(&self.repo, id)
    }

    /// Op-space resolver entry point. Discovers heads via
    /// [`Storage::issues_heads`]; for each issue touched on any head,
    /// walks each head's op chain via [`Storage::read_history_at`] and
    /// reduces field-by-field per spec §6's ordering tuple. Returns
    /// the merged state for every touched issue.
    ///
    /// **Does not land a commit.** Pair with
    /// [`Storage::record_merge_op_space`] to write the merged record
    /// + comments back and land a single merge commit with one
    /// `Jjf-Op: merge` trailer per resolved issue.
    ///
    /// Returns an empty [`MergeReport`] if `issues_heads()` finds zero
    /// or one head — there's no divergence to resolve.
    pub fn resolve_divergence(&self) -> Result<MergeReport> {
        let heads = self.issues_heads()?;
        if heads.len() < 2 {
            return Ok(MergeReport { issues: Vec::new() });
        }
        merge_ops::resolve(&self.repo, &heads)
    }

    /// Land the resolved merge as a single multi-parent commit on the
    /// `issues` bookmark. Companion to [`Storage::resolve_divergence`]:
    /// the report it returns plus the heads it walked feed straight
    /// into this call.
    ///
    /// Behavior:
    /// - Creates a merge commit with `heads` as its parents and one
    ///   `Jjf-Op: merge` trailer per issue in `report` (spec §5.7).
    /// - Writes the merged `issues/<id>.json` and
    ///   `issues/<id>.comments.jsonl` for every issue in the report,
    ///   overwriting whatever jj materialized in the merge's working
    ///   copy (including the textual conflict markers).
    /// - Points the `issues` bookmark at the merge commit and steps
    ///   `@` off it, matching the 4-CLI dance.
    ///
    /// Errors with `Invalid` if `heads.len() < 2` (no merge needed) or
    /// the report is empty (nothing to resolve — the caller should
    /// pin the bookmark via the file-bytes path or do nothing).
    pub fn record_merge_op_space(
        &self,
        heads: &[String],
        report: &MergeReport,
    ) -> Result<()> {
        if heads.len() < 2 {
            return Err(Error::Invalid(format!(
                "record_merge_op_space requires >= 2 heads, got {}",
                heads.len()
            )));
        }
        if report.issues.is_empty() {
            return Err(Error::Invalid(
                "record_merge_op_space requires at least one resolved issue".into(),
            ));
        }

        // 1. Build the merge commit. One `Jjf-Op: merge` per issue.
        let summary = if report.issues.len() == 1 {
            format!("jjf: issue {} - merge", report.issues[0].id)
        } else {
            format!("jjf: merge {} issues", report.issues.len())
        };
        let ops: Vec<Op> = report
            .issues
            .iter()
            .map(|b| Op::Merge {
                issue_id: b.id.clone(),
            })
            .collect();
        let jjf_at = now_rfc3339_nanos()?;
        let msg = build_commit_message(&summary, &ops, &jjf_at);

        let mut argv: Vec<&str> = vec!["new"];
        for h in heads {
            argv.push(h.as_str());
        }
        argv.push("-m");
        argv.push(&msg);
        self.repo.run(&argv)?;

        // 2. Write resolved bytes — record + comments — for every issue.
        let wc_root = self.repo.root();
        for merged in &report.issues {
            let json_path = wc_root.join(issue_json_relpath(&merged.id));
            if let Some(parent) = json_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_record_json(&json_path, &merged.record)?;
            let comments_path = wc_root.join(issue_comments_relpath(&merged.id));
            write_comments_jsonl(&comments_path, &merged.comments)?;
        }

        // 3. Pin the bookmark at the merge commit.
        self.repo.run(&[
            "bookmark",
            "set",
            ISSUES_BOOKMARK,
            "-r",
            "@",
            "--allow-backwards",
        ])?;

        // 4. Step `@` off the bookmark.
        self.repo.run(&["new", "root()"])?;

        // Drop the in-process snapshot memo — the bookmark moved.
        self.invalidate_snapshot_memo();

        Ok(())
    }

    /// Push every issue and memory ref to `remote` via standard git
    /// transport (`git push <remote> 'refs/jjf/issues/*:refs/jjf/issues/*'
    /// 'refs/jjf/memories/*:refs/jjf/memories/*'`).
    ///
    /// The push refspec deliberately excludes `refs/jjf/meta/*`. The
    /// `format-version` sentinel is a per-clone presence flag; pushing
    /// it would non-fast-forward whenever two peers each ran
    /// `jjf init` (see ticket `95fb2d6` for the design call). The
    /// remote acquires its sentinel from whoever ran `jjf init` against
    /// it first. Server-side config is vanilla git — Forgejo / Gitea /
    /// GitLab / GitHub all accept this.
    ///
    /// Returns a tally of refs submitted; the actual per-ref disposition
    /// (created / fast-forwarded / no-op) is opaque from this side.
    /// Failures bubble up as `Error::Git` with verbatim stderr — the
    /// CLI's typed-error classifier ([`crate::sync_v3::PushReportV3`]
    /// docstring) sorts auth / network / non-fast-forward signals into
    /// distinct surface errors.
    ///
    /// **v3-only.** Calling this on a v2-mode Storage returns
    /// `Error::Invalid` — v2 push goes through the bookmark transport
    /// in `crates/jjf/src/main.rs::run_push`. Mode is locked at
    /// `Storage::open` time; a v2 → v3 migration runs on open if the
    /// env-var opt-out isn't set, so under normal operator use mode is
    /// always V3 here.
    pub fn push_v3(&self, remote: &str) -> Result<PushReportV3> {
        if self.mode != StorageMode::V3 {
            return Err(Error::Invalid(
                "push_v3 requires v3-shape storage; this repo is v2"
                    .to_owned(),
            ));
        }
        // Push doesn't mutate local refs, so the cache key is
        // unchanged. The memo doesn't need to drop.
        sync_v3::push_v3(&self.git, remote)
    }

    /// Pull every `refs/jjf/*` ref from `remote`. Runs
    /// `git fetch <remote> 'refs/jjf/*:refs/remotes/<remote>/jjf/*'`,
    /// then per-remote-tracking-ref reconciles with the corresponding
    /// local `refs/jjf/<rest>` ref using git-bug's five-scenario merge
    /// algorithm:
    ///
    /// 1. New (remote-only) → copy.
    /// 2. Identical → no-op.
    /// 3. Local ahead → no-op.
    /// 4. Fast-forward → advance local ref.
    /// 5. Diverged → land a 2-parent merge commit on the local ref
    ///    whose tree is the LWW-resolved snapshot and whose message
    ///    carries a `Jjf-Op: merge` trailer.
    ///
    /// Returns per-scenario counts so the CLI can emit a meaningful
    /// summary in both plain-text and `--json` shapes. The post-pull
    /// state is observable by re-reading any issue via `Storage::show`;
    /// the read path's `cat-file blob` reads the new tip's tree, which
    /// for a merge IS the resolved snapshot.
    ///
    /// **No content-level conflict markers.** The DAG IS the merge; the
    /// LWW resolver replays both sides' op chains in spec §6 order and
    /// writes the merged record into the merge commit's tree. There is
    /// no human-surface "unmergeable" failure mode in this path.
    ///
    /// **v3-only.** See [`Storage::push_v3`] for the V2 fallback note.
    pub fn pull_v3(&self, remote: &str) -> Result<PullReportV3> {
        if self.mode != StorageMode::V3 {
            return Err(Error::Invalid(
                "pull_v3 requires v3-shape storage; this repo is v2"
                    .to_owned(),
            ));
        }
        // Pull mutates local refs (fast-forward, merge, copy-on-new),
        // so the cache must drop its in-process memo — the next read
        // re-probes the ref-set and rebuilds against the new tips.
        let report = sync_v3::pull_v3(&self.git, remote)?;
        self.invalidate_snapshot_memo();
        Ok(report)
    }

    /// Read the per-issue op chain rooted at an explicit revision
    /// rather than the bookmark tip. The default
    /// [`Storage::read_history`] is this with `rev = "bookmarks(issues)"`.
    ///
    /// Used by the op-space merge driver to walk each head of a
    /// divergent `issues` bookmark independently: pass each entry of
    /// [`Storage::issues_heads`] as `rev` to get that head's full op
    /// chain in isolation. The returned [`HistoryEntry`] vector is in
    /// chronological commit order (oldest first) — the LWW reducer
    /// sorts by the spec §6 ordering tuple `(jjf_at_or_commit_time,
    /// commit, trailer_index)` itself.
    ///
    /// `rev` can be any revset jj accepts (typically a change_id short
    /// from `issues_heads()`, but `bookmarks(issues)`, a commit_id
    /// short, or any other shape works). Errors with `IssueNotFound`
    /// if no commit reachable from `rev` touches this issue's files.
    pub fn read_history_at(
        &self,
        rev: &str,
        id: &IssueId,
    ) -> Result<Vec<HistoryEntry>> {
        history::read_history_at(&self.repo, rev, id)
    }

    // ---- internals ---------------------------------------------------

    /// Common path for mutate-the-JSON-record ops. Reads the current
    /// record from the bookmark tip, hands it to `f` to inspect-and-
    /// optionally-mutate, and writes the result back inside one commit.
    ///
    /// The closure returns a [`MutateOutcome`]:
    ///
    /// - `Write(ops)` — the closure mutated the record; persist with
    ///   these ops. `updated_at` is bumped just before the commit.
    /// - `Skip` — the fresh record already satisfies the caller's
    ///   intent (idempotent no-op); no commit lands, return `Ok(())`.
    /// - `Conflict(err)` — the fresh record violates the caller's
    ///   precondition; surface the typed error.
    ///
    /// The tri-state contract exists because the closure is the only
    /// place with the freshly-read record in scope. On a CAS-loss
    /// retry, `mutate` calls the closure AGAIN against the new state
    /// — so any precondition check (e.g. "is the issue still Open?"
    /// for claim) must live INSIDE the closure to run on every
    /// attempt. Pre-`a6b8fb7`, preconditions were checked once before
    /// `mutate` was entered, and the retry would re-apply the
    /// mutation against a post-race record that no longer satisfied
    /// the precondition — silently landing a duplicate write. The
    /// `Mutate` enum surface is the fix; see `a6b8fb7`.
    ///
    /// We read from the bookmark (via `jj file show -r bookmarks(issues)`)
    /// rather than from the working copy because step 4 of the dance
    /// (`jj new root()`) leaves the working copy on a fresh empty
    /// change with no issue files in it. The authoritative state lives
    /// at the bookmark.
    fn mutate<F>(&self, id: &IssueId, summary: &str, f: F) -> Result<bool>
    where
        F: Fn(&mut IssueRecord) -> MutateOutcome,
    {
        // Run the read + mutate + commit cycle. On a typed
        // ConcurrentWrite, re-read the record from the (now-updated)
        // bookmark and re-evaluate the closure against it (the
        // closure-as-precondition contract from `a6b8fb7`).
        //
        // Retry budget is governed by [`RetryPolicy::from_env`] —
        // defaults to 5 retries with geometric backoff so per-issue
        // contention (N concurrent comments / mutations on the same
        // id) is absorbed silently.
        //
        // Return value is `true` when a commit landed, `false` when
        // the closure returned [`MutateOutcome::Skip`] — callers that
        // need to distinguish "wrote" from "no-op'd" (notably
        // `Storage::claim`, to surface race-lost vs. fresh-claim to
        // the `ready --claim` CLI path) inspect this; callers that
        // don't can ignore it.
        let policy = RetryPolicy::from_env();
        run_with_retry(
            policy,
            || self.mutate_once(id, summary, &f),
            || self.invalidate_snapshot_memo(),
        )
    }

    /// Single attempt of [`Storage::mutate`]: read the record from the
    /// bookmark, run the user's closure against it, and either commit
    /// the resulting changes ([`MutateOutcome::Write`]) or short-
    /// circuit ([`MutateOutcome::Skip`] / [`MutateOutcome::Conflict`]).
    ///
    /// Returns a typed [`Error::ConcurrentWrite`] if the commit dance
    /// races. Doesn't retry — the caller (`mutate`) wraps with retry
    /// logic that re-reads state on each attempt. The closure is
    /// re-evaluated on the retry's fresh record (this is the
    /// re-precondition contract that fixes `a6b8fb7`).
    fn mutate_once<F>(&self, id: &IssueId, summary: &str, f: &F) -> Result<bool>
    where
        F: Fn(&mut IssueRecord) -> MutateOutcome,
    {
        let mut record = self.read_record_from_bookmark(id)?;
        let ops = match f(&mut record) {
            MutateOutcome::Write(ops) => ops,
            MutateOutcome::Skip => return Ok(false),
            MutateOutcome::Conflict(err) => return Err(err),
        };
        record.updated_at = now_rfc3339()?;
        if self.mode == StorageMode::V3 {
            // V3: build the new tree from the mutated record. The
            // existing comments stream stays in the tree byte-for-
            // byte; we re-read it and pass it back so the new commit
            // carries the same `comments.jsonl` blob as the previous
            // tip. Re-reading vs. structurally re-pointing matters
            // because git computes the tree oid from the blob set —
            // and a tree that "preserves" comments must literally
            // re-list the blob.
            let existing_comments = self.read_comments_from_bookmark(id)?;
            let comments: Option<&[Comment]> = if existing_comments.is_empty() {
                None
            } else {
                Some(existing_comments.as_slice())
            };
            self.commit_record_v3(id, &record, comments, summary, &ops)?;
            return Ok(true);
        }
        let id_clone = id.clone();
        self.commit_record_change(summary, &ops, |wc_root| {
            write_record_json(&wc_root.join(issue_json_relpath(&id_clone)), &record)?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Read the current record for `id` from authoritative storage.
    ///
    /// In V2 mode this reads `issues/<id>.json` at the `issues`
    /// bookmark tip. In V3 mode this reads `issue.json` from the tip
    /// commit's tree on `refs/jjf/issues/<id>`. Either way, returns
    /// [`Error::IssueNotFound`] if the issue doesn't exist.
    fn read_record_from_bookmark(&self, id: &IssueId) -> Result<IssueRecord> {
        if self.mode == StorageMode::V3 {
            return v3_write::read_record_v3(&self.git, id);
        }
        let relpath = issue_json_relpath(id);
        let text = match self.repo.run(&[
            "file",
            "show",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            &format!("root:{}", relpath.display()),
        ]) {
            Ok(s) => s,
            Err(_) => {
                // jj returns non-zero if the path doesn't exist at that
                // revision. Treat that as issue-not-found rather than
                // surfacing the raw jj error — callers expect a typed
                // signal.
                return Err(Error::IssueNotFound(id.clone()));
            }
        };
        Ok(serde_json::from_str(&text)?)
    }

    /// Read the current comments stream for `id` from authoritative
    /// storage. Returns an empty vec if no comments file is present
    /// (the writer creates an empty file at issue-create time in V2;
    /// in V3, the file is only present once the first comment lands).
    fn read_comments_from_bookmark(&self, id: &IssueId) -> Result<Vec<Comment>> {
        if self.mode == StorageMode::V3 {
            return v3_write::read_comments_v3(&self.git, id);
        }
        let relpath = issue_comments_relpath(id);
        let text = match self.repo.run(&[
            "file",
            "show",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            &format!("root:{}", relpath.display()),
        ]) {
            Ok(s) => s,
            Err(_) => {
                // Missing comments file => no comments. The record's
                // existence is the source of truth on whether the issue
                // exists; callers should check that first.
                return Ok(Vec::new());
            }
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            out.push(serde_json::from_str(line)?);
        }
        Ok(out)
    }

    /// Run the 4-CLI dance once. `summary` is the human-readable first
    /// line of the commit message; `ops` becomes the `Jjf-Op:` trailer
    /// stanza; `apply` is the closure that mutates files inside the
    /// working copy (relative to `wc_root`, which is the repo root).
    ///
    /// V3 counterpart to [`Storage::commit_record_change`].
    ///
    /// Builds the trailer block, lands a commit on
    /// `refs/jjf/issues/<id>` via [`v3_write::commit_record_v3`], and
    /// runs the same post-write bookkeeping (snapshot memo
    /// invalidation, concurrent-write translation). Zero `jj` calls.
    ///
    /// The CAS failure inside `v3_write::commit_record_v3` is
    /// already translated to [`Error::ConcurrentWrite`], so the
    /// retry policy in [`Storage::mutate`] / [`Storage::add_comment`]
    /// / [`Storage::create_issue`] recognizes it unchanged.
    fn commit_record_v3(
        &self,
        id: &IssueId,
        record: &IssueRecord,
        comments: Option<&[Comment]>,
        summary: &str,
        ops: &[Op],
    ) -> Result<()> {
        // Stamp every op stanza in this commit with the same nano-
        // precision op-time (spec §5). Same shape as the v2 path.
        let jjf_at = now_rfc3339_nanos()?;
        let msg = build_commit_message(summary, ops, &jjf_at);
        let result = v3_write::commit_record_v3(&self.git, id, record, comments, &msg);
        // Invalidate the snapshot memo on both success and failure —
        // the v3 read path lands in a later ticket, but the memo
        // pattern matches v2 (lib.rs's `commit_record_change`): every
        // mutation drops it so the next read re-probes. The v2-shape
        // snapshot cache only loads on V2-mode reads (`snapshot()`
        // hits the `issues` bookmark), so this invalidation is a no-
        // op on a v3-mode repo today; it's symmetric for clarity.
        self.invalidate_snapshot_memo();
        result.map(|_| ())
    }

    /// V3 counterpart to [`Storage::commit_memory_change`]. Same
    /// shape as [`Storage::commit_record_v3`] but routes to
    /// [`v3_write::commit_memory_v3`] (the per-memory ref namespace).
    ///
    /// `memory = None` is the unset case: the tree is empty, the
    /// commit's trailer carries `Jjf-Op: unset-memory`.
    fn commit_memory_v3(
        &self,
        key: &str,
        memory: Option<&Memory>,
        msg: &str,
    ) -> Result<()> {
        let result = v3_write::commit_memory_v3(&self.git, key, memory, msg);
        self.invalidate_snapshot_memo();
        result.map(|_| ())
    }

    /// On any jj failure whose stderr matches the concurrent-write
    /// fingerprint, this surfaces [`Error::ConcurrentWrite`] with a
    /// hint string. The caller is responsible for the retry decision
    /// — most mutations re-read state on retry (so a stale snapshot
    /// doesn't clobber a concurrent writer's landed work), which has
    /// to happen in the higher-level mutator (`mutate`, `add_comment`,
    /// `create_issue`), not here.
    ///
    /// On non-conflict failures the underlying [`Error`] surfaces
    /// unchanged.
    ///
    /// In either failure case the snapshot memo is invalidated so a
    /// subsequent retry / probe sees fresh state.
    fn commit_record_change<F>(
        &self,
        summary: &str,
        ops: &[Op],
        apply: F,
    ) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        // Stamp every op stanza in this commit with the same nano-
        // precision op-time (spec §5: `Jjf-At:` per stanza). The
        // record-level `created_at`/`updated_at` use second resolution
        // and are stamped separately by the per-verb mutators; only the
        // trailer carries nanos.
        let jjf_at = now_rfc3339_nanos()?;
        let msg = build_commit_message(summary, ops, &jjf_at);

        match self.try_commit_dance(&msg, apply) {
            Ok(()) => {
                self.invalidate_snapshot_memo();
                Ok(())
            }
            Err(e) if is_concurrent_write(&e) => {
                self.invalidate_snapshot_memo();
                Err(Error::ConcurrentWrite {
                    hint: "another writer landed first. Retry your command.".into(),
                })
            }
            Err(e) => {
                self.invalidate_snapshot_memo();
                Err(e)
            }
        }
    }

    /// Inner helper for `commit_record_change`: runs the 4-CLI dance
    /// once with an `apply` closure that takes a `&Path`. Returns the
    /// raw `Result<(), Error>` so the caller can decide on retry vs.
    /// translation. Doesn't invalidate the snapshot memo — the caller
    /// is responsible (so failure paths can re-probe consistent
    /// state).
    fn try_commit_dance<F>(&self, msg: &str, apply: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        // 1. jj new bookmarks(issues) -m '<msg>'
        self.repo.run(&["new", ISSUES_BOOKMARK_REVSET, "-m", msg])?;

        // 2. Edit the working copy. jj snapshots on the next command.
        apply(self.repo.root())?;

        // 3. jj bookmark set issues -r @ --allow-backwards
        self.repo.run(&[
            "bookmark",
            "set",
            ISSUES_BOOKMARK,
            "-r",
            "@",
            "--allow-backwards",
        ])?;

        // 4. jj new root() — step @ off the bookmark.
        self.repo.run(&["new", "root()"])?;

        Ok(())
    }

    /// Run the 4-CLI dance for a memory mutation. Memory commits carry
    /// a single `Jjf-Op: set-memory` or `unset-memory` trailer (no
    /// `Jjf-Issue:`), so we build the message directly rather than via
    /// [`build_commit_message`] (which assumes per-issue ops).
    ///
    /// Concurrent-write failures are translated to typed
    /// [`Error::ConcurrentWrite`] for clean operator UX (rather than
    /// the raw jj-internal vomit). Memory mutations are scalar LWW
    /// writes per spec — the resolver will pick whichever stamp lands
    /// later — so no in-storage retry is necessary; an operator that
    /// races their own `jjf remember` can retry the command. Out of
    /// scope for the v1 auto-retry policy per
    /// `qa-concurrent-write-ux`.
    fn commit_memory_change<F>(&self, msg: &str, apply: F) -> Result<()>
    where
        F: FnOnce(&Path) -> Result<()>,
    {
        match self.try_commit_dance(msg, apply) {
            Ok(()) => {
                self.invalidate_snapshot_memo();
                Ok(())
            }
            Err(e) if is_concurrent_write(&e) => {
                self.invalidate_snapshot_memo();
                Err(Error::ConcurrentWrite {
                    hint: "another writer landed first. Retry your command.".into(),
                })
            }
            Err(e) => {
                self.invalidate_snapshot_memo();
                Err(e)
            }
        }
    }

    /// Read a single memory by key from authoritative storage.
    /// Returns `Ok(None)` if the key doesn't exist (V2: no
    /// `memories/<key>.json`; V3: no `refs/jjf/memories/<key>`, or
    /// the ref's tip carries an empty tree from an `unset` op).
    fn read_memory_from_bookmark(&self, key: &str) -> Result<Option<Memory>> {
        if self.mode == StorageMode::V3 {
            return v3_write::read_memory_v3(&self.git, key);
        }
        let relpath = memory::memory_json_relpath(key);
        let text = match self.repo.run(&[
            "file",
            "show",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            &format!("root:{}", relpath.display()),
        ]) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        Ok(Some(serde_json::from_str(&text)?))
    }

    /// Enumerate every memory key present in `memories/<key>.json` at
    /// the `issues` bookmark tip. Sorted ascending, deduplicated.
    ///
    /// Kept around as a typed primitive even though `list_memories`
    /// now uses the snapshot cache — internal callers that only
    /// need keys can still use this without paying the full
    /// per-record parse.
    #[allow(dead_code)]
    fn list_memory_keys(&self) -> Result<Vec<String>> {
        let text = match self.repo.run(&[
            "file",
            "list",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            "-T",
            "path ++ \"\\n\"",
            "root:memories/",
        ]) {
            Ok(s) => s,
            // No memories directory yet — empty list.
            Err(_) => return Ok(Vec::new()),
        };
        let mut keys: Vec<String> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("memories/") else {
                continue;
            };
            let Some(stem) = rest.strip_suffix(".json") else {
                continue;
            };
            // Defensive: reject any path with directory separators.
            if stem.contains('/') {
                continue;
            }
            keys.push(stem.to_owned());
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Does this issue id already have a record in authoritative
    /// storage? Used for the collision retry in `create_issue` and
    /// for phantom-dep validation at draft time.
    fn issue_exists_on_bookmark(&self, id: &IssueId) -> Result<bool> {
        if self.mode == StorageMode::V3 {
            return v3_write::issue_exists_v3(&self.git, id);
        }
        let relpath = issue_json_relpath(id);
        // `jj file show` exits non-zero if the path is absent at the
        // requested revision. We don't distinguish "missing file" from
        // "jj broke"; the latter is vanishingly unlikely here and the
        // next jj call in `commit_record_change` will surface it.
        Ok(self
            .repo
            .run(&[
                "file",
                "show",
                "-r",
                ISSUES_BOOKMARK_REVSET,
                &format!("root:{}", relpath.display()),
            ])
            .is_ok())
    }
}

// ---- record I/O ------------------------------------------------------

/// Relative path of an issue's JSON record from repo root.
pub(crate) fn issue_json_relpath(id: &IssueId) -> PathBuf {
    PathBuf::from("issues").join(format!("{}.json", id))
}

/// Relative path of an issue's comments file from repo root.
pub(crate) fn issue_comments_relpath(id: &IssueId) -> PathBuf {
    PathBuf::from("issues").join(format!("{}.comments.jsonl", id))
}

/// Pre-migration v1 path of an issue's JSON record. The migration
/// commit (`Storage::maybe_migrate_v1_to_v2`) renames `bugs/<id>.*`
/// to `issues/<id>.*` on a single commit, but commits *prior* to
/// the migration touched the v1 paths. The history walker and read
/// replay query include BOTH paths in their `jj log` filter so they
/// don't drop pre-migration ops out of the per-issue chain.
pub(crate) fn v1_issue_json_relpath(id: &IssueId) -> PathBuf {
    PathBuf::from("bugs").join(format!("{}.json", id))
}

/// Pre-migration v1 path of an issue's comments file. See
/// [`v1_issue_json_relpath`] for why both paths are needed.
pub(crate) fn v1_issue_comments_relpath(id: &IssueId) -> PathBuf {
    PathBuf::from("bugs").join(format!("{}.comments.jsonl", id))
}

fn write_record_json(path: &Path, record: &IssueRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(record)?;
    s.push('\n');
    std::fs::write(path, s)?;
    Ok(())
}

fn write_memory_json(path: &Path, mem: &Memory) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(mem)?;
    s.push('\n');
    std::fs::write(path, s)?;
    Ok(())
}

fn write_comments_jsonl(path: &Path, comments: &[Comment]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = String::new();
    for c in comments {
        s.push_str(&serde_json::to_string(c)?);
        s.push('\n');
    }
    std::fs::write(path, s)?;
    Ok(())
}

// ---- commit message --------------------------------------------------

/// Build the full commit message: one-line summary, blank line, then
/// the trailer stanza per `docs/storage-format.md` §5.
///
/// Every op stanza is stamped with the same `jjf_at` (the writer's
/// `now_rfc3339_nanos()` at the moment of the call). The single-stamp
/// shape matches the multi-op-per-commit semantics: ops in one commit
/// were intended together and are ordered by trailer index within the
/// commit, so they share an op-time and rely on the `(jjf_at,
/// commit_hash, trailer_index)` tuple for total order in op-space
/// replay.
pub(crate) fn build_commit_message(summary: &str, ops: &[Op], jjf_at: &str) -> String {
    let mut s = String::new();
    s.push_str(summary);
    s.push_str("\n\n");
    for op in ops.iter() {
        // No blank line between op stanzas — they're one continuous
        // trailer block per spec §5.5.
        s.push_str(&op.to_trailer_block(jjf_at));
    }
    s
}

// ---- helpers ---------------------------------------------------------

fn sorted_dedup(xs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = xs.to_vec();
    v.sort();
    v.dedup();
    v
}

/// Sort + dedup a slice of typed dep edges by `(target, kind)`.
/// Mirrors [`sorted_dedup`]'s contract for the v2.4 edge shape.
fn sorted_dedup_edges(xs: &[DepEdge]) -> Vec<DepEdge> {
    let mut v: Vec<DepEdge> = xs.to_vec();
    v.sort();
    v.dedup();
    v
}

/// Crate-internal alias so `read.rs` can reuse the same hash function
/// without duplicating the inline implementation.
#[cfg(debug_assertions)]
pub(crate) fn sha256_hex_for_read(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

/// Crate-internal helper for the op-space merge driver: hash a body
/// string to its hex sha-256 so a winning `Op::SetBody { body_hash }`
/// can be matched against each head's rendered record. Mirrors the
/// same hash the writer computes at `set_body` time.
pub(crate) fn body_hash_hex(body: &str) -> String {
    sha256_hex(body.as_bytes())
}

/// Hex sha-256 for `set-body` trailers (`Jjf-Body-Hash` per spec §5.2).
/// We need this but don't want a `sha2` dep for one site — we lift
/// the public-domain reference implementation inline. Throughput
/// doesn't matter; a body hash is hashed once per commit.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha256::sha256(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(hex_nybble(b >> 4));
        out.push(hex_nybble(b & 0xf));
    }
    out
}

fn hex_nybble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'a' + (n - 10)) as char,
        _ => unreachable!(),
    }
}

/// Current time as RFC 3339 in UTC, second resolution. We avoid
/// pulling `chrono` / `time` just to render the timestamps the spec
/// asks for; format is well-known and the math is small.
///
/// Tests may pin the clock by setting `JJF_TEST_CLOCK_SECS` to a
/// fixed `u64` epoch-seconds value (e.g. `1735660800`). The override
/// affects both this function and [`now_rfc3339_nanos`], which derives
/// its seconds from the same source. Production code never sets this
/// env var; the override exists so timing-sensitive tests (like
/// `read_history_walks_same_second_comment_appends`, which depends on
/// two consecutive writes landing in the same wall-clock second) are
/// deterministic under heavy parallel test load.
fn now_rfc3339() -> Result<String> {
    Ok(epoch_secs_to_rfc3339(current_epoch_secs()?))
}

fn current_epoch_secs() -> Result<u64> {
    if let Ok(v) = std::env::var("JJF_TEST_CLOCK_SECS") {
        if let Ok(n) = v.parse::<u64>() {
            return Ok(n);
        }
    }
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Clock(format!("system clock before unix epoch: {e}")))?;
    Ok(dur.as_secs())
}

/// Current time as RFC 3339 in UTC with nanosecond resolution. Used by
/// the trailer writer so each op stanza carries a `Jjf-At:` field with
/// enough resolution that two ops on the same second (a real failure
/// mode at jj 0.40's per-second commit-time granularity) sort
/// deterministically. The JSON record's `created_at`/`updated_at`
/// continue to use [`now_rfc3339`] per spec §3.1 — only trailers get
/// nanos.
pub(crate) fn now_rfc3339_nanos() -> Result<String> {
    // When `JJF_TEST_CLOCK_SECS` is set, nanos resolve to live
    // sub-second so trailer ordering still works; only the second
    // component is pinned.
    if std::env::var_os("JJF_TEST_CLOCK_SECS").is_some() {
        let secs = current_epoch_secs()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        return Ok(epoch_nanos_to_rfc3339(secs, nanos));
    }
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Clock(format!("system clock before unix epoch: {e}")))?;
    Ok(epoch_nanos_to_rfc3339(dur.as_secs(), dur.subsec_nanos()))
}

/// Format `(secs_since_epoch, nanos)` as
/// `YYYY-MM-DDTHH:MM:SS.fffffffffZ` — RFC 3339 with nine fractional
/// digits. Shares the civil-from-days math with
/// [`epoch_secs_to_rfc3339`]; only the fractional-seconds suffix
/// differs.
pub(crate) fn epoch_nanos_to_rfc3339(secs: u64, nanos: u32) -> String {
    let base = epoch_secs_to_rfc3339(secs);
    // base is `YYYY-MM-DDTHH:MM:SSZ`; insert `.fffffffff` before the
    // trailing `Z` to land on `YYYY-MM-DDTHH:MM:SS.fffffffffZ`.
    let trunk = &base[..base.len() - 1];
    format!("{trunk}.{nanos:09}Z")
}

/// Format seconds-since-epoch as `YYYY-MM-DDTHH:MM:SSZ`. UTC only,
/// no fractional seconds. Handles years from 1970 onward via the
/// civil-from-days algorithm by Howard Hinnant (public domain).
pub(crate) fn epoch_secs_to_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = (secs % 86_400) as u32;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hour, minute, second
    )
}

/// Parse a strict-shape RFC 3339 stamp (`YYYY-MM-DDTHH:MM:SSZ`,
/// UTC, second resolution — exactly what [`epoch_secs_to_rfc3339`]
/// emits) back to seconds-since-epoch. Returns `None` if the input
/// doesn't match the expected shape or any field is out of range.
///
/// Used by [`Storage::stale`] to compute `now - updated_at`. We do
/// not bring in `chrono` / `time` for this; the on-disk stamps are
/// strict and emitted by us, so the parse stays small.
fn rfc3339_to_epoch_secs(s: &str) -> Option<u64> {
    // `YYYY-MM-DDTHH:MM:SSZ` is 20 bytes. Defensive length check.
    let b = s.as_bytes();
    if b.len() != 20 {
        return None;
    }
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T'
        || b[13] != b':' || b[16] != b':' || b[19] != b'Z'
    {
        return None;
    }
    let parse_u = |start: usize, end: usize| -> Option<u32> {
        s.get(start..end).and_then(|t| t.parse::<u32>().ok())
    };
    let year = parse_u(0, 4)? as i32;
    let month = parse_u(5, 7)?;
    let day = parse_u(8, 10)?;
    let hour = parse_u(11, 13)?;
    let minute = parse_u(14, 16)?;
    let second = parse_u(17, 19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        // Pre-1970 stamps don't fit `u64` epoch-seconds; reject.
        return None;
    }
    let secs = (days as u64) * 86_400
        + (hour as u64) * 3600
        + (minute as u64) * 60
        + (second as u64);
    Some(secs)
}

/// Howard Hinnant's `days_from_civil` (public domain). The inverse
/// of [`civil_from_days`]: returns days since 1970-01-01 for the
/// given UTC civil date. Negative when the input predates the epoch.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { (y as i64) - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64; // [0, 399]
    let m = m as u64;
    let d = d as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146_096]
    era * 146_097 + (doe as i64) - 719_468
}

/// Howard Hinnant's `civil_from_days` (public domain). `z` is days
/// since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146_096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ---- tiny inline sha-256 --------------------------------------------
//
// We include a minimal sha-256 because we use it for exactly one
// thing (the `Jjf-Body-Hash` trailer). The body-hash exists to make
// `set-body` trailers self-describing without inflating the trailer.
// Tested in `mod tests`.
mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
        0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
        0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub fn sha256(input: &[u8]) -> [u8; 32] {
        let mut h: [u32; 8] = [
            0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c,
            0x1f83d9ab, 0x5be0cd19,
        ];
        let bit_len = (input.len() as u64) * 8;
        let mut buf: Vec<u8> = Vec::with_capacity(input.len() + 72);
        buf.extend_from_slice(input);
        buf.push(0x80);
        while buf.len() % 64 != 56 {
            buf.push(0);
        }
        buf.extend_from_slice(&bit_len.to_be_bytes());
        for chunk in buf.chunks_exact(64) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([
                    chunk[i * 4],
                    chunk[i * 4 + 1],
                    chunk[i * 4 + 2],
                    chunk[i * 4 + 3],
                ]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7)
                    ^ w[i - 15].rotate_right(18)
                    ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17)
                    ^ w[i - 2].rotate_right(19)
                    ^ (w[i - 2] >> 10);
                w[i] = w[i - 16]
                    .wrapping_add(s0)
                    .wrapping_add(w[i - 7])
                    .wrapping_add(s1);
            }
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
                (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ ((!e) & g);
                let t1 = hh
                    .wrapping_add(s1)
                    .wrapping_add(ch)
                    .wrapping_add(K[i])
                    .wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let mj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(mj);
                hh = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
            h[5] = h[5].wrapping_add(f);
            h[6] = h[6].wrapping_add(g);
            h[7] = h[7].wrapping_add(hh);
        }
        let mut out = [0u8; 32];
        for (i, word) in h.iter().enumerate() {
            out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip_canonical() {
        let rec = IssueRecord {
            version: 2,
            id: IssueId::parse("aa6600b").unwrap(),
            title: "segfault on empty input".into(),
            slug: Some("segfault-on-empty-input".into()),
            body: "Running `./app` with no arguments crashes.".into(),
            status: Status::Open,
            block_reason: None,
            type_: IssueType::Bug,
            labels: vec!["bug".into(), "p1".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
            created_at: "2026-06-21T12:00:00Z".into(),
            updated_at: "2026-06-21T15:34:48Z".into(),
        };
        let s = serde_json::to_string_pretty(&rec).unwrap();
        let back: IssueRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
        // Field-ordering check: spec §3.1 / v2.1 — fields appear in
        // schema order so jj's textual auto-merger has stable diffs.
        let v_idx = s.find("\"version\"").unwrap();
        let id_idx = s.find("\"id\"").unwrap();
        let title_idx = s.find("\"title\"").unwrap();
        let slug_idx = s.find("\"slug\"").unwrap();
        let body_idx = s.find("\"body\"").unwrap();
        let status_idx = s.find("\"status\"").unwrap();
        let type_idx = s.find("\"type\"").unwrap();
        let labels_idx = s.find("\"labels\"").unwrap();
        let deps_idx = s.find("\"dependencies\"").unwrap();
        let assignee_idx = s.find("\"assignee\"").unwrap();
        let created_idx = s.find("\"created_at\"").unwrap();
        let updated_idx = s.find("\"updated_at\"").unwrap();
        assert!(v_idx < id_idx);
        assert!(id_idx < title_idx);
        assert!(title_idx < slug_idx);
        assert!(slug_idx < body_idx);
        assert!(body_idx < status_idx);
        assert!(status_idx < type_idx);
        assert!(type_idx < labels_idx);
        assert!(labels_idx < deps_idx);
        assert!(deps_idx < assignee_idx);
        assert!(assignee_idx < created_idx);
        assert!(created_idx < updated_idx);
    }

    #[test]
    fn op_roundtrip_serde() {
        let ops = [
            Op::Create {
                issue_id: IssueId::parse("aa6600b").unwrap(),
                title: "t".into(),
                status: Status::Open,
            },
            Op::SetStatus {
                issue_id: IssueId::parse("aa6600b").unwrap(),
                status: Status::Closed,
            },
            Op::LabelAdd {
                issue_id: IssueId::parse("aa6600b").unwrap(),
                label: "fixed".into(),
            },
            Op::Merge {
                issue_id: IssueId::parse("aa6600b").unwrap(),
            },
        ];
        for op in &ops {
            let s = serde_json::to_string(op).unwrap();
            let back: Op = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, op);
        }
    }

    #[test]
    fn op_serde_emits_v2_issue_id_field() {
        // Spec §5.2 (v2): the JSON shape of an op uses `issue_id`. v1
        // emitted `bug_id` — that shape is gone on the new write path
        // (the trailer parser still reads either, see trailer.rs).
        let op = Op::SetStatus {
            issue_id: IssueId::parse("aa6600b").unwrap(),
            status: Status::Closed,
        };
        let s = serde_json::to_string(&op).unwrap();
        assert!(
            s.contains("\"issue_id\":\"aa6600b\""),
            "op serde should emit `issue_id`, got: {s}"
        );
        assert!(
            !s.contains("\"bug_id\""),
            "op serde must NOT emit legacy `bug_id`, got: {s}"
        );
    }

    #[test]
    fn trailer_format_single_op_create_matches_spec() {
        // §5.4 example, extended with the §5-mandated `Jjf-At:` line.
        let op = Op::Create {
            issue_id: IssueId::parse("aa6600b").unwrap(),
            title: "segfault on empty input".into(),
            status: Status::Open,
        };
        let msg = build_commit_message(
            "jjf: issue aa6600b - create",
            &[op],
            "2026-06-22T12:34:56.123456789Z",
        );
        let expected = "\
jjf: issue aa6600b - create

Jjf-Op: create
Jjf-Issue: aa6600b
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Title: segfault on empty input
Jjf-Status: open
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn trailer_format_multi_op_matches_spec() {
        // §5.5 example minus the free-text body (the build_commit_message
        // helper doesn't synthesize that — callers pass it via summary
        // if they want it). Every stanza in a multi-op commit shares
        // the same `Jjf-At:` — they were issued together.
        let issue = IssueId::parse("aa6600b").unwrap();
        let ops = [
            Op::SetStatus {
                issue_id: issue.clone(),
                status: Status::Closed,
            },
            Op::LabelAdd {
                issue_id: issue.clone(),
                label: "fixed".into(),
            },
        ];
        let msg = build_commit_message(
            "jjf: issue aa6600b - close + label",
            &ops,
            "2026-06-22T12:34:56.123456789Z",
        );
        let expected = "\
jjf: issue aa6600b - close + label

Jjf-Op: set-status
Jjf-Issue: aa6600b
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Status: closed
Jjf-Op: label-add
Jjf-Issue: aa6600b
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Label: fixed
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn rfc3339_nanos_format_matches_spec_shape() {
        // 2026-06-22T12:00:00.000000000Z = secs=1_782_129_600 nanos=0.
        let s = epoch_nanos_to_rfc3339(1_782_129_600, 0);
        assert_eq!(s, "2026-06-22T12:00:00.000000000Z");
        // Non-zero nanos lands in the fractional slot, zero-padded to
        // exactly nine digits (the spec calls this rfc3339-nano).
        let s = epoch_nanos_to_rfc3339(1_782_129_600, 1);
        assert_eq!(s, "2026-06-22T12:00:00.000000001Z");
        let s = epoch_nanos_to_rfc3339(1_782_129_600, 123_456_789);
        assert_eq!(s, "2026-06-22T12:00:00.123456789Z");
    }

    #[test]
    fn id_shape() {
        for _ in 0..1000 {
            let id = IssueId::random();
            assert_eq!(id.as_str().len(), 7);
            assert!(
                id.as_str().chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
                "bad id: {}",
                id
            );
        }
    }

    #[test]
    fn id_parses_lowercase_hex_only() {
        assert!(IssueId::parse("aa6600b").is_ok());
        assert!(IssueId::parse("AA6600B").is_err());
        assert!(IssueId::parse("abcdefg").is_err());
        assert!(IssueId::parse("123").is_err());
        assert!(IssueId::parse("12345678").is_err());
    }

    #[test]
    fn rfc3339_format_matches_spec_example() {
        // Spec §3.2 example "2026-06-21T12:00:00Z" is the formatting
        // shape we need to produce.
        // 2026-06-21T12:00:00Z = 1_782_043_200 (verified via `date -u
        // -j -f "%Y-%m-%dT%H:%M:%SZ"`).
        let s = epoch_secs_to_rfc3339(1_782_043_200);
        assert_eq!(s, "2026-06-21T12:00:00Z");

        // Unix epoch.
        assert_eq!(epoch_secs_to_rfc3339(0), "1970-01-01T00:00:00Z");

        // Leap-year math sanity: 2024-02-29T00:00:00Z = 1709164800.
        assert_eq!(epoch_secs_to_rfc3339(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn sha256_known_vector() {
        // sha256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let s = sha256_hex(b"");
        assert_eq!(
            s,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let s = sha256_hex(b"abc");
        assert_eq!(
            s,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // ---- v2.4 (agent-dep-types) tests ----------------------------------

    /// v1 → v2.4 back-compat: an IssueRecord written by a pre-v2.4
    /// writer carries `dependencies: ["abc1234", "def5678"]` (bare
    /// string ids). The v2.4 reader materializes each string as a
    /// `DepEdge { kind: Blocks }`, preserving the v1 default
    /// semantics transparently. No backfill needed.
    #[test]
    fn v1_dependencies_read_as_blocks_edges() {
        let v1_json = r#"{
            "version": 2,
            "id": "aa6600b",
            "title": "v1 record",
            "slug": null,
            "body": "",
            "status": "open",
            "type": "unspecified",
            "labels": [],
            "dependencies": ["abc1234", "def5678"],
            "assignee": null,
            "created_at": "2026-06-22T12:00:00Z",
            "updated_at": "2026-06-22T12:00:00Z"
        }"#;
        let rec: IssueRecord = serde_json::from_str(v1_json).unwrap();
        assert_eq!(
            rec.dependencies,
            vec![
                DepEdge {
                    target: IssueId::parse("abc1234").unwrap(),
                    kind: DepKind::Blocks,
                },
                DepEdge {
                    target: IssueId::parse("def5678").unwrap(),
                    kind: DepKind::Blocks,
                },
            ],
        );
    }

    /// v2.4 round-trip: a record with mixed-kind edges serializes to
    /// the tagged shape `{"target": "...", "kind": "..."}` and reads
    /// back as the same edges.
    #[test]
    fn v24_tagged_edges_round_trip() {
        let rec = IssueRecord {
            version: 2,
            id: IssueId::parse("aa6600b").unwrap(),
            title: "v2.4 record".into(),
            slug: None,
            body: String::new(),
            status: Status::Open,
            block_reason: None,
            type_: IssueType::Feature,
            labels: vec![],
            dependencies: vec![
                DepEdge {
                    target: IssueId::parse("abc1234").unwrap(),
                    kind: DepKind::Blocks,
                },
                DepEdge {
                    target: IssueId::parse("def5678").unwrap(),
                    kind: DepKind::ParentChild,
                },
                DepEdge {
                    target: IssueId::parse("1111111").unwrap(),
                    kind: DepKind::Related,
                },
                DepEdge {
                    target: IssueId::parse("2222222").unwrap(),
                    kind: DepKind::DiscoveredFrom,
                },
            ],
            assignee: None,
            created_at: "2026-06-22T12:00:00Z".into(),
            updated_at: "2026-06-22T12:00:00Z".into(),
        };
        let s = serde_json::to_string(&rec).unwrap();
        // Spot-check: tagged form is present.
        assert!(s.contains(r#""kind":"parent-child""#), "got: {s}");
        assert!(s.contains(r#""kind":"discovered-from""#), "got: {s}");
        let back: IssueRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
    }

    /// Mixed v1/v2.4 entries in the same array (defensive — a
    /// hand-edited record might mix shapes; the reader should
    /// tolerate). v1 entries become `Blocks`; v2.4 entries keep
    /// their kind.
    #[test]
    fn mixed_v1_and_v24_entries_deserialize() {
        let json = r#"{
            "version": 2,
            "id": "aa6600b",
            "title": "mixed",
            "slug": null,
            "body": "",
            "status": "open",
            "type": "unspecified",
            "labels": [],
            "dependencies": [
                "abc1234",
                {"target": "def5678", "kind": "parent-child"}
            ],
            "assignee": null,
            "created_at": "2026-06-22T12:00:00Z",
            "updated_at": "2026-06-22T12:00:00Z"
        }"#;
        let rec: IssueRecord = serde_json::from_str(json).unwrap();
        assert_eq!(
            rec.dependencies,
            vec![
                DepEdge {
                    target: IssueId::parse("abc1234").unwrap(),
                    kind: DepKind::Blocks,
                },
                DepEdge {
                    target: IssueId::parse("def5678").unwrap(),
                    kind: DepKind::ParentChild,
                },
            ],
        );
    }

    /// `DepKind::parse_wire` round-trips for every variant.
    #[test]
    fn dep_kind_wire_round_trip() {
        for k in [
            DepKind::Blocks,
            DepKind::ParentChild,
            DepKind::Related,
            DepKind::DiscoveredFrom,
        ] {
            assert_eq!(DepKind::parse_wire(k.as_str()), Some(k));
        }
        assert_eq!(DepKind::parse_wire("nope"), None);
    }

    /// Op trailer rendering carries `Jjf-Dep-Kind:` for each of the
    /// four kinds. A `dep-add` op's stanza:
    /// ```text
    /// Jjf-Op: dep-add
    /// Jjf-Issue: <owner>
    /// Jjf-At: <stamp>
    /// Jjf-Dep: <target>
    /// Jjf-Dep-Kind: <kind>
    /// ```
    #[test]
    fn dep_add_trailer_carries_dep_kind() {
        let op = Op::DepAdd {
            issue_id: IssueId::parse("aa6600b").unwrap(),
            dep: IssueId::parse("def5678").unwrap(),
            kind: DepKind::ParentChild,
        };
        let stanza = op.to_trailer_block("2026-06-22T12:00:00.000000000Z");
        assert!(stanza.contains("Jjf-Op: dep-add"), "got: {stanza}");
        assert!(stanza.contains("Jjf-Dep: def5678"), "got: {stanza}");
        assert!(
            stanza.contains("Jjf-Dep-Kind: parent-child"),
            "got: {stanza}"
        );
    }

    // ---- list_ready fixpoint cascade tests ------------------------------

    /// Helper: build an `Issue` projection for the in-memory
    /// `compute_blocked_set` tests. The function only inspects
    /// `id`, `status`, `dependencies`, so we leave the other fields
    /// at default-ish values.
    fn mk_issue(id: &str, status: Status, deps: Vec<DepEdge>) -> Issue {
        Issue {
            id: IssueId::parse(id).unwrap(),
            title: String::new(),
            slug: None,
            body: String::new(),
            status,
            block_reason: None,
            type_: IssueType::Unspecified,
            labels: vec![],
            dependencies: deps,
            assignee: None,
            comments: vec![],
            created_at: "2026-06-22T12:00:00Z".into(),
            updated_at: "2026-06-22T12:00:00Z".into(),
        }
    }

    fn iid(s: &str) -> IssueId {
        IssueId::parse(s).unwrap()
    }

    /// Blocks-edge to an open target blocks the owner.
    #[test]
    fn blocks_edge_to_open_target_blocks_owner() {
        let all = vec![
            mk_issue("aaaaaa1", Status::Open, vec![]),
            mk_issue(
                "bbbbbb1",
                Status::Open,
                vec![DepEdge {
                    target: iid("aaaaaa1"),
                    kind: DepKind::Blocks,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(blocked.contains(&iid("bbbbbb1")));
        assert!(!blocked.contains(&iid("aaaaaa1")));
    }

    /// Blocks-edge to a closed target does NOT block the owner.
    #[test]
    fn blocks_edge_to_closed_target_unblocks_owner() {
        let all = vec![
            mk_issue("aaaaaa2", Status::Closed, vec![]),
            mk_issue(
                "bbbbbb2",
                Status::Open,
                vec![DepEdge {
                    target: iid("aaaaaa2"),
                    kind: DepKind::Blocks,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&iid("bbbbbb2")));
    }

    /// Related and DiscoveredFrom edges to an open target do NOT
    /// block the owner.
    #[test]
    fn related_and_discovered_from_edges_do_not_block() {
        let all = vec![
            mk_issue("aaaaaa3", Status::Open, vec![]),
            mk_issue(
                "bbbbbb3",
                Status::Open,
                vec![DepEdge {
                    target: iid("aaaaaa3"),
                    kind: DepKind::Related,
                }],
            ),
            mk_issue(
                "ccccc03",
                Status::Open,
                vec![DepEdge {
                    target: iid("aaaaaa3"),
                    kind: DepKind::DiscoveredFrom,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&iid("bbbbbb3")));
        assert!(!blocked.contains(&iid("ccccc03")));
    }

    /// Parent-child cascade: A → B → C via parent-child. A is open
    /// and blocked (by a `blocks` edge to D). Both B and C are
    /// blocked via the cascade.
    #[test]
    fn parent_child_cascade_open_blocked_parent_blocks_children() {
        let d = iid("ddddddd");
        let a = iid("aaaaaa4");
        let b = iid("bbbbbb4");
        let c = iid("ccccc04");
        let all = vec![
            mk_issue("ddddddd", Status::Open, vec![]),
            mk_issue(
                "aaaaaa4",
                Status::Open,
                vec![DepEdge {
                    target: d.clone(),
                    kind: DepKind::Blocks,
                }],
            ),
            mk_issue(
                "bbbbbb4",
                Status::Open,
                vec![DepEdge {
                    target: a.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
            mk_issue(
                "ccccc04",
                Status::Open,
                vec![DepEdge {
                    target: b.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        // A is blocked by the blocks edge.
        assert!(blocked.contains(&a));
        // B and C inherit via the parent-child cascade.
        assert!(blocked.contains(&b));
        assert!(blocked.contains(&c));
        // D is not blocked.
        assert!(!blocked.contains(&d));
    }

    /// Parent-child cascade: same A → B → C chain, but A is CLOSED.
    /// Closed parents do NOT block children (closed deps don't
    /// participate). B and C are NOT blocked.
    #[test]
    fn parent_child_cascade_closed_parent_does_not_block_children() {
        let a = iid("aaaaaa5");
        let b = iid("bbbbbb5");
        let c = iid("ccccc05");
        let all = vec![
            mk_issue("aaaaaa5", Status::Closed, vec![]),
            mk_issue(
                "bbbbbb5",
                Status::Open,
                vec![DepEdge {
                    target: a.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
            mk_issue(
                "ccccc05",
                Status::Open,
                vec![DepEdge {
                    target: b.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&b));
        assert!(!blocked.contains(&c));
    }

    /// Parent-child cascade: A is open and NOT blocked (no blocks
    /// edge, no blocked parent). B and C are NOT blocked via the
    /// cascade — the cascade only fires when the parent is BLOCKED,
    /// not merely open.
    #[test]
    fn parent_child_cascade_open_but_unblocked_parent_does_not_block_children() {
        let a = iid("aaaaaa6");
        let b = iid("bbbbbb6");
        let c = iid("ccccc06");
        let all = vec![
            mk_issue("aaaaaa6", Status::Open, vec![]),
            mk_issue(
                "bbbbbb6",
                Status::Open,
                vec![DepEdge {
                    target: a.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
            mk_issue(
                "ccccc06",
                Status::Open,
                vec![DepEdge {
                    target: b.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&a));
        assert!(!blocked.contains(&b));
        assert!(!blocked.contains(&c));
    }

    /// Cycle handling: A → B and B → A via parent-child, neither
    /// closed, neither has a blocks edge. The fixpoint terminates
    /// (bounded by issue count) and treats both as NOT BLOCKED.
    /// Documented policy: a pure parent-child cycle with no external
    /// blocker doesn't propagate — there's nothing to cascade.
    #[test]
    fn parent_child_cycle_without_external_blocker_terminates_and_unblocks() {
        let a = iid("aaaaaa7");
        let b = iid("bbbbbb7");
        let all = vec![
            mk_issue(
                "aaaaaa7",
                Status::Open,
                vec![DepEdge {
                    target: b.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
            mk_issue(
                "bbbbbb7",
                Status::Open,
                vec![DepEdge {
                    target: a.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
        ];
        // The fixpoint must terminate. If the loop runs forever the
        // test hangs — which is the assertion, in effect.
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&a));
        assert!(!blocked.contains(&b));
    }

    /// Cycle handling with external blocker: A → B and B → A via
    /// parent-child, AND A has a blocks-edge to C (open). The
    /// cascade now propagates: A is blocked → B is blocked (B is
    /// child of A) → A is already blocked (idempotent). The fixpoint
    /// terminates with BOTH blocked.
    #[test]
    fn parent_child_cycle_with_external_blocker_propagates_to_both() {
        let a = iid("aaaaaa8");
        let b = iid("bbbbbb8");
        let c = iid("ccccc08");
        let all = vec![
            mk_issue("ccccc08", Status::Open, vec![]),
            mk_issue(
                "aaaaaa8",
                Status::Open,
                vec![
                    DepEdge {
                        target: b.clone(),
                        kind: DepKind::ParentChild,
                    },
                    DepEdge {
                        target: c.clone(),
                        kind: DepKind::Blocks,
                    },
                ],
            ),
            mk_issue(
                "bbbbbb8",
                Status::Open,
                vec![DepEdge {
                    target: a.clone(),
                    kind: DepKind::ParentChild,
                }],
            ),
        ];
        let blocked = compute_blocked_set(&all);
        assert!(blocked.contains(&a));
        assert!(blocked.contains(&b));
        assert!(!blocked.contains(&c));
    }

    /// A dangling parent-child target (non-existent issue id) does
    /// NOT block the owner — same policy as the v1 blocks-on-
    /// dangling rule. Defensive: a typo in a dep id shouldn't wedge
    /// progress on the owner.
    #[test]
    fn dangling_parent_child_target_does_not_block() {
        let phantom = iid("fffffff");
        let b = iid("bbbbbb9");
        let all = vec![mk_issue(
            "bbbbbb9",
            Status::Open,
            vec![DepEdge {
                target: phantom.clone(),
                kind: DepKind::ParentChild,
            }],
        )];
        let blocked = compute_blocked_set(&all);
        assert!(!blocked.contains(&b));
    }
}
