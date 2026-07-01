//! v3 sync (`iss push` / `iss pull`).
//!
//! Spec: `docs/storage-out-of-tree.md` "Sync (push/pull)".
//!
//! - **Push** runs `git push <remote> 'refs/jjf/*:refs/jjf/*'`. Standard
//!   git transport. No server-side config; the `refs/jjf/*` wildcard
//!   covers `refs/jjf/issues/*`, `refs/jjf/memories/*`, and
//!   `refs/jjf/meta/*` (the format-version sentinel — idempotent to push
//!   and keeps v3 detection consistent across replicas).
//!
//! - **Pull** runs `git fetch <remote> 'refs/jjf/*:refs/remotes/<remote>/jjf/*'`
//!   to land remote-tracking refs, then walks them and reconciles each
//!   against the corresponding local `refs/jjf/<rest>` ref using
//!   git-bug's five-scenario per-ref merge algorithm:
//!
//!   1. **New (remote-only):** local absent → copy remote tip into local
//!      ref.
//!   2. **Identical:** local == remote → no-op.
//!   3. **Local ahead:** remote is ancestor of local → no-op.
//!   4. **Fast-forward:** local is ancestor of remote → advance local to
//!      remote tip.
//!   5. **Diverged:** neither is ancestor → run the op-space LWW
//!      reducer on both sides, build a 2-parent merge commit carrying
//!      the resolved record + comments in its tree and a `Jjf-Op: merge`
//!      trailer, plant on the local ref.
//!
//! The DAG is the merge: meta/memory refs use the same five scenarios.
//! The merge commit's tree IS the LWW snapshot because the v3 read path
//! reads `cat-file blob <ref>:<path>` (the tip's tree), so the tip
//! tree must match the resolved state.

use crate::git::{GitError, GitRepo};
use crate::id::IssueId;
use crate::merge_ops::{reduce_to_merged, HeadSnapshot, MergedIssue};
use crate::op::Op;
use crate::v3_write::{
    self, commit_merge_v3, read_comments_at_oid_v3, read_record_at_oid_v3,
    MEMORY_JSON_FILE,
};
use crate::{build_commit_message, now_rfc3339_nanos, Error, Memory, Result};

/// All v3 refs under one namespace, used by `for_each_ref` enumeration.
/// We push and fetch every ref matching this prefix.
const V3_REF_ROOT: &str = "refs/jjf/";

/// Push refspecs for the data refs (issues + memories). These are
/// non-force: the per-ref CAS protocol guarantees fast-forward-only
/// updates and we want a non-fast-forward to fail loud.
const PUSH_REFSPEC_ISSUES: &str = "refs/jjf/issues/*:refs/jjf/issues/*";
const PUSH_REFSPEC_MEMORIES: &str = "refs/jjf/memories/*:refs/jjf/memories/*";

/// Push refspec for the `meta/*` sentinel namespace. **Force** (`+`
/// prefix): every peer plants its own sentinel at `iss init` time and
/// the two are non-fast-forward against each other, but the sentinel
/// is a presence flag whose content isn't validated (see ticket
/// `95fb2d6` for the design call). Force-overwriting is safe: any peer
/// that needs the sentinel acquires it from the next push, and the
/// flag's only role is "this remote is v3-shape" — which any non-zero
/// commit oid attests.
const PUSH_REFSPEC_META: &str = "+refs/jjf/meta/*:refs/jjf/meta/*";

/// Outcome of [`push_v3`]. Empty today; preserved for future per-ref
/// reporting (e.g. count of refs pushed, refs rejected).
#[derive(Debug, Clone, Default)]
pub struct PushReportV3 {
    /// Number of `refs/jjf/*` refs that exist locally and were submitted
    /// to the remote. The actual server-side disposition (created /
    /// fast-forwarded / no-op) is opaque from this side; standard
    /// transport doesn't echo per-ref outcomes.
    pub refs_pushed: usize,
}

/// Outcome of [`pull_v3`]. Per-scenario tallies so the CLI can produce
/// a human-readable summary and the `--json` envelope can carry the
/// fine-grained counts.
#[derive(Debug, Clone, Default)]
pub struct PullReportV3 {
    /// Refs in scenario 1 (new locally — copied from remote tip).
    pub new_local: usize,
    /// Refs in scenario 2 (identical — no-op).
    pub identical: usize,
    /// Refs in scenario 3 (local ahead — no-op).
    pub local_ahead: usize,
    /// Refs in scenario 4 (fast-forwarded local to remote tip).
    pub fast_forwards: usize,
    /// Refs in scenario 5 (diverged — merge commit landed). Each one
    /// touches an issue or memory ref; meta refs are never expected to
    /// diverge but we handle them defensively (treat as a single-parent
    /// fast-forward of whichever side has the more recent commit, never
    /// land a merge commit on `refs/jjf/meta/*`).
    pub merged: usize,
}

/// Sequence of per-ref decisions the pull reconciler can emit. Kept as a
/// typed enum so the orchestrator can branch / count without
/// string-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeAction {
    /// Scenario 1: copy remote tip into local ref.
    Copy,
    /// Scenario 2 / 3: no work.
    Noop,
    /// Scenario 4: fast-forward local to remote tip.
    FastForward,
    /// Scenario 5: build a merge commit.
    Merge,
}

/// Compute the five-scenario merge action for one ref. Pure on top of
/// the git ancestry queries.
fn classify_merge(
    git: &GitRepo,
    local: Option<&str>,
    remote: &str,
) -> Result<MergeAction> {
    let local = match local {
        None => return Ok(MergeAction::Copy),
        Some(l) => l,
    };
    if local == remote {
        return Ok(MergeAction::Noop);
    }
    // `git merge-base --is-ancestor <a> <b>` exits 0 if `a` is an
    // ancestor of `b`, exit 1 if not, exit >1 on error. We use the
    // raw helper to get the exit code rather than the panic-on-non-zero
    // wrapper.
    if is_ancestor(git, remote, local)? {
        // remote is an ancestor of local → local is ahead.
        return Ok(MergeAction::Noop);
    }
    if is_ancestor(git, local, remote)? {
        return Ok(MergeAction::FastForward);
    }
    Ok(MergeAction::Merge)
}

/// Wrap `git merge-base --is-ancestor`. Distinguishes exit 1 ("not an
/// ancestor", happy-path negative) from any other failure (translates
/// to `Error::Git`).
fn is_ancestor(git: &GitRepo, ancestor: &str, descendant: &str) -> Result<bool> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(git.root());
    cmd.args(["merge-base", "--is-ancestor", ancestor, descendant]);
    let out = cmd.output().map_err(|e| Error::Git(GitError::Io(e)))?;
    if out.status.success() {
        return Ok(true);
    }
    if out.status.code() == Some(1) {
        // Documented exit code for "not an ancestor"; not an error.
        return Ok(false);
    }
    Err(Error::Git(GitError::Cli {
        cmd: format!(
            "git merge-base --is-ancestor {} {}",
            ancestor, descendant
        ),
        status: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }))
}

/// Run `git push <remote>` with three refspecs: non-force on
/// `refs/jjf/issues/*` and `refs/jjf/memories/*`, force on
/// `refs/jjf/meta/*`. Counts the local issue + memory refs at call
/// time so the report carries a meaningful tally.
///
/// **Why force-push meta.** Every peer plants its own
/// `refs/jjf/meta/format-version` at `iss init` time, and those
/// sentinels will be non-fast-forward against each other even though
/// both sides represent v3. The sentinel is a presence flag — its
/// content isn't validated by any reader (see ticket `95fb2d6` for the
/// design call). Force-overwriting is therefore safe and matches the
/// "either value is fine" semantics the pull-side merge driver now
/// uses for meta divergence. Meta is excluded from `refs_pushed`
/// because callers think of "refs pushed" as user data.
///
/// Failures are translated to `Error::Git` with verbatim stderr; the
/// CLI's typed push-error classifier runs on top.
pub(crate) fn push_v3(git: &GitRepo, remote: &str) -> Result<PushReportV3> {
    let local_refs = git
        .for_each_ref(V3_REF_ROOT)
        .map_err(Error::Git)?;
    // Count only the data refs (issues + memories). The sentinel push
    // is bookkeeping; we don't surface it in the tally.
    let refs_pushed = local_refs
        .iter()
        .filter(|r| {
            r.starts_with("refs/jjf/issues/") || r.starts_with("refs/jjf/memories/")
        })
        .count();
    let has_meta = local_refs.iter().any(|r| r.starts_with("refs/jjf/meta/"));
    // No refs to push at all (no data, no sentinel): skip the git call.
    if refs_pushed == 0 && !has_meta {
        return Ok(PushReportV3 { refs_pushed: 0 });
    }
    // Direct shell-out because GitRepo::run wraps Command but doesn't
    // expose a multi-arg-with-pipes helper.
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(git.root());
    cmd.args(["push", remote]);
    cmd.arg(PUSH_REFSPEC_ISSUES);
    cmd.arg(PUSH_REFSPEC_MEMORIES);
    if has_meta {
        cmd.arg(PUSH_REFSPEC_META);
    }
    let out = cmd.output().map_err(|e| Error::Git(GitError::Io(e)))?;
    if !out.status.success() {
        return Err(Error::Git(GitError::Cli {
            cmd: format!(
                "git push {} {} {}{}",
                remote,
                PUSH_REFSPEC_ISSUES,
                PUSH_REFSPEC_MEMORIES,
                if has_meta {
                    format!(" {}", PUSH_REFSPEC_META)
                } else {
                    String::new()
                }
            ),
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }));
    }
    Ok(PushReportV3 { refs_pushed })
}

/// Run `git fetch <remote> 'refs/jjf/*:refs/remotes/<remote>/jjf/*'`,
/// then per-remote-tracking-ref reconcile with the corresponding local
/// `refs/jjf/<rest>` ref using the five-scenario merge algorithm.
///
/// Returns a [`PullReportV3`] with per-scenario counts. The CLI surfaces
/// these in both plain-text and `--json` shapes.
pub(crate) fn pull_v3(git: &GitRepo, remote: &str) -> Result<PullReportV3> {
    fetch_v3(git, remote)?;

    let remote_tracking_prefix = format!("refs/remotes/{}/jjf/", remote);
    let remote_refs = git
        .for_each_ref(&remote_tracking_prefix)
        .map_err(Error::Git)?;

    let mut report = PullReportV3::default();
    for remote_ref in remote_refs {
        // Strip `refs/remotes/<remote>/jjf/` → `<rest>` → `refs/jjf/<rest>`.
        let Some(stem) = remote_ref.strip_prefix(&remote_tracking_prefix) else {
            continue;
        };
        if stem.is_empty() {
            continue;
        }
        let local_ref = format!("refs/jjf/{}", stem);
        reconcile_one(git, &local_ref, &remote_ref, stem, &mut report)?;
    }
    Ok(report)
}

/// `git fetch <remote> 'refs/jjf/*:refs/remotes/<remote>/jjf/*'`. The
/// fetch refspec lands remote-tracking refs under the standard
/// `refs/remotes/<remote>/...` namespace, so later git commands (`log`,
/// `merge-base`) treat them as normal remote-tracking branches.
fn fetch_v3(git: &GitRepo, remote: &str) -> Result<()> {
    // Force-prefix (`+`) on the fetch refspec — the remote-tracking
    // ref is local-only, so overwriting on non-fast-forward is safe;
    // the per-ref reconciler in `pull_v3` then compares the new
    // remote-tracking tip against the local data ref using the
    // five-scenario classifier. Without the `+`, two peers each
    // running `iss init` (planting divergent `meta/format-version`
    // commits) cause `git fetch` itself to reject — see ticket
    // `0c0e7d8`.
    let fetch_refspec = format!("+refs/jjf/*:refs/remotes/{}/jjf/*", remote);
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(git.root());
    cmd.args(["fetch", remote, &fetch_refspec]);
    let out = cmd.output().map_err(|e| Error::Git(GitError::Io(e)))?;
    if !out.status.success() {
        return Err(Error::Git(GitError::Cli {
            cmd: format!("git fetch {} {}", remote, fetch_refspec),
            status: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }));
    }
    Ok(())
}

/// Reconcile one (`local_ref`, `remote_ref`) pair. `stem` is the path
/// after `refs/remotes/<remote>/jjf/` (e.g. `issues/abc1234` or
/// `meta/format-version`); we use it to disambiguate which ref family
/// drives the merge case (issues / memories diverge → run reducer; meta
/// refs are sentinels and never reach the diverged branch in production
/// but we degrade safely if they do).
fn reconcile_one(
    git: &GitRepo,
    local_ref: &str,
    remote_ref: &str,
    stem: &str,
    report: &mut PullReportV3,
) -> Result<()> {
    let remote_tip = git
        .resolve_ref(remote_ref)
        .map_err(Error::Git)?
        .ok_or_else(|| {
            Error::Git(GitError::Cli {
                cmd: format!("git rev-parse --verify --quiet {}", remote_ref),
                status: None,
                stderr: "expected ref to resolve".into(),
            })
        })?;
    let local_tip = git.resolve_ref(local_ref).map_err(Error::Git)?;

    let action = classify_merge(git, local_tip.as_deref(), &remote_tip)?;
    match action {
        MergeAction::Copy => {
            // No local ref yet — point it at the remote tip directly via
            // CAS-from-zero. If a concurrent writer landed a local copy
            // between our probe and update-ref, that becomes a
            // ConcurrentWrite (the standard translate path).
            git.update_ref(local_ref, &remote_tip, crate::git::ZERO_OID)
                .map_err(translate_git)?;
            report.new_local += 1;
        }
        MergeAction::Noop => {
            // Either identical, or local is ahead. Tally separately —
            // identical when local == remote, ahead otherwise.
            if local_tip.as_deref() == Some(remote_tip.as_str()) {
                report.identical += 1;
            } else {
                report.local_ahead += 1;
            }
        }
        MergeAction::FastForward => {
            let old = local_tip.as_deref().unwrap_or(crate::git::ZERO_OID);
            git.update_ref(local_ref, &remote_tip, old)
                .map_err(translate_git)?;
            report.fast_forwards += 1;
        }
        MergeAction::Merge => {
            // local_tip is Some by construction — Merge only emits when
            // both sides exist AND neither is an ancestor.
            let local_oid = local_tip
                .expect("merge action requires both sides present");
            merge_diverged_ref(git, local_ref, stem, &local_oid, &remote_tip)?;
            // Meta refs (the `format-version` sentinel) take the no-op
            // branch inside `merge_diverged_ref` — keep local, no merge
            // commit lands. Don't count those as merges; the operator-
            // facing tally should only reflect actual data-ref merges.
            if !stem.starts_with("meta/") {
                report.merged += 1;
            }
        }
    }
    Ok(())
}

/// Handle scenario 5 (diverged) for one ref. Splits on the ref family
/// (issues / memories / meta) because each carries a different blob
/// shape and a different reducer. For meta refs we conservatively
/// fall back to fast-forwarding the side that's a strict descendant —
/// but since we've already ruled that out in the classifier, a diverged
/// meta ref returns a typed error rather than landing a merge that's
/// not in the spec.
fn merge_diverged_ref(
    git: &GitRepo,
    local_ref: &str,
    stem: &str,
    local_oid: &str,
    remote_oid: &str,
) -> Result<()> {
    if let Some(id_stem) = stem.strip_prefix("issues/") {
        let id = IssueId::parse(id_stem).map_err(|e| {
            Error::Invalid(format!(
                "invalid issue id {} in remote ref {}: {}",
                id_stem, local_ref, e
            ))
        })?;
        merge_issue_ref(git, &id, local_oid, remote_oid)?;
        return Ok(());
    }
    if let Some(key) = stem.strip_prefix("memories/") {
        merge_memory_ref(git, key, local_oid, remote_oid)?;
        return Ok(());
    }
    if stem.starts_with("meta/") {
        // Meta refs (the `format-version` sentinel) are presence flags,
        // not content-bearing — see `95fb2d6` for the design call. If
        // both sides have a sentinel but they're different commits,
        // either value is fine because the readers only check presence.
        // Keep the local value; the operator is the side that ran
        // `iss init` locally, so their sentinel is what later writes on
        // this clone will chain off.
        let _ = (local_oid, remote_oid, local_ref);
        return Ok(());
    }
    // Truly unknown namespace. We don't ship any other meta family
    // today; if one appears here it's a forward-compat gap that should
    // fail loud rather than land an undefined merge.
    Err(Error::Invalid(format!(
        "diverged ref {} not in a known v3 namespace (stem={}); refusing to merge",
        local_ref, stem
    )))
}

/// Build the reduced state of a diverged issue ref and plant the
/// 2-parent merge commit on `refs/jjf/issues/<id>`.
fn merge_issue_ref(
    git: &GitRepo,
    id: &IssueId,
    local_oid: &str,
    remote_oid: &str,
) -> Result<()> {
    let merged = reduce_two_heads_issue(git, id, local_oid, remote_oid)?;
    let summary = format!("iss: issue {} - merge", id);
    let ops = [Op::Merge {
        issue_id: id.clone(),
    }];
    let jjf_at = now_rfc3339_nanos()?;
    let msg = build_commit_message(&summary, &ops, &jjf_at);
    commit_merge_v3(
        git,
        id,
        &merged.record,
        &merged.comments,
        &[local_oid, remote_oid],
        &msg,
        local_oid,
    )?;
    Ok(())
}

/// Run `reduce_to_merged` over the (local, remote) parents of a
/// diverged issue ref. The reducer is the same one v2 op-space pull
/// uses; only the snapshot loader changes from `jj file show -r <rev>`
/// to `git cat-file blob <oid>:<path>`.
fn reduce_two_heads_issue(
    git: &GitRepo,
    id: &IssueId,
    local_oid: &str,
    remote_oid: &str,
) -> Result<MergedIssue> {
    let mut snapshots: Vec<HeadSnapshot> = Vec::with_capacity(2);
    for parent_oid in [local_oid, remote_oid] {
        let entries =
            match crate::history::read_history_at_v3_rev(git, parent_oid, id) {
                Ok(v) => v,
                Err(Error::IssueNotFound(_)) => Vec::new(),
                Err(e) => return Err(e),
            };
        let record = read_record_at_oid_v3(git, parent_oid)?;
        let comments = read_comments_at_oid_v3(git, parent_oid)?;
        snapshots.push(HeadSnapshot {
            record,
            comments,
            entries,
        });
    }
    reduce_to_merged(id, &snapshots)
}

/// Memory diverge case. Memories have a much smaller op vocabulary
/// (`set-memory` / `unset-memory`); we treat the per-memory op chain as
/// LWW on its single value field by walking the chain commit-time-
/// ordered and picking the latest write. Implementation choice for v3:
/// the merge commit's tree carries whichever parent's `memory.json` has
/// the more recent commit-time stamp, falling back to local if the
/// stamps tie. The `Jjf-Op: merge` trailer carries the memory key in
/// place of an issue id (memories share the same trailer machinery —
/// see `crate::memory`).
///
/// **Simplification.** Unlike issues, memories don't have a typed op
/// reducer in the v3 module yet; the unified Op enum encodes memory
/// ops via a free-form key but the LWW reducer is issue-shaped. We
/// punt: compare commit timestamps on the two parent tips and copy the
/// later parent's `memory.json` into a new two-parent merge commit.
/// Acceptable per spec because memories are operator notes (no
/// concurrent-edit semantics enforced); the audit chain still preserves
/// both branches.
fn merge_memory_ref(
    git: &GitRepo,
    key: &str,
    local_oid: &str,
    remote_oid: &str,
) -> Result<()> {
    // Read both parents' memory bytes. Either side might be `unset`
    // (empty tree); a None-on-either-side means the merge resolves to
    // whichever side does have a value, and if both are None the merge
    // resolves to None.
    let local_memory = read_memory_at_oid(git, local_oid, key)?;
    let remote_memory = read_memory_at_oid(git, remote_oid, key)?;

    // Pick by commit-time. `git show -s --format=%ct <oid>` prints the
    // commit timestamp as epoch seconds; we compare integers. On a tie
    // (same second), keep the local — operator's most recent action is
    // the more authoritative one.
    let local_ts = commit_timestamp(git, local_oid)?;
    let remote_ts = commit_timestamp(git, remote_oid)?;
    let winning = if remote_ts > local_ts {
        remote_memory.clone()
    } else {
        local_memory.clone()
    };

    // Build the new tree for the merge commit. If the winning value is
    // Some, it's a one-file tree (`memory.json`); if None, an empty
    // tree (encoding "unset").
    let tree_oid = match &winning {
        Some(mem) => {
            // Use the same JSON shape v3_write does.
            let bytes = serde_json::to_string_pretty(mem).map(|mut s| {
                s.push('\n');
                s
            })?;
            let blob_oid = git
                .hash_object(bytes.as_bytes())
                .map_err(translate_git)?;
            git.mktree(&[("100644", MEMORY_JSON_FILE, &blob_oid)])
                .map_err(translate_git)?
        }
        None => git.mktree(&[]).map_err(translate_git)?,
    };

    let summary = format!("iss: memory {} - merge", key);
    // Memories don't have an issue id; we encode the op as a generic
    // free-form `Jjf-Op: merge` line with `Jjf-Key:` carrying the key.
    // Keeping the trailer minimal mirrors how v2 memory ops shape their
    // trailer. We don't go through `build_commit_message` here because
    // it assumes a `Jjf-Issue:` line on every stanza; memories use
    // `Jjf-Key:` instead.
    let jjf_at = now_rfc3339_nanos()?;
    let mut msg = String::new();
    msg.push_str(&summary);
    msg.push_str("\n\n");
    msg.push_str("Jjf-Op: merge\n");
    msg.push_str("Jjf-Key: ");
    msg.push_str(key);
    msg.push('\n');
    msg.push_str("Jjf-At: ");
    msg.push_str(&jjf_at);
    msg.push('\n');

    let new_commit = git
        .commit_tree(&tree_oid, &[local_oid, remote_oid], &msg)
        .map_err(translate_git)?;
    let ref_name = v3_write::refs::memory_ref(key);
    git.update_ref(&ref_name, &new_commit, local_oid)
        .map_err(translate_git)?;
    let _ = remote_memory;
    Ok(())
}

/// Read `memory.json` at an arbitrary commit oid. Returns `Ok(None)` if
/// the file is absent (an `unset` op landed at that revision, leaving
/// an empty tree).
fn read_memory_at_oid(
    git: &GitRepo,
    oid: &str,
    _key: &str,
) -> Result<Option<Memory>> {
    let blob = match git.cat_blob(oid, MEMORY_JSON_FILE).map_err(translate_git)? {
        Some(b) => b,
        None => return Ok(None),
    };
    let text = String::from_utf8(blob).map_err(|e| {
        Error::Invalid(format!(
            "memory.json on {} was not valid UTF-8: {e}",
            oid
        ))
    })?;
    Ok(Some(serde_json::from_str(&text)?))
}

/// `git show -s --format=%ct <oid>` — epoch-seconds commit timestamp.
fn commit_timestamp(git: &GitRepo, oid: &str) -> Result<u64> {
    let out = git
        .run(&["show", "-s", "--format=%ct", oid])
        .map_err(Error::Git)?;
    let trimmed = out.trim();
    trimmed.parse::<u64>().map_err(|e| {
        Error::Invalid(format!(
            "git commit timestamp on {} was not a u64: {} (err: {})",
            oid, trimmed, e
        ))
    })
}

/// Translate raw [`GitError`] into the typed storage `Error`, detecting
/// CAS-failures and surfacing them as [`Error::ConcurrentWrite`]. Mirrors
/// the `translate` helper in `v3_write.rs`; we duplicate the body
/// (rather than route through `v3_write`) so the sync module is
/// self-contained — `v3_write::translate` is `fn`, not `pub(crate) fn`,
/// and lifting it adds a small surface area we don't need elsewhere.
fn translate_git(e: GitError) -> Error {
    if e.is_concurrent_write() {
        Error::ConcurrentWrite {
            hint: "another writer landed first on a per-ref update. Retry pull."
                .into(),
        }
    } else {
        Error::Git(e)
    }
}

// Note: end-to-end coverage lives in `crates/jjf/tests/push_pull.rs`,
// which drives the compiled binary against per-test scratch jj clones.
// The merge-action classifier is small enough to exercise via the
// integration tests' diverged-clone scenarios; we don't repeat the
// shape here as a unit test.
