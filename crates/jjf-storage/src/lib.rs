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
mod history;
mod id;
mod jj;
mod memory;
mod merge_ops;
mod op;
mod read;
mod record;
mod trailer;

use std::path::{Path, PathBuf};

pub use history::HistoryEntry;
pub use id::{IdError, IssueId};
pub use jj::JjError;
pub use memory::slugify;
pub use merge_ops::{MergeReport, MergedIssue};
pub use op::Op;
pub use record::{
    Comment, DepEdge, DepKind, Issue, IssueDraft, IssueRecord, IssueType, Memory, Status,
};

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

    /// A slug failed the v2.1 validation rules (charset, length,
    /// hyphen placement). Surfaced from `Storage::create_issue` /
    /// `Storage::update` whenever a non-`None` slug doesn't pass
    /// `validate_slug`.
    #[error("invalid slug {slug:?}: {reason}")]
    InvalidSlug {
        slug: String,
        reason: SlugInvalidReason,
    },

    /// Two open issues can't share a slug. Surfaced from
    /// `Storage::create_issue` / `Storage::update` when an attempted
    /// slug write collides with an existing OPEN issue's slug.
    /// `conflicts_with` carries the id of the issue already holding
    /// the slug so the operator can disambiguate. Closed issues
    /// release their slug; reusing a slug from a closed issue is
    /// allowed.
    #[error(
        "slug {slug:?} already in use by open issue {conflicts_with}"
    )]
    SlugCollision {
        slug: String,
        conflicts_with: IssueId,
    },

    /// `Storage::resolve` was handed a string that isn't a valid id
    /// and doesn't match any open issue's slug. The handle is
    /// preserved so the CLI's `slug_not_found` error envelope can
    /// surface the operator-supplied value.
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
    // Closed and dangling targets are NOT active and never block.
    // A `Blocked` target (v2.5) is still ACTIVE — it's parked on
    // an external signal, not done. A dep on a blocked issue still
    // blocks the dependent (the work isn't complete).
    let is_active = |target: &IssueId| -> bool {
        match by_id.get(target) {
            Some(i) => match i.status {
                Status::Open | Status::Blocked | Status::InProgress => true,
                Status::Closed => false,
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

/// A handle to a repo whose `issues` bookmark exists. Use
/// [`Storage::init`] to create the bookmark (idempotent) in a fresh
/// repo, or [`Storage::open`] when you know the bookmark is already
/// in place.
///
/// Carries a per-instance snapshot cache memo so multiple read calls
/// within one CLI invocation share the head-probe + cache load. The
/// memo is invalidated on writes (the mutator drops it), so the next
/// read sees fresh state.
#[derive(Debug, Clone)]
pub struct Storage {
    repo: JjRepo,
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
        let storage = Self {
            repo: JjRepo::open(root),
            snapshot_memo: Default::default(),
        };
        storage.maybe_migrate_v1_to_v2()?;
        Ok(storage)
    }

    /// Open or create a storage handle, bootstrapping the `issues`
    /// bookmark per spec §1.1 if absent. Idempotent: calling twice
    /// against the same repo is a no-op the second time.
    ///
    /// Three distinct outcomes:
    ///
    /// - `repo_root` is not a jj repo at all → [`Error::NotAJjRepo`].
    /// - `repo_root` is a jj repo and `issues` is missing → create an
    ///   empty seed commit (description: [`ISSUES_SEED_DESCRIPTION`]),
    ///   point the `issues` bookmark at it, step `@` off the bookmark
    ///   (so the first subsequent mutation's `jj new bookmarks(issues)`
    ///   doesn't snapshot stale working-copy state). Return Storage.
    /// - `repo_root` is a jj repo and `issues` already exists → return
    ///   Storage with no repo-side changes.
    ///
    /// **v1 → v2 inline migration.** If the v1 `bugs` bookmark is
    /// present and `issues` is not, `init` runs the migration before
    /// returning. Subsequent calls are no-ops.
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

        // v1 → v2 migration first. If a `bugs` bookmark exists and
        // `issues` doesn't, rename. After this point, the bookmark
        // probe below will succeed if migration ran.
        let storage = Self {
            repo: repo.clone(),
            snapshot_memo: Default::default(),
        };
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
            // Already initialized (or just-migrated); nothing to do.
            return Ok(storage);
        }

        // Bootstrap. Three jj calls: branch a fresh empty change off
        // root() with the seed description, point the bookmark at it,
        // then step @ off the bookmark.
        repo.run(&["new", "root()", "-m", ISSUES_SEED_DESCRIPTION])?;
        repo.run(&["bookmark", "create", ISSUES_BOOKMARK, "-r", "@"])?;
        repo.run(&["new", "root()"])?;

        Ok(storage)
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
        let snap = std::sync::Arc::new(cache::load_or_rebuild(
            &self.repo,
            self.repo.root(),
        )?);
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

    /// Create a new issue from a draft. Returns the freshly-minted
    /// issue ID. Lands one commit on the `issues` bookmark with op
    /// vocabulary `create`.
    ///
    /// Validates `draft.slug` per spec v2.1 §3.1 if present
    /// (`Error::InvalidSlug`); rejects a slug already in use by an
    /// open issue (`Error::SlugCollision`). Closed issues release
    /// their slug — reusing a closed issue's slug is allowed.
    pub fn create_issue(&self, draft: &IssueDraft) -> Result<IssueId> {
        if draft.title.trim().is_empty() {
            return Err(Error::Invalid("issue title must not be empty".into()));
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
            // Uniqueness across OPEN issues. Spec v2.1: slug
            // collisions are forbidden among open issues; closed
            // issues release their slug. The probe reads every open
            // issue; for v2.1 N is small (the live planner has 10
            // issues) so a read-all is fine. If N gets meaningfully
            // bigger a slug-index (separate ticket) is the follow-up.
            if let Some(conflict) = self.find_open_slug_collision(slug, None)? {
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
        self.commit_record_change(&summary, &ops, |wc_root| {
            write_record_json(&wc_root.join(issue_json_relpath(&id)), &record)?;
            // Comments file: create empty so readers don't trip on
            // ENOENT for new issues. Spec §4 allows empty == no comments.
            write_comments_jsonl(&wc_root.join(issue_comments_relpath(&id)), &[])?;
            Ok(())
        })?;

        Ok(id)
    }

    /// Replace the title.
    pub fn set_title(&self, id: &IssueId, title: &str) -> Result<()> {
        if title.trim().is_empty() {
            return Err(Error::Invalid("title must not be empty".into()));
        }
        let title = title.to_owned();
        self.mutate(id, &format!("jjf: issue {} - set-title", id), |rec| {
            rec.title = title.clone();
            Ok(vec![Op::SetTitle {
                issue_id: rec.id.clone(),
                title: title.clone(),
            }])
        })
    }

    /// Replace the status.
    pub fn set_status(&self, id: &IssueId, status: Status) -> Result<()> {
        self.mutate(id, &format!("jjf: issue {} - set-status", id), |rec| {
            rec.status = status;
            Ok(vec![Op::SetStatus {
                issue_id: rec.id.clone(),
                status,
            }])
        })
    }

    /// Replace the body.
    pub fn set_body(&self, id: &IssueId, body: &str) -> Result<()> {
        let body = body.to_owned();
        self.mutate(id, &format!("jjf: issue {} - set-body", id), |rec| {
            rec.body = body.clone();
            let hash = sha256_hex(body.as_bytes());
            Ok(vec![Op::SetBody {
                issue_id: rec.id.clone(),
                body_hash: hash,
            }])
        })
    }

    /// Replace the assignee. `None` clears it.
    pub fn set_assignee(&self, id: &IssueId, assignee: Option<&str>) -> Result<()> {
        let assignee = assignee.map(str::to_owned);
        self.mutate(id, &format!("jjf: issue {} - set-assignee", id), |rec| {
            rec.assignee = assignee.clone();
            Ok(vec![Op::SetAssignee {
                issue_id: rec.id.clone(),
                assignee: assignee.clone(),
            }])
        })
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
    /// Title validation matches `set_title` (non-empty after trim);
    /// other fields accept any string.
    pub fn update(&self, id: &IssueId, fields: UpdateFields) -> Result<()> {
        if fields.is_empty() {
            return Err(Error::Invalid(
                "update called with no fields set".into(),
            ));
        }
        if let Some(title) = &fields.title {
            if title.trim().is_empty() {
                return Err(Error::Invalid("title must not be empty".into()));
            }
        }
        // Pre-validate the slug, if any, BEFORE the storage-side
        // mutate dance. We validate the syntactic shape here and the
        // uniqueness probe runs INSIDE `mutate` (after we've read the
        // current record), so we know whether the issue is open and
        // can scope the collision check accordingly.
        if let Some(Some(slug)) = &fields.slug {
            if let Err(reason) = validate_slug(slug) {
                return Err(Error::InvalidSlug {
                    slug: slug.clone(),
                    reason,
                });
            }
        }
        // Slug uniqueness probe: a non-`None` slug write must not
        // collide with any OTHER open issue's slug. We probe before
        // entering `mutate` so a collision error fires before any
        // commit machinery spins up.
        if let Some(Some(new_slug)) = &fields.slug {
            if let Some(conflict) =
                self.find_open_slug_collision(new_slug, Some(id))?
            {
                return Err(Error::SlugCollision {
                    slug: new_slug.clone(),
                    conflicts_with: conflict,
                });
            }
        }
        let summary = format!("jjf: issue {} - update", id);
        self.mutate(id, &summary, |rec| {
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
            Ok(ops)
        })
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
    pub fn claim(&self, id: &IssueId, who: &str) -> Result<()> {
        let who = who.trim();
        if who.is_empty() {
            return Err(Error::Invalid(
                "claim: assignee must not be empty".into(),
            ));
        }
        let current = self.read_record_from_bookmark(id)?;
        match current.status {
            Status::Closed => {
                return Err(Error::Invalid(format!(
                    "issue {id} is closed; reopen before claiming"
                )));
            }
            Status::Blocked => {
                // v2.5: parked on an external signal. Claiming a
                // blocked issue would silently flip its status to
                // in-progress AND drop the reason on the floor —
                // confusing for the next reader. Force the operator
                // to `jjf unblock` first; the explicit step preserves
                // the audit trail.
                return Err(Error::Invalid(format!(
                    "issue {id} is blocked; unblock before claiming"
                )));
            }
            Status::InProgress => {
                // Already claimed. Same user → no-op (return Ok
                // without writing). Different user → AlreadyClaimed.
                match current.assignee.as_deref() {
                    Some(existing) if existing == who => return Ok(()),
                    Some(existing) => {
                        return Err(Error::AlreadyClaimed {
                            by: existing.to_owned(),
                        })
                    }
                    // InProgress without an assignee is a degenerate
                    // state (shouldn't happen via the normal claim
                    // path), but treat it as claimable rather than
                    // wedging.
                    None => {}
                }
            }
            Status::Open => {}
        }
        let who_owned = who.to_owned();
        self.mutate(id, &format!("jjf: issue {} - claim", id), |rec| {
            rec.assignee = Some(who_owned.clone());
            rec.status = Status::InProgress;
            Ok(vec![
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
        let current = self.read_record_from_bookmark(id)?;
        if current.status == Status::Closed {
            return Err(Error::Invalid(format!(
                "issue {id} is closed; nothing to unclaim"
            )));
        }
        if current.status == Status::Open && current.assignee.is_none() {
            // No-op: already in the unclaimed state.
            return Ok(());
        }
        self.mutate(id, &format!("jjf: issue {} - unclaim", id), |rec| {
            rec.assignee = None;
            rec.status = Status::Open;
            Ok(vec![
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
        let current = self.read_record_from_bookmark(id)?;
        if current.status == Status::Closed {
            return Err(Error::Invalid(format!(
                "issue {id} is closed; reopen before blocking"
            )));
        }
        let reason_owned = normalized;
        self.mutate(id, &format!("jjf: issue {} - block", id), |rec| {
            rec.status = Status::Blocked;
            rec.block_reason = reason_owned.clone();
            Ok(vec![
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
        let current = self.read_record_from_bookmark(id)?;
        if current.status == Status::Closed {
            return Err(Error::Invalid(format!(
                "issue {id} is closed; nothing to unblock"
            )));
        }
        if current.status == Status::Open && current.block_reason.is_none() {
            // No-op: already in the unblocked state.
            return Ok(());
        }
        self.mutate(id, &format!("jjf: issue {} - unblock", id), |rec| {
            rec.status = Status::Open;
            rec.block_reason = None;
            Ok(vec![
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
    }

    /// Add a label. No-op (per spec §5.2) if already present, but the
    /// commit is still landed so the audit log records intent.
    pub fn add_label(&self, id: &IssueId, label: &str) -> Result<()> {
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: issue {} - label-add", id), |rec| {
            if !rec.labels.iter().any(|l| l == &label) {
                rec.labels.push(label.clone());
                rec.labels.sort();
            }
            Ok(vec![Op::LabelAdd {
                issue_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
    }

    /// Remove a label. No-op (spec §5.2) if not present.
    pub fn remove_label(&self, id: &IssueId, label: &str) -> Result<()> {
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: issue {} - label-rm", id), |rec| {
            rec.labels.retain(|l| l != &label);
            Ok(vec![Op::LabelRm {
                issue_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
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
    pub fn add_dep_edge(
        &self,
        id: &IssueId,
        target: &IssueId,
        kind: DepKind,
    ) -> Result<()> {
        let target = target.clone();
        self.mutate(id, &format!("jjf: issue {} - dep-add", id), |rec| {
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
            Ok(vec![Op::DepAdd {
                issue_id: rec.id.clone(),
                dep: target.clone(),
                kind,
            }])
        })
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
            Ok(vec![Op::DepRm {
                issue_id: rec.id.clone(),
                dep: target.clone(),
                kind,
            }])
        })
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
        // replaces the prior N-spawn `read()` loop.
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
    pub fn add_comment(&self, id: &IssueId, body: &str, author: &str) -> Result<IssueId> {
        if author.trim().is_empty() {
            return Err(Error::Invalid("comment author must not be empty".into()));
        }
        let id = id.clone();
        let body = body.to_owned();
        let author = author.to_owned();
        // The issue record's update + the comments file edit are part of
        // one commit. We can't piggyback `add_comment` on `mutate()`
        // because the comments file isn't part of the JSON record.
        let mut record = self.read_record_from_bookmark(&id)?;
        let existing_comments = self.read_comments_from_bookmark(&id)?;
        record.updated_at = now_rfc3339()?;
        let comment_id = IssueId::random();
        let comment = Comment {
            id: comment_id.clone(),
            author,
            created_at: record.updated_at.clone(),
            body,
        };
        let summary = format!("jjf: issue {} - comment-add", id);
        let mut all_comments = existing_comments;
        all_comments.push(comment);
        self.commit_record_change(
            &summary,
            &[Op::CommentAdd {
                issue_id: id.clone(),
                comment_id: comment_id.clone(),
            }],
            |wc_root| {
                write_record_json(&wc_root.join(issue_json_relpath(&id)), &record)?;
                write_comments_jsonl(
                    &wc_root.join(issue_comments_relpath(&id)),
                    &all_comments,
                )?;
                Ok(())
            },
        )?;
        Ok(comment_id)
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
        // the same cost as the prior single-key `jj file show`
        // worst case, and amortizes across subsequent calls.
        let snapshot = self.snapshot()?;
        Ok(snapshot.memories.get(key).cloned())
    }

    /// Enumerate every memory present at the `issues` bookmark tip,
    /// sorted by key ascending. Reads each `memories/<key>.json` and
    /// returns the parsed records.
    pub fn list_memories(&self) -> Result<Vec<Memory>> {
        // Snapshot cache: every Memory on the bookmark tip is in
        // `snapshot.memories`. Skip the per-key `jj file show`
        // loop entirely. See `docs/storage-index-design.md`.
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
        let snapshot = self.snapshot()?;
        if let Some(issue) = snapshot.issues.get(id) {
            return Ok(issue.clone());
        }
        // Cache miss for this id (very unusual — either a race with
        // a concurrent writer between probe and lookup, OR the id
        // genuinely isn't on the bookmark). Fall through to the
        // per-id read for a sharp `IssueNotFound` error.
        read::read(&self.repo, id)
    }

    /// Resolve a user-supplied handle to a concrete `IssueId`.
    ///
    /// - If `handle` parses as an `IssueId` (7 lowercase-hex), return
    ///   that id directly. No bookmark lookup — the id is the
    ///   authoritative shape; checking existence is the caller's
    ///   job via `Storage::read`.
    /// - Otherwise, walk every issue on the bookmark and return the
    ///   id whose slug matches `handle` exactly. The lookup is
    ///   exact-match, case-sensitive (slugs are kebab-case
    ///   lowercase per validation).
    /// - If no issue's slug matches, return [`Error::SlugNotFound`].
    ///
    /// Implementation is read-all-then-match: O(N) over every
    /// `issues/<id>.json` at the bookmark tip. For v2.1's small N
    /// this is fine; if it ever proves slow, a slug → id index is
    /// the follow-up. The match scans both open AND closed issues
    /// (so the operator can `jjf show <slug>` against a closed
    /// orientation handle — uniqueness is enforced only across
    /// OPEN issues at write time).
    pub fn resolve(&self, handle: &str) -> Result<IssueId> {
        // Fast path: handle IS an id. We deliberately don't probe
        // the bookmark here — callers that need the existence check
        // get it from the subsequent `read` / mutator call.
        if let Ok(id) = IssueId::parse(handle) {
            return Ok(id);
        }
        // Slug path: HashMap lookup on the snapshot cache's
        // pre-built `slug_index`. Before the cache, this was an
        // O(N) shell-out loop — see closing comment on `b9f628b`.
        let snapshot = self.snapshot()?;
        if let Some(id) = snapshot.slug_index.get(handle) {
            return Ok(id.clone());
        }
        // The slug_index only carries OPEN/InProgress slugs (per
        // spec v2.1, closed issues release their slug). But the
        // method contract says we scan closed issues too — fall
        // back to a linear scan over the cache's full issue map.
        for issue in snapshot.issues.values() {
            if issue.slug.as_deref() == Some(handle) {
                return Ok(issue.id.clone());
            }
        }
        Err(Error::SlugNotFound {
            handle: handle.to_owned(),
        })
    }

    /// Probe for a slug collision among ACTIVE (Open or
    /// InProgress) issues. Returns `Some(id)` for the offending
    /// active issue if any other active issue carries this exact
    /// slug, `None` if the slug is free. `self_id` (if provided) is
    /// excluded from the probe — used by the update path so
    /// re-setting an issue's existing slug doesn't self-conflict.
    ///
    /// Closed issues do NOT participate: spec v2.1 says closed
    /// issues release their slug. v2.3: InProgress issues DO
    /// participate — claiming doesn't free the slug.
    fn find_open_slug_collision(
        &self,
        slug: &str,
        self_id: Option<&IssueId>,
    ) -> Result<Option<IssueId>> {
        // Snapshot cache: the cache's slug_index only carries
        // ACTIVE (Open / InProgress) slug holders by construction
        // (see `cache::SnapshotCache::from_parts`). One HashMap
        // lookup replaces the per-id `jj file show` loop.
        let snapshot = self.snapshot()?;
        if let Some(holder) = snapshot.slug_index.get(slug) {
            // The slug_index may hold a closed issue if no active
            // issue claims this slug. Re-check status to be
            // tolerant of that case (defensive — `from_parts`
            // populates active first).
            if Some(holder) == self_id {
                return Ok(None);
            }
            if let Some(issue) = snapshot.issues.get(holder) {
                match issue.status {
                    Status::Open | Status::Blocked | Status::InProgress => {
                        return Ok(Some(holder.clone()));
                    }
                    Status::Closed => return Ok(None),
                }
            }
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
        // Snapshot cache provides every id on the bookmark tip with
        // one process spawn (head probe) on the hit path. On a miss,
        // the rebuild reads every record via one batched `jj file
        // show` invocation. See `docs/storage-index-design.md`.
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
        // Read every issue on the bookmark. We need full records
        // (status, type_, dependencies, labels) for both the
        // candidate set and the dep-status lookup.
        //
        // Snapshot cache (per `docs/storage-index-design.md`):
        // probe the bookmark head, load `.jj/jjforge-cache.json` on
        // a hit, rebuild via one batched `jj file show` on a miss.
        // Replaces the prior N-spawn `read()` loop.
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
                Status::Closed => false,
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
    /// record from the bookmark tip, hands it to `f` for mutation +
    /// op-list construction, bumps `updated_at`, writes it back inside
    /// one commit.
    ///
    /// We read from the bookmark (via `jj file show -r bookmarks(issues)`)
    /// rather than from the working copy because step 4 of the dance
    /// (`jj new root()`) leaves the working copy on a fresh empty
    /// change with no issue files in it. The authoritative state lives
    /// at the bookmark.
    fn mutate<F>(&self, id: &IssueId, summary: &str, f: F) -> Result<()>
    where
        F: FnOnce(&mut IssueRecord) -> Result<Vec<Op>>,
    {
        let mut record = self.read_record_from_bookmark(id)?;
        let ops = f(&mut record)?;
        record.updated_at = now_rfc3339()?;
        let id = id.clone();
        self.commit_record_change(summary, &ops, |wc_root| {
            write_record_json(&wc_root.join(issue_json_relpath(&id)), &record)?;
            Ok(())
        })
    }

    /// Read the current `issues/<id>.json` from the bookmark tip.
    fn read_record_from_bookmark(&self, id: &IssueId) -> Result<IssueRecord> {
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

    /// Read the current `issues/<id>.comments.jsonl` from the bookmark
    /// tip. Returns an empty vec if the file is empty (the writer
    /// creates an empty file at issue-create time).
    fn read_comments_from_bookmark(&self, id: &IssueId) -> Result<Vec<Comment>> {
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

    /// Run the 4-CLI dance. `summary` is the human-readable first line
    /// of the commit message; `ops` becomes the `Jjf-Op:` trailer
    /// stanza; `apply` is the closure that mutates files inside the
    /// working copy (relative to `wc_root`, which is the repo root).
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

        // 1. jj new bookmarks(issues) -m '<msg>'
        self.repo.run(&["new", ISSUES_BOOKMARK_REVSET, "-m", &msg])?;

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

        // Drop the in-process snapshot memo. The on-disk cache file
        // stays put; the next read probes the head, sees the new
        // commit, and rebuilds.
        self.invalidate_snapshot_memo();

        Ok(())
    }

    /// Run the 4-CLI dance for a memory mutation. Memory commits carry
    /// a single `Jjf-Op: set-memory` or `unset-memory` trailer (no
    /// `Jjf-Issue:`), so we build the message directly rather than via
    /// [`build_commit_message`] (which assumes per-issue ops).
    fn commit_memory_change<F>(&self, msg: &str, apply: F) -> Result<()>
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

        // Drop the in-process snapshot memo so the next read picks
        // up the new memory record.
        self.invalidate_snapshot_memo();

        Ok(())
    }

    /// Read a single `memories/<key>.json` from the bookmark tip.
    /// Returns `Ok(None)` if the file is absent (the key doesn't
    /// exist, or `unset_memory` cleared it).
    fn read_memory_from_bookmark(&self, key: &str) -> Result<Option<Memory>> {
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

    /// Does this issue id already have a record on the bookmark? Used
    /// for the collision retry in `create_issue`.
    fn issue_exists_on_bookmark(&self, id: &IssueId) -> Result<bool> {
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
fn now_rfc3339_nanos() -> Result<String> {
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
