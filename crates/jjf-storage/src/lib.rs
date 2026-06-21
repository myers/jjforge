//! `jjf-storage` — the **write path** of jjforge's on-disk storage
//! layer.
//!
//! This crate implements the 4-CLI working-copy dance that lands a
//! single bug mutation as one commit on the `bugs` bookmark, with a
//! `Jjf-Op:` trailer (and an accompanying `Jjf-Bug:` trailer) recording
//! what changed. The on-disk schema is pinned by `docs/storage-format.md`
//! v1 (commit `2d79305`).
//!
//! # The dance
//!
//! For one mutation:
//!
//! ```text
//! jj new bookmarks(bugs) -m '<msg with trailers>'
//! <edit bugs/<id>.json (and bugs/<id>.comments.jsonl if applicable)>
//! jj bookmark set bugs -r @ --allow-backwards
//! jj new root()
//! ```
//!
//! Step 4 steps `@` off the bookmark so the next mutation's `jj new
//! bookmarks(bugs)` doesn't snapshot the previous edit into a stale
//! working copy. Lifted directly from `experiments/jj-shellout-hello/`.
//!
//! # Out of scope
//!
//! - Read path (`storage-read-single`, `storage-read-history`).
//! - `jjf init` / bookmark bootstrap (`storage-bootstrap`).
//! - The merge driver (`jjf-merge`).
//! - The `jjf` binary (mvp-cli).
//! - The `comments.jsonl` merge policy.
//!
//! # Verdict pins
//!
//! - `2130de1` — shell out to `jj`; do not link `jj-lib`.
//! - `a60bb95` — `Jjf-Op:` trailers are the audit surface.
//! - `dcd4b57` — dedicated `bugs` bookmark.

#![forbid(unsafe_code)]

mod id;
mod jj;
mod op;
mod read;
mod record;

use std::path::{Path, PathBuf};

pub use id::BugId;
pub use jj::JjError;
pub use op::Op;
pub use record::{Bug, BugDraft, BugRecord, Comment, Status};

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
    #[error("bug not found in working copy: {0}")]
    BugNotFound(BugId),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("clock: {0}")]
    Clock(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// The name of the bookmark bug data lives on. See
/// `docs/storage-format.md` §1.
pub const BUGS_BOOKMARK: &str = "bugs";

/// The revset that resolves to the tip of the `bugs` bookmark. We use
/// the function form (`bookmarks(bugs)`) rather than the bare bookmark
/// name so a stray collision with another revset never bites.
pub const BUGS_BOOKMARK_REVSET: &str = "bookmarks(bugs)";

/// A handle to a repo whose `bugs` bookmark already exists (bootstrap
/// is a different ticket — `storage-bootstrap`).
#[derive(Debug, Clone)]
pub struct Storage {
    repo: JjRepo,
}

impl Storage {
    /// Open a storage handle at the given repo root. The path must be
    /// absolute; we don't resolve `~` or relative paths — that's the
    /// caller's job (mvp-cli, tests). The `bugs` bookmark must already
    /// exist.
    pub fn open(repo_root: impl Into<PathBuf>) -> Result<Self> {
        let root = repo_root.into();
        if !root.is_absolute() {
            return Err(Error::Invalid(format!(
                "Storage::open requires an absolute path, got {}",
                root.display()
            )));
        }
        Ok(Self {
            repo: JjRepo::open(root),
        })
    }

    /// The repo root this storage handle is rooted at.
    pub fn repo_root(&self) -> &Path {
        self.repo.root()
    }

    /// Create a new bug from a draft. Returns the freshly-minted bug
    /// ID. Lands one commit on the `bugs` bookmark with op vocabulary
    /// `create`.
    pub fn create_bug(&self, draft: &BugDraft) -> Result<BugId> {
        if draft.title.trim().is_empty() {
            return Err(Error::Invalid("bug title must not be empty".into()));
        }

        // Reroll on collision. The space is 2^28 ≈ 268M and a repo
        // typically has only a handful, so this loop never runs more
        // than once in practice. We probe the bookmark (not the
        // working copy) because the dance leaves @ on root() with no
        // bug files staged.
        let id = loop {
            let candidate = BugId::random();
            if !self.bug_exists_on_bookmark(&candidate)? {
                break candidate;
            }
        };

        let now = now_rfc3339()?;
        let record = BugRecord {
            version: 1,
            id: id.clone(),
            title: draft.title.clone(),
            body: draft.body.clone(),
            status: Status::Open,
            labels: sorted_dedup(&draft.labels),
            dependencies: sorted_dedup_ids(&draft.dependencies),
            assignee: draft.assignee.clone(),
            created_at: now.clone(),
            updated_at: now,
        };

        // The `create` op trailer (spec §5.2) carries only title +
        // status. Anything else the draft seeds — body, labels,
        // dependencies, assignee — is recorded as additional ops in
        // the same commit (spec §5.5 allows multi-op commits). Without
        // this, the audit chain (and the read-path op-replay) would
        // miss seed-time fields entirely, and the v1-contract
        // cross-check would fire on every non-trivial create.
        let summary = format!("jjf: bug {} - create", id);
        let mut ops: Vec<Op> = Vec::new();
        ops.push(Op::Create {
            bug_id: id.clone(),
            title: record.title.clone(),
            status: Status::Open,
        });
        if !record.body.is_empty() {
            ops.push(Op::SetBody {
                bug_id: id.clone(),
                body_hash: sha256_hex(record.body.as_bytes()),
            });
        }
        for label in &record.labels {
            ops.push(Op::LabelAdd {
                bug_id: id.clone(),
                label: label.clone(),
            });
        }
        for dep in &record.dependencies {
            ops.push(Op::DepAdd {
                bug_id: id.clone(),
                dep: dep.clone(),
            });
        }
        if let Some(assignee) = &record.assignee {
            ops.push(Op::SetAssignee {
                bug_id: id.clone(),
                assignee: Some(assignee.clone()),
            });
        }
        self.commit_record_change(&summary, &ops, |wc_root| {
            write_record_json(&wc_root.join(bug_json_relpath(&id)), &record)?;
            // Comments file: create empty so readers don't trip on
            // ENOENT for new bugs. Spec §4 allows empty == no comments.
            write_comments_jsonl(&wc_root.join(bug_comments_relpath(&id)), &[])?;
            Ok(())
        })?;

        Ok(id)
    }

    /// Replace the title.
    pub fn set_title(&self, id: &BugId, title: &str) -> Result<()> {
        if title.trim().is_empty() {
            return Err(Error::Invalid("title must not be empty".into()));
        }
        let title = title.to_owned();
        self.mutate(id, &format!("jjf: bug {} - set-title", id), |rec| {
            rec.title = title.clone();
            Ok(vec![Op::SetTitle {
                bug_id: rec.id.clone(),
                title: title.clone(),
            }])
        })
    }

    /// Replace the status.
    pub fn set_status(&self, id: &BugId, status: Status) -> Result<()> {
        self.mutate(id, &format!("jjf: bug {} - set-status", id), |rec| {
            rec.status = status;
            Ok(vec![Op::SetStatus {
                bug_id: rec.id.clone(),
                status,
            }])
        })
    }

    /// Replace the body.
    pub fn set_body(&self, id: &BugId, body: &str) -> Result<()> {
        let body = body.to_owned();
        self.mutate(id, &format!("jjf: bug {} - set-body", id), |rec| {
            rec.body = body.clone();
            let hash = sha256_hex(body.as_bytes());
            Ok(vec![Op::SetBody {
                bug_id: rec.id.clone(),
                body_hash: hash,
            }])
        })
    }

    /// Replace the assignee. `None` clears it.
    pub fn set_assignee(&self, id: &BugId, assignee: Option<&str>) -> Result<()> {
        let assignee = assignee.map(str::to_owned);
        self.mutate(id, &format!("jjf: bug {} - set-assignee", id), |rec| {
            rec.assignee = assignee.clone();
            Ok(vec![Op::SetAssignee {
                bug_id: rec.id.clone(),
                assignee: assignee.clone(),
            }])
        })
    }

    /// Add a label. No-op (per spec §5.2) if already present, but the
    /// commit is still landed so the audit log records intent.
    pub fn add_label(&self, id: &BugId, label: &str) -> Result<()> {
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: bug {} - label-add", id), |rec| {
            if !rec.labels.iter().any(|l| l == &label) {
                rec.labels.push(label.clone());
                rec.labels.sort();
            }
            Ok(vec![Op::LabelAdd {
                bug_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
    }

    /// Remove a label. No-op (spec §5.2) if not present.
    pub fn remove_label(&self, id: &BugId, label: &str) -> Result<()> {
        let label = label.to_owned();
        self.mutate(id, &format!("jjf: bug {} - label-rm", id), |rec| {
            rec.labels.retain(|l| l != &label);
            Ok(vec![Op::LabelRm {
                bug_id: rec.id.clone(),
                label: label.clone(),
            }])
        })
    }

    /// Add a dependency.
    pub fn add_dependency(&self, id: &BugId, dep: &BugId) -> Result<()> {
        let dep = dep.clone();
        self.mutate(id, &format!("jjf: bug {} - dep-add", id), |rec| {
            if !rec.dependencies.iter().any(|d| d == &dep) {
                rec.dependencies.push(dep.clone());
                rec.dependencies.sort();
            }
            Ok(vec![Op::DepAdd {
                bug_id: rec.id.clone(),
                dep: dep.clone(),
            }])
        })
    }

    /// Remove a dependency.
    pub fn remove_dependency(&self, id: &BugId, dep: &BugId) -> Result<()> {
        let dep = dep.clone();
        self.mutate(id, &format!("jjf: bug {} - dep-rm", id), |rec| {
            rec.dependencies.retain(|d| d != &dep);
            Ok(vec![Op::DepRm {
                bug_id: rec.id.clone(),
                dep: dep.clone(),
            }])
        })
    }

    /// Append a comment. Generates a fresh 7-hex comment id and updates
    /// the bug record's `updated_at`.
    pub fn add_comment(&self, id: &BugId, body: &str, author: &str) -> Result<()> {
        if author.trim().is_empty() {
            return Err(Error::Invalid("comment author must not be empty".into()));
        }
        let id = id.clone();
        let body = body.to_owned();
        let author = author.to_owned();
        // The bug record's update + the comments file edit are part of
        // one commit. We can't piggyback `add_comment` on `mutate()`
        // because the comments file isn't part of the JSON record.
        let mut record = self.read_record_from_bookmark(&id)?;
        let existing_comments = self.read_comments_from_bookmark(&id)?;
        record.updated_at = now_rfc3339()?;
        let comment_id = BugId::random();
        let comment = Comment {
            id: comment_id.clone(),
            author,
            created_at: record.updated_at.clone(),
            body,
        };
        let summary = format!("jjf: bug {} - comment-add", id);
        let mut all_comments = existing_comments;
        all_comments.push(comment);
        self.commit_record_change(
            &summary,
            &[Op::CommentAdd {
                bug_id: id.clone(),
                comment_id: comment_id.clone(),
            }],
            |wc_root| {
                write_record_json(&wc_root.join(bug_json_relpath(&id)), &record)?;
                write_comments_jsonl(
                    &wc_root.join(bug_comments_relpath(&id)),
                    &all_comments,
                )?;
                Ok(())
            },
        )?;
        Ok(())
    }

    /// Read a single bug back from the `bugs` bookmark tip. Returns
    /// the latest scalar field values plus the full chronological
    /// comment thread. Errors with `BugNotFound` if `bugs/<id>.json`
    /// is absent at the bookmark.
    ///
    /// Implementation cross-checks the file-read view against an
    /// op-replay view in debug builds — see `read.rs` for the rules.
    pub fn read(&self, id: &BugId) -> Result<Bug> {
        read::read(&self.repo, id)
    }

    // ---- internals ---------------------------------------------------

    /// Common path for mutate-the-JSON-record ops. Reads the current
    /// record from the bookmark tip, hands it to `f` for mutation +
    /// op-list construction, bumps `updated_at`, writes it back inside
    /// one commit.
    ///
    /// We read from the bookmark (via `jj file show -r bookmarks(bugs)`)
    /// rather than from the working copy because step 4 of the dance
    /// (`jj new root()`) leaves the working copy on a fresh empty
    /// change with no bug files in it. The authoritative state lives
    /// at the bookmark.
    fn mutate<F>(&self, id: &BugId, summary: &str, f: F) -> Result<()>
    where
        F: FnOnce(&mut BugRecord) -> Result<Vec<Op>>,
    {
        let mut record = self.read_record_from_bookmark(id)?;
        let ops = f(&mut record)?;
        record.updated_at = now_rfc3339()?;
        let id = id.clone();
        self.commit_record_change(summary, &ops, |wc_root| {
            write_record_json(&wc_root.join(bug_json_relpath(&id)), &record)?;
            Ok(())
        })
    }

    /// Read the current `bugs/<id>.json` from the bookmark tip.
    fn read_record_from_bookmark(&self, id: &BugId) -> Result<BugRecord> {
        let relpath = bug_json_relpath(id);
        let text = match self.repo.run(&[
            "file",
            "show",
            "-r",
            BUGS_BOOKMARK_REVSET,
            &format!("root:{}", relpath.display()),
        ]) {
            Ok(s) => s,
            Err(_) => {
                // jj returns non-zero if the path doesn't exist at that
                // revision. Treat that as bug-not-found rather than
                // surfacing the raw jj error — callers expect a typed
                // signal.
                return Err(Error::BugNotFound(id.clone()));
            }
        };
        Ok(serde_json::from_str(&text)?)
    }

    /// Read the current `bugs/<id>.comments.jsonl` from the bookmark
    /// tip. Returns an empty vec if the file is empty (the v1 writer
    /// creates an empty file at bug-create time).
    fn read_comments_from_bookmark(&self, id: &BugId) -> Result<Vec<Comment>> {
        let relpath = bug_comments_relpath(id);
        let text = match self.repo.run(&[
            "file",
            "show",
            "-r",
            BUGS_BOOKMARK_REVSET,
            &format!("root:{}", relpath.display()),
        ]) {
            Ok(s) => s,
            Err(_) => {
                // Missing comments file => no comments. The record's
                // existence is the source of truth on whether the bug
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
        let msg = build_commit_message(summary, ops);

        // 1. jj new bookmarks(bugs) -m '<msg>'
        self.repo.run(&["new", BUGS_BOOKMARK_REVSET, "-m", &msg])?;

        // 2. Edit the working copy. jj snapshots on the next command.
        apply(self.repo.root())?;

        // 3. jj bookmark set bugs -r @ --allow-backwards
        self.repo.run(&[
            "bookmark",
            "set",
            BUGS_BOOKMARK,
            "-r",
            "@",
            "--allow-backwards",
        ])?;

        // 4. jj new root() — step @ off the bookmark.
        self.repo.run(&["new", "root()"])?;

        Ok(())
    }

    /// Does this bug id already have a record on the bookmark? Used
    /// for the collision retry in `create_bug`.
    fn bug_exists_on_bookmark(&self, id: &BugId) -> Result<bool> {
        let relpath = bug_json_relpath(id);
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
                BUGS_BOOKMARK_REVSET,
                &format!("root:{}", relpath.display()),
            ])
            .is_ok())
    }
}

// ---- record I/O ------------------------------------------------------

/// Relative path of a bug's JSON record from repo root.
pub(crate) fn bug_json_relpath(id: &BugId) -> PathBuf {
    PathBuf::from("bugs").join(format!("{}.json", id))
}

/// Relative path of a bug's comments file from repo root.
pub(crate) fn bug_comments_relpath(id: &BugId) -> PathBuf {
    PathBuf::from("bugs").join(format!("{}.comments.jsonl", id))
}

fn write_record_json(path: &Path, record: &BugRecord) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut s = serde_json::to_string_pretty(record)?;
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
pub(crate) fn build_commit_message(summary: &str, ops: &[Op]) -> String {
    let mut s = String::new();
    s.push_str(summary);
    s.push_str("\n\n");
    for (i, op) in ops.iter().enumerate() {
        if i > 0 {
            // No blank line between op stanzas — they're one continuous
            // trailer block per spec §5.5.
        }
        s.push_str(&op.to_trailer_block());
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

fn sorted_dedup_ids(xs: &[BugId]) -> Vec<BugId> {
    let mut v: Vec<BugId> = xs.to_vec();
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
        let rec = BugRecord {
            version: 1,
            id: BugId::parse("aa6600b").unwrap(),
            title: "segfault on empty input".into(),
            body: "Running `./app` with no arguments crashes.".into(),
            status: Status::Open,
            labels: vec!["bug".into(), "p1".into()],
            dependencies: vec![],
            assignee: Some("alice".into()),
            created_at: "2026-06-21T12:00:00Z".into(),
            updated_at: "2026-06-21T15:34:48Z".into(),
        };
        let s = serde_json::to_string_pretty(&rec).unwrap();
        let back: BugRecord = serde_json::from_str(&s).unwrap();
        assert_eq!(back, rec);
        // Field-ordering check: version must come before id, etc.
        let v_idx = s.find("\"version\"").unwrap();
        let id_idx = s.find("\"id\"").unwrap();
        let title_idx = s.find("\"title\"").unwrap();
        let status_idx = s.find("\"status\"").unwrap();
        let labels_idx = s.find("\"labels\"").unwrap();
        let deps_idx = s.find("\"dependencies\"").unwrap();
        let assignee_idx = s.find("\"assignee\"").unwrap();
        let created_idx = s.find("\"created_at\"").unwrap();
        let updated_idx = s.find("\"updated_at\"").unwrap();
        assert!(v_idx < id_idx);
        assert!(id_idx < title_idx);
        assert!(title_idx < status_idx);
        assert!(status_idx < labels_idx);
        assert!(labels_idx < deps_idx);
        assert!(deps_idx < assignee_idx);
        assert!(assignee_idx < created_idx);
        assert!(created_idx < updated_idx);
    }

    #[test]
    fn op_roundtrip_serde() {
        let ops = [
            Op::Create {
                bug_id: BugId::parse("aa6600b").unwrap(),
                title: "t".into(),
                status: Status::Open,
            },
            Op::SetStatus {
                bug_id: BugId::parse("aa6600b").unwrap(),
                status: Status::Closed,
            },
            Op::LabelAdd {
                bug_id: BugId::parse("aa6600b").unwrap(),
                label: "fixed".into(),
            },
            Op::Merge {
                bug_id: BugId::parse("aa6600b").unwrap(),
            },
        ];
        for op in &ops {
            let s = serde_json::to_string(op).unwrap();
            let back: Op = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, op);
        }
    }

    #[test]
    fn trailer_format_single_op_create_matches_spec() {
        // §5.4 example.
        let op = Op::Create {
            bug_id: BugId::parse("aa6600b").unwrap(),
            title: "segfault on empty input".into(),
            status: Status::Open,
        };
        let msg = build_commit_message("jjf: bug aa6600b - create", &[op]);
        let expected = "\
jjf: bug aa6600b - create

Jjf-Op: create
Jjf-Bug: aa6600b
Jjf-Title: segfault on empty input
Jjf-Status: open
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn trailer_format_multi_op_matches_spec() {
        // §5.5 example minus the free-text body (the build_commit_message
        // helper doesn't synthesize that — callers pass it via summary
        // if they want it).
        let bug = BugId::parse("aa6600b").unwrap();
        let ops = [
            Op::SetStatus {
                bug_id: bug.clone(),
                status: Status::Closed,
            },
            Op::LabelAdd {
                bug_id: bug.clone(),
                label: "fixed".into(),
            },
        ];
        let msg = build_commit_message("jjf: bug aa6600b - close + label", &ops);
        let expected = "\
jjf: bug aa6600b - close + label

Jjf-Op: set-status
Jjf-Bug: aa6600b
Jjf-Status: closed
Jjf-Op: label-add
Jjf-Bug: aa6600b
Jjf-Label: fixed
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn id_shape() {
        for _ in 0..1000 {
            let id = BugId::random();
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
        assert!(BugId::parse("aa6600b").is_ok());
        assert!(BugId::parse("AA6600B").is_err());
        assert!(BugId::parse("abcdefg").is_err());
        assert!(BugId::parse("123").is_err());
        assert!(BugId::parse("12345678").is_err());
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
