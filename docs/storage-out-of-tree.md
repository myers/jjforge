# jjforge storage v3: out-of-tree refs

Status: design, not yet implemented. Pinned by research ticket
`487536a storage-out-of-tree-refs`. This doc is the verdict
the ticket asked for: chosen storage shape, op log shape, sync
refspec, migration plan from v2, and a cost estimate.

The implementation is a separate epic (`bd98097
storage-v3-out-of-tree`). The acceptance criteria on `487536a`
are satisfied by this doc PLUS that epic existing.

## TL;DR

Move every issue off the `issues` jj bookmark and into a
per-issue git ref under `refs/jjf/issues/<id>`. Each ref's
commit chain IS that issue's op log; new ops are appended as
new commits via `git update-ref`, not via jj's working-copy
dance. The `issues` bookmark goes away entirely — there is no
shared bookmark in v3.

This kills the HEAD-drift problem at its source: jjforge stops
calling `jj new bookmarks(issues)` / `jj bookmark set` and
stops moving the jj working copy at all. The `JJF_ALLOW_SELF_HOST`
guard dies. The sibling-working-dir pattern dies. Any colocated
jj+git repo can host its own planner.

## Why not the other two shapes

The research ticket framed three candidates. Two are rejected.

### Rejected: single bookmark, tree-shaped storage (v2 + JSON)

This is the v2 shape. Every mutation rewrites a tree on the
`issues` bookmark via the 4-CLI jj write dance. The dance is
the root cause of HEAD drift in colocated repos. **Any design
that uses a jj bookmark inherits the drift** — `jj bookmark set`
+ `jj new root()` is what moves git HEAD onto `refs/jj/root`.
We're not going to fix this by tweaking the dance; the dance
itself is the problem.

### Rejected: per-issue refs without a commit chain (snapshot-only)

A degenerate variant: `refs/jjf/issues/<id>` points at a single
commit holding the latest JSON snapshot; each mutation rewrites
that commit (force-update the ref). This works mechanically and
keeps ref count down to one per issue. But it throws away:

- The op-replay cross-check we already use (`crates/jjf-storage/src/read.rs`
  walks the bookmark history and replays ops as a sanity check
  against the snapshot file). Without a chain there's nothing to
  replay.
- Multi-writer convergence. The DAG of operation commits is what
  lets two clients edit the same issue offline and rejoin via a
  merge commit. A force-pushed snapshot ref loses one writer's
  work on conflict.
- The trailer-log audit surface. Today we can `git log` an
  issue's history via the bookmark; with a snapshot-only ref
  there's no history to log.

This shape is cheaper to implement but strictly worse.

### Chosen: per-issue refs with commit-chain-of-op-packs

Git-bug's pattern, verified by reading 34 archived `refs/bugs/*`
refs in this repo. Each ref's commit chain is the op log for
one issue. Each commit's tree carries the current snapshot of
that issue's JSON. The trailer-block shape (one or more
`Jjf-Op:` stanzas per commit message) carries forward unchanged
from v2.

This is the shape git-bug shipped, in production for years,
with `git push`/`pull` round-tripping cleanly via standard git
transport.

## v3 storage shape

### Ref layout

```
refs/jjf/issues/<7hex-id>        # one ref per open or closed issue
refs/jjf/memories/<key-slug>     # one ref per persistent memory
refs/jjf/meta/format-version     # singleton: holds the v3 marker
```

The 7-hex id is jjforge's existing canonical id (per
`docs/storage-format.md` §2). No id-length change. No
SHA-256 promotion; we keep the 7-hex generation logic.

**Ref count at scale.** Asterinas migration target is ~968
issues. 968 + ~10 memories + 1 meta = ~979 refs under
`refs/jjf/*`. Git stores these in `.git/packed-refs` after
`git pack-refs`, so the on-disk size is ~50 bytes per ref ≈
50 KB. Negligible. (Git-bug repos with 10K+ bugs are documented
to work fine.)

**Memories.** Same shape as issues, separate subnamespace. The
existing v2 `memories/<key>.json` file storage moves to
`refs/jjf/memories/<key-slug>` with its own commit-chain log.

### Op log shape (per ref)

Each commit on `refs/jjf/issues/<id>`:

```
tree:
  issue.json            # the full current snapshot of the issue
  comments.jsonl        # the full current comments file (if any)

message:
  <summary line>
  <blank>
  Jjf-Op: <kind>
  Jjf-Issue: <id>
  Jjf-At: <RFC3339 nanos>
  Jjf-<op-specific>: <payload>
  [...more trailer stanzas if multi-op commit...]
```

**The tree carries the snapshot, the trailer carries the op.**
This is a deliberate redundancy:

- Snapshot-in-tree means fast reads: `git cat-file blob
  refs/jjf/issues/<id>:issue.json` is one git call, no replay.
- Trailer-in-message means an auditable op log: `git log
  refs/jjf/issues/<id>` shows every mutation in order, parsable
  by the existing v2 trailer parser (`crates/jjf-storage/src/trailer.rs`).
- The op-replay cross-check (currently in
  `crates/jjf-storage/src/read.rs`) still works — walk the
  chain, replay ops, compare to the snapshot in the tip's tree,
  panic in debug if they diverge. This is the same safety net
  we have today, ported.

**The trailer vocabulary doesn't change.** Every `Op` kind that
v2 emits (`create`, `set-title`, `set-status`, `set-body`,
`set-assignee`, `set-block-reason`, `set-slug`, `set-type`,
`label-add`, `label-rm`, `dep-add`, `dep-rm`, `comment-add`,
`merge`, `set-memory`, `unset-memory`) carries forward
identically. The trailer parser stays unchanged.

The trailer-injection hardening that landed this session
(`1dce1ec8`, `2348d168`, `4aa787f`, `434a2f87`) — newline
rejection on assignee/label/block-reason, self-dep check,
phantom-target check — is pure boundary validation. It
transfers as-is.

### Write path

Each mutation runs:

1. **Read current state.** `git cat-file blob refs/jjf/issues/<id>:issue.json`
   (or read an empty record for a `create`).
2. **Apply the op in memory.** Same code as today's write side
   — produce the next snapshot.
3. **Build a tree.** `git mktree` (or `git hash-object -w` for
   each blob then `git mktree`) for the new `issue.json` /
   `comments.jsonl`.
4. **Build a commit.** `git commit-tree <tree> -p <prev-tip> -m <msg>`
   where `<msg>` is the summary + trailer block. For a `create`,
   no `-p`.
5. **Atomically update the ref.** `git update-ref refs/jjf/issues/<id> <new-commit> <old-commit>`
   — the third argument forces git to verify the ref still
   points at `<old-commit>` before updating, so concurrent
   writes fail loudly instead of silently overwriting.

**Zero jj calls on the write path.** The jj CLI is bypassed
entirely. Git HEAD never moves. The jj working copy never
changes. The 4-CLI dance is gone; step 4 (the HEAD-drift
trigger) is gone with it.

Concurrent writes from the same process retry on
`update-ref` failure (mirrors the v2.5 `ConcurrentWrite` retry
policy landed in `434a2f87`). Concurrent writes from different
processes / different clients get caught at `git update-ref`'s
CAS check; the loser re-reads the new tip, replays its op,
retries.

### Read path

`Storage::read_record(id)`:

1. `git cat-file blob refs/jjf/issues/<id>:issue.json` → parse JSON.

That's it. One git call, no jj, no working-copy state.
Comments: `git cat-file blob refs/jjf/issues/<id>:comments.jsonl`.

`Storage::list_records()`:

1. `git for-each-ref --format='%(refname)' refs/jjf/issues/`
2. For each ref, `git cat-file blob <ref>:issue.json`.

This is N+1 git calls for N issues. The snapshot cache pattern
from v2 (`.jj/jjforge-cache.json`) ports cleanly: cache keyed
by the concatenation of all `refs/jjf/issues/*` ref shas; bulk
reads hit the cache when the ref-sha set is unchanged. For 968
asterinas issues, ref enumeration plus 968 `cat-file blob`
calls is ~1–2s cold, ~10ms warm-cache. Acceptable; optimizable
later via `git cat-file --batch`.

`Storage::list_ready()` uses the cache.

### Sync (push/pull)

The `jjf push` / `jjf pull` operator interface stays the same.
The implementation swaps `git push origin issues:issues` for:

```
git push origin 'refs/jjf/*:refs/jjf/*'
git fetch origin 'refs/jjf/*:refs/remotes/<remote>/jjf/*'
```

The fetch refspec mirrors git-bug's convention — remote-tracking
refs land under `refs/remotes/<remote>/jjf/*` rather than
overwriting local refs directly. The pull verb then merges
remote-tracking refs into local refs (git-bug's five-scenario
merge algorithm: fast-forward on ancestor, no-op on identical,
merge-commit on divergence).

Standard git transport. No server-side config. **No web-UI
visibility for issue data.** Forgejo / GitLab / Gitea show only
`refs/heads/` and `refs/tags/`; `refs/jjf/*` is invisible from
the web view. This is consistent with git-bug's experience and
explicitly acceptable per the research ticket — jjforge data is
read via `jjf show`, not via the web UI.

The `jjf remote add|ls|rm` verbs wrap `jj git remote *` today;
they keep doing so (jj remote management still works — we're
just not using jj's bookmark transport, but jj's remote config
is fine).

## v2 → v3 migration

Auto-migrate at first `Storage::open` on a v2-shape repo, same
detection pattern as v1 → v2 (the existing
`Storage::maybe_migrate_v1_to_v2` at
`crates/jjf-storage/src/lib.rs:892–1000`).

**Detection.** A repo is v2-shape if the `issues` bookmark
exists and `refs/jjf/issues/*` does not. A repo is v3-shape if
`refs/jjf/issues/*` exists. We don't store a separate version
file; the presence of the v3 refs IS the version marker.

**Migration steps:**

1. List every issue id from the v2 `issues` bookmark by
   enumerating `issues/<id>.json` files at the bookmark tip
   (same as the existing v1 → v2 walker).
2. For each issue:
   a. Walk the v2 op log for that issue
      (`crates/jjf-storage/src/history.rs::read_history_at`)
      to get the full chain of commits whose trailer's
      `Jjf-Issue:` matches `<id>`.
   b. Replay each op-commit as a new commit on
      `refs/jjf/issues/<id>`, preserving the trailer block
      verbatim. Tree carries the snapshot at that point in
      history.
   c. After every op is replayed, the ref tip's snapshot
      matches the v2 snapshot file.
3. Same loop for memories: enumerate `memories/<key>.json` →
   walk the per-key op log → replay onto `refs/jjf/memories/<key>`.
4. **Delete the `issues` bookmark.** `jj bookmark delete issues`
   in a non-colocated repo, or `git update-ref -d refs/heads/issues`
   (jj's bookmarks are git refs under the hood). The bookmark
   is gone; the working copy stays untouched.
5. Write `refs/jjf/meta/format-version` pointing at a commit
   carrying a one-line `version: 3` blob. This is the
   idempotency marker — re-opens after migration see the v3
   refs and skip the migration.

**Preserves op-log granularity.** Every v2 op becomes one v3
commit. `git log refs/jjf/issues/<id>` shows the same audit
trail post-migration as `jj log -r 'descendants(<v2-create-commit>)
& trailers("Jjf-Issue: <id>")'` showed pre-migration.

**The migration is one-shot and irreversible.** No v3 → v2
downgrade path. We expect the operator to dogfood for a session
or two; if it goes wrong, recover from a remote that hasn't
been pushed-to yet.

**The v2 source repo guard (`JJF_ALLOW_SELF_HOST`) stays during
the migration.** It only dies *after* migration: v3 mutations
don't move the working copy, so the guard's reason-to-exist
evaporates. The guard's deletion is a separate commit in the
v3 implementation epic, not part of the migration commit.

## What stays the same

- **JSON schema.** `Record`, `Comment`, `Memory` shapes
  (`crates/jjf-storage/src/types.rs` etc.) unchanged.
- **CLI surface.** All `jjf` verbs work the same; all
  `--json` envelopes unchanged. The user-visible behavior is
  identical except that mutating verbs no longer drift HEAD.
- **Trailer format.** Every `Jjf-Op:` / `Jjf-Issue:` /
  `Jjf-At:` etc. line unchanged. Op vocabulary unchanged.
- **Op-replay cross-check** in debug builds (`read.rs` path B
  vs path A) — ported to walk the per-issue ref's chain
  instead of the bookmark's chain.
- **Snapshot cache** — ported, keyed on per-ref shas instead of
  bookmark tip.
- **Boundary validation** — `validate_title`, `validate_no_newlines`,
  self-dep check, phantom-target check, concurrent-write retry —
  all unchanged.
- **`jjf remote *`** — wraps `jj git remote *`; jj remotes are
  git remotes, so this works regardless of where we put issue
  refs.

## What goes away

- **The 4-CLI write dance** (`crates/jjf-storage/src/lib.rs::try_commit_dance`,
  lines 2836–2860). Replaced by the git-only write path above.
- **The `issues` bookmark.** Gone after migration. No bookmark
  in v3.
- **`JJF_ALLOW_SELF_HOST` env var** and the `refuse_self_hosted_write`
  preflight on every mutating verb
  (`crates/jjf/src/preflight.rs:185–224`). Gone — the bug it
  guarded against can't happen in v3.
- **The sibling-working-dir pattern** for jjforge's own repo.
  Gone. CLAUDE.md's "Operating in a colocated jj+git repo"
  section gets a major rewrite.
- **The "step @ off bookmark" jj call** (`jj new root()`,
  step 4 of the dance). Gone — there's nothing to step off of.

## Cost estimate

Rough sizing, from the v2 surface area:

- **Write path rewrite**: ~1 session. Swap `try_commit_dance`
  for the git-only path. Every existing op writer (create,
  update, label, comment, claim, block, etc.) routes through
  the new path. The op-to-trailer serialization
  (`crates/jjf-storage/src/op.rs`) stays.
- **Read path rewrite**: ~½ session. `read_record`,
  `read_comments`, `list_records`, `replay_ops` all repoint at
  the new ref layout.
- **Init rewrite**: ~¼ session. `Storage::init` creates no
  bookmark; it just writes `refs/jjf/meta/format-version`.
- **v2 → v3 migrator**: ~½ session. Mirror the v1 → v2
  migrator's shape.
- **Push/pull refspec**: ~¼ session. Swap the refspec in
  `jjf push` / `jjf pull`. Add the pull-merge logic (git-bug's
  five-scenario algorithm); per-issue refs make it easier than
  bookmark-shaped storage since divergence is per-issue.
- **Snapshot cache port**: ~¼ session. Recompute cache key
  from per-ref shas.
- **Test sweep**: ~½ session. The 421-test workspace will need
  golden-output updates wherever a test asserts the v2 storage
  shape directly. The CLI-surface tests should stay green
  without changes.
- **Delete the self-host guard**: ~¼ session. Drop the
  preflight, rip `JJF_ALLOW_SELF_HOST` out of CLAUDE.md, fix
  the section that recommends the sibling-working-dir pattern.

**Total: ~3–4 sessions of focused work**, dispatchable as
~6–8 child tickets under one implementation epic (filed as
`bd98097`).

## Decision on the blocked asterinas migration

The asterinas migration (`cc2fa96`) was blocked by HEAD drift
when `jjf init` ran on a colocated repo with a live working
tree. Two options:

- **A. Wait for v3.** Don't re-attempt the migration until the
  redesign ships. The asterinas tree stays as a read-only
  markdown archive in the meantime. **~3–4 sessions of delay.**
- **B. Sibling-clone interim.** Migrate now using the
  sibling-working-dir workaround
  (`git clone ~/p/asterinas-workspace ~/p/asterinas-workspace-data`,
  init jjforge there, push the `issues` bookmark to the upstream
  Forgejo, pull it back into the live workspace via remote
  refs). Two checkouts on disk until v3 lands, then migrate to
  v3 along with everyone else.

**Recommendation: A. Wait.** The migration is large
(~968 Haiku-dispatched issues) and the sibling-clone interim
adds friction to every read/write on asterinas data forever
until v3 — the wrong layer to be paying that cost. The
asterinas markdown tree was an archive long before we tried
to migrate it; it can stay an archive for another 3–4 sessions
without harm.

The blocker edge on `cc2fa96` stays. `bd98097` blocks `cc2fa96`.

## Pointers

- Research ticket: `jjf show 487536a` (or `storage-out-of-tree-refs`).
- Implementation epic: `jjf show bd98097`.
- Blocked migration: `jjf show cc2fa96`.
- Prior art: git-bug's storage layer (github.com/MichaelMure/git-bug);
  34 archived `refs/bugs/*` refs in this repo are walkable via
  `git-bug bug show <id>`.
- HEAD-drift origin: issue `08cf14b`, CLAUDE.md "Operating in a
  colocated jj+git repo" section, the 2026-06-23 asterinas
  session recovery.
- Current code to rewrite: `crates/jjf-storage/src/lib.rs`
  (write dance, init, migrator), `crates/jjf-storage/src/read.rs`
  (read path), `crates/jjf/src/preflight.rs`
  (self-host guard).
- Current code to PRESERVE: `crates/jjf-storage/src/trailer.rs`
  (trailer parser), `crates/jjf-storage/src/op.rs`
  (op-to-trailer serialization), `crates/jjf-storage/src/history.rs`
  (history walker — repoint to per-issue refs, keep the
  logic).
