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
pub use record::{Comment, Issue, IssueDraft, IssueRecord, IssueType, Memory, Status};

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
}

/// Filter bundle for [`Storage::list_ready`].
///
/// All filters AND with the implicit "open + unblocked" criteria;
/// within each filter axis the semantics match `jjf ls`:
///
/// - `labels`: AND — an issue must carry EVERY listed label.
/// - `types`: OR — an issue's type must equal AT LEAST ONE listed
///   type. Empty filter accepts every type.
/// - `limit`: truncate the returned vec after the priority sort.
///   `None` means unlimited.
///
/// The default value (`ReadyFilter::default()`) is "no extra
/// filters" — equivalent to `jjf ready` with no flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadyFilter {
    pub labels: Vec<String>,
    pub types: Vec<IssueType>,
    pub limit: Option<usize>,
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

/// A handle to a repo whose `issues` bookmark exists. Use
/// [`Storage::init`] to create the bookmark (idempotent) in a fresh
/// repo, or [`Storage::open`] when you know the bookmark is already
/// in place.
#[derive(Debug, Clone)]
pub struct Storage {
    repo: JjRepo,
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
        let storage = Self { repo: repo.clone() };
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
            type_,
            labels: sorted_dedup(&draft.labels),
            dependencies: sorted_dedup_ids(&draft.dependencies),
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
                dep: dep.clone(),
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

    /// Add a dependency.
    pub fn add_dependency(&self, id: &IssueId, dep: &IssueId) -> Result<()> {
        let dep = dep.clone();
        self.mutate(id, &format!("jjf: issue {} - dep-add", id), |rec| {
            if !rec.dependencies.iter().any(|d| d == &dep) {
                rec.dependencies.push(dep.clone());
                rec.dependencies.sort();
            }
            Ok(vec![Op::DepAdd {
                issue_id: rec.id.clone(),
                dep: dep.clone(),
            }])
        })
    }

    /// Remove a dependency.
    pub fn remove_dependency(&self, id: &IssueId, dep: &IssueId) -> Result<()> {
        let dep = dep.clone();
        self.mutate(id, &format!("jjf: issue {} - dep-rm", id), |rec| {
            rec.dependencies.retain(|d| d != &dep);
            Ok(vec![Op::DepRm {
                issue_id: rec.id.clone(),
                dep: dep.clone(),
            }])
        })
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
        self.read_memory_from_bookmark(key)
    }

    /// Enumerate every memory present at the `issues` bookmark tip,
    /// sorted by key ascending. Reads each `memories/<key>.json` and
    /// returns the parsed records.
    pub fn list_memories(&self) -> Result<Vec<Memory>> {
        let keys = self.list_memory_keys()?;
        let mut out = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(mem) = self.read_memory_from_bookmark(&key)? {
                out.push(mem);
            }
        }
        Ok(out)
    }

    /// Read a single issue back from the `issues` bookmark tip. Returns
    /// the latest scalar field values plus the full chronological
    /// comment thread. Errors with `IssueNotFound` if `issues/<id>.json`
    /// is absent at the bookmark.
    ///
    /// Implementation cross-checks the file-read view against an
    /// op-replay view in debug builds — see `read.rs` for the rules.
    pub fn read(&self, id: &IssueId) -> Result<Issue> {
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
        // Slow path: scan every issue's slug.
        for id in self.list_ids()? {
            let rec = self.read_record_from_bookmark(&id)?;
            if rec.slug.as_deref() == Some(handle) {
                return Ok(id);
            }
        }
        Err(Error::SlugNotFound {
            handle: handle.to_owned(),
        })
    }

    /// Probe for a slug collision among OPEN issues. Returns
    /// `Some(id)` for the offending open issue if any other open
    /// issue carries this exact slug, `None` if the slug is free.
    /// `self_id` (if provided) is excluded from the probe — used by
    /// the update path so re-setting an issue's existing slug
    /// doesn't self-conflict.
    ///
    /// Closed issues do NOT participate: spec v2.1 says closed
    /// issues release their slug.
    fn find_open_slug_collision(
        &self,
        slug: &str,
        self_id: Option<&IssueId>,
    ) -> Result<Option<IssueId>> {
        for id in self.list_ids()? {
            if Some(&id) == self_id {
                continue;
            }
            let rec = self.read_record_from_bookmark(&id)?;
            if rec.status != Status::Open {
                continue;
            }
            if rec.slug.as_deref() == Some(slug) {
                return Ok(Some(id));
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
        // `jj file list` prints paths relative to the CURRENT WORKING
        // DIRECTORY by default — which means a subprocess invoked from
        // any cwd other than the repo root sees its output prefixed with
        // a relative climb (e.g. `p/jjforge/crates/.../issues/<id>.json`).
        // We pin the output to repo-root-relative slash-paths by piping
        // the `TreeEntry.path()` `RepoPath` through the template (its
        // template form is exactly that). This is cwd-independent and
        // stable across platforms.
        let text = self.repo.run(&[
            "file",
            "list",
            "-r",
            ISSUES_BOOKMARK_REVSET,
            "-T",
            "path ++ \"\\n\"",
            "root:issues/",
        ])?;
        let mut ids: Vec<IssueId> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            // Path shape: `issues/<7-hex>.json`. We only key off `.json`
            // entries — `.comments.jsonl` siblings belong to issues that
            // also have a `.json`, so they'd double-count. A bare-named
            // file (no `issues/` prefix) or a file without `.json` is
            // skipped silently.
            let Some(rest) = line.strip_prefix("issues/") else {
                continue;
            };
            let Some(stem) = rest.strip_suffix(".json") else {
                continue;
            };
            // Defensive: parse as IssueId (rejects uppercase, wrong
            // length, non-hex). A stray `issues/foo.json` is skipped
            // rather than blowing up enumeration.
            if let Ok(id) = IssueId::parse(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Enumerate every OPEN issue whose dependencies are all closed —
    /// the agent-ready set.
    ///
    /// An issue is "ready" iff:
    ///
    /// - Its `status` is [`Status::Open`].
    /// - Every id in its `dependencies` field either points at a
    ///   [`Status::Closed`] issue OR at a non-existent issue id (a
    ///   dangling reference). Open deps block; closed and dangling
    ///   deps don't.
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
        let ids = self.list_ids()?;
        let mut all: Vec<Issue> = Vec::with_capacity(ids.len());
        for id in &ids {
            all.push(self.read(id)?);
        }

        // Build the closed-id set from the SAME read pass — no
        // second IO round-trip. An open issue that depends on an id
        // NOT in `all` (dangling) is treated as unblocked: the dep
        // can't ever close (it doesn't exist), so we don't want to
        // wedge progress on an editing typo.
        //
        // The sets own clones of the ids so the subsequent
        // `all.into_iter().filter(...)` can move issues out of `all`
        // without a borrow conflict.
        let known: std::collections::HashSet<IssueId> =
            all.iter().map(|i| i.id.clone()).collect();
        let closed: std::collections::HashSet<IssueId> = all
            .iter()
            .filter(|i| i.status == Status::Closed)
            .map(|i| i.id.clone())
            .collect();

        // Roadmap-type issues are never returned. They're never work
        // to do; they're the planning surface itself. The ticket and
        // the closing comment on `7100b51` both pin this.
        let mut ready: Vec<Issue> = all
            .into_iter()
            .filter(|i| i.status == Status::Open)
            .filter(|i| i.type_ != IssueType::Roadmap)
            .filter(|i| {
                // Every dep is either closed or dangling. An open,
                // existing dep blocks.
                i.dependencies
                    .iter()
                    .all(|d| !known.contains(d) || closed.contains(d))
            })
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

fn sorted_dedup_ids(xs: &[IssueId]) -> Vec<IssueId> {
    let mut v: Vec<IssueId> = xs.to_vec();
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
fn now_rfc3339() -> Result<String> {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| Error::Clock(format!("system clock before unix epoch: {e}")))?;
    Ok(epoch_secs_to_rfc3339(dur.as_secs()))
}

/// Current time as RFC 3339 in UTC with nanosecond resolution. Used by
/// the trailer writer so each op stanza carries a `Jjf-At:` field with
/// enough resolution that two ops on the same second (a real failure
/// mode at jj 0.40's per-second commit-time granularity) sort
/// deterministically. The JSON record's `created_at`/`updated_at`
/// continue to use [`now_rfc3339`] per spec §3.1 — only trailers get
/// nanos.
fn now_rfc3339_nanos() -> Result<String> {
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
}
