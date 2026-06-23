# Storage read-path scale: design

Status: design call from ticket `b9f628b`.
Date: 2026-06-23.
Recommendation: **ship the snapshot cache; do NOT ship a real index
yet.** Pin the index design as a fallback if cache-rebuild costs
ever become a real problem.

## 1. The scale problem, measured

Every list-shaped read path (`jjf ls`, `jjf ready`, `jjf show <slug>`,
`jjf memories`) calls `Storage::read` or `Storage::read_record_from_bookmark`
once per id. Each of those is a `jj file show` shell-out — and
sometimes two (one for `<id>.json`, one for `<id>.comments.jsonl`).
Per `experiments/jj-cli-overhead/README.md`, one `jj` invocation
costs ~15 ms in process-startup-and-repo-discovery before it does any
actual work.

Measured wall-clock at three sizes against a synthetic bookmark
(`experiments/scale-index/`, Apple Silicon, jj 0.40.0):

| N | `list_ready` | `resolve` slug worst case | wc-walk rebuild | cache hit |
|---|-----:|-----:|-----:|-----:|
| 17 | 0.38s | 0.19s | 0.09s | 0.014s |
| 100 | 2.23s | 1.10s | 0.14s | 0.014s |
| 1000 | 22.5s | 11.2s | 0.60s | 0.015s |
| 10000 | ~225s (extrap) | ~110s (extrap) | 5.1s | 0.025s |

The headline number is `list_ready` at N=1000: **22.5 seconds**. At
N=10000 the linear extrapolation is **~4 minutes per call**. That's
not a future problem — `~/p/asterinas-workspace` has ~1000 issues
today and the operator wants to migrate it to jjforge. We're at the
cliff.

The interesting cell is the **cache-hit column**: ~15 ms regardless
of N (the floor is one `jj log` to read the bookmark head). That's
the head-commit probe ("did the bookmark move since the cache was
written?") plus deserializing a pre-built JSON file. The cache
itself is built once when the bookmark moves; reads after that pay
~15 ms.

`wc-walk rebuild` is a stand-in for what a snapshot cache's "miss
path" would actually do: `jj edit bookmarks(issues)` to materialize
every file in the working copy, then a normal directory walk and
`serde_json::from_str`. At N=10000 it's 5 seconds. Worst-case the
operator pays that once per write that moves the bookmark.

## 2. Snapshot cache — the v1 fix

Ship this. Cheapest possible thing that crushes the read path.

**Location.** `.jj/jjforge-cache.json` (or `.bincode` / `.cbor` for
faster parse — JSON parsing for 10k issues is ~10 ms and not the
hot path, so we pick whichever is most operator-readable; JSON
wins). `.jj/` is already gitignored, so the cache is invisible to
git by construction.

**Schema (sketch).**

```json
{
  "schema_version": 1,
  "head_commit": "<jj commit_id at write time>",
  "issues": {
    "<id>": { "<the Issue record + comments>" }
  },
  "memories": {
    "<key>": { "<the Memory record>" }
  },
  "slug_index": {
    "<slug>": "<id>"
  }
}
```

The slug index is redundant with the per-issue records but lets
`resolve(slug)` be a HashMap lookup instead of a full scan even on
the cache hit path. Cost: ~0 bytes (slugs are short, ~10k of them
≈ 200 KB).

**Read path.**

```
fn open_or_load_cache(repo) -> Cache {
    let head = repo.run(["log", "-r", "bookmarks(issues)",
                          "-T", "commit_id", "--limit", "1",
                          "--no-graph"]);
    if let Some(cache) = read_cache_file(".jj/jjforge-cache.json") {
        if cache.head_commit == head {
            return cache;  // hit; ~15 ms total
        }
    }
    rebuild_cache(repo, head)  // miss; up to ~5 s at N=10k
}
```

`Storage::list_ready` / `Storage::list` / `Storage::resolve` /
`Storage::list_memories` all become trivial map operations on the
loaded cache. `Storage::read(id)` becomes a single HashMap lookup
plus the existing comment-sort.

**Rebuild path.** Two options that the benchmark proves are both
fast enough at N=10k:

1. **Working-copy walk** (5 s at N=10k). `jj edit bookmarks(issues)`
   materializes the bookmark, then `read_dir("issues/")` + parse.
   Then `jj new root()` to step `@` back off. **Drawback:** mutates
   `@` — risky to run while the operator may have a concurrent
   commit landing. The 4-CLI write dance does the same thing
   though, so it's already in jjforge's vocabulary.

2. **`jj file show` per id** (the current N-spawn pattern). 22 s at
   N=1000, 225 s at N=10k. Reject — this is the thing we're trying
   to fix.

3. **One batched `jj log`** with N `--files` filters returning all
   tree contents. The benchmark shows the *enumeration* (commit
   listing) at ~0.05 s for N=10k, but `jj log` doesn't return file
   contents — you'd still need a per-id `jj file show` after. Reject.

Pick option 1. The bookmark mutation footprint is fine: the read
path holds the cache rebuild lock (in-memory `Mutex` or a `.jj/
jjforge-cache.lock` advisory file), so two concurrent rebuilds
don't fight. A concurrent writer is the same situation we already
have today — jj's bookmark-set's fast-forward semantics handle the
race.

**Invalidation.** Writers don't need to know about the cache.
Every write moves the `issues` bookmark; the next read sees
`head != cache.head_commit` and rebuilds. We never INVALIDATE; we
PROBE on every read.

**Failure modes (cheap to handle).**

- Cache file missing → rebuild from scratch.
- Cache file corrupt JSON → log a warning, rebuild from scratch.
- Cache schema_version mismatch → rebuild from scratch.
- `.jj/` directory non-writable → fall back to in-memory cache for
  this process invocation (no persistence). Verbosely: one
  `info!("could not persist read cache: {e}")` line on stderr.

**The cache is .gitignored by construction.** `.jj/` is in the
project's gitignore (the merge-driver and jj's own state-bag both
live there). The cache file inherits.

**Self-host write guard.** The orchestrator's self-host write
guard (`Storage::open` refuses inside the source tree without
`JJF_ALLOW_SELF_HOST=1`) is on the WRITE path. The cache is on the
READ path. No interaction.

## 3. Index candidates — pick later, ship never (probably)

If somehow the cache rebuild becomes painful (say, the operator
has a 100k-issue codebase and rebuilds happen too often), the
fallback is a persistent index. We still do not pick one today.
But the candidate analysis pins the answer for the moment we need
to make the call.

**rusqlite with the `bundled` feature** is the right choice if we
ship an index.

Rationale:

- Our hot path is a **query**: "open issues, no open dep blocking
  them, sorted by type priority then `created_at`." SQLite's query
  planner is what we want. With redb we'd hand-roll one
  pre-computed BTree per access pattern (`by_status × by_dep_target ×
  by_type`) — that's just rebuilding SQLite's index machinery in
  Rust, badly.
- "C in the dep graph" via `rusqlite/bundled` is a cargo-only step
  — no system dep. The C compiles as part of the build like any
  proc-macro. Compare to redb (pure Rust, no FFI): cleaner, but for
  the wrong hot path.
- Migrations: in the index world we don't ALTER. The index is pure
  derived state. On schema bump we delete `.jj/jjforge-index.db`
  and rebuild from the cache or from the bookmark. SQLite's
  `ALTER TABLE` story doesn't matter; redb's "disk format not yet
  1.0" doesn't matter either; both win on the migration question
  by collapse.
- Maturity: SQLite is the most stable embedded DB ever shipped.
  redb is excellent and used by Trin in production, but at our
  read-throughput it's a wash and SQLite's tooling story (sqlite3
  CLI, every-language drivers, query analyzers) is unmatched if a
  future operator wants to peek.

**Reject** if `no C in the dep graph` becomes a hard constraint
later → ship redb instead, accept the manual per-query BTrees.

**Reject sled, heed, fjall** for the reasons in the ticket body:
sled has recovery bugs the maintainer admits; heed is a
key-value LMDB wrapper with worse ergonomics than redb for the
same outcome; fjall's LSM characteristics are aimed at workloads
ours isn't (high write throughput).

**Reject surrealdb, tantivy** as over-spec — graph queries and
full-text search are different problems.

## 4. Sketched index schema (if we ever build it)

```sql
CREATE TABLE issues (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL,
    slug       TEXT,
    status     TEXT NOT NULL,    -- 'open' | 'in_progress' | 'closed'
    type       TEXT,             -- 'bug' | 'feature' | ...
    body_sha   TEXT,
    assignee   TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_issues_slug_open
    ON issues(slug) WHERE status != 'closed' AND slug IS NOT NULL;
CREATE INDEX idx_issues_status_type_created
    ON issues(status, type, created_at);

CREATE TABLE issue_labels (
    id    TEXT NOT NULL,
    label TEXT NOT NULL,
    PRIMARY KEY (id, label)
);
CREATE INDEX idx_labels_label_id ON issue_labels(label, id);

CREATE TABLE issue_deps (
    id     TEXT NOT NULL,
    target TEXT NOT NULL,
    kind   TEXT NOT NULL,        -- 'blocks' | 'parent-child' | ...
    PRIMARY KEY (id, target, kind)
);
CREATE INDEX idx_deps_target ON issue_deps(target);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- meta: { 'head_commit': '<jj commit_id>',
--         'schema_version': '1' }
```

`jjf ready` becomes (roughly):

```sql
SELECT i.* FROM issues i
WHERE i.status = 'open'
  AND i.id NOT IN (
    SELECT d.id FROM issue_deps d
    JOIN issues t ON t.id = d.target
    WHERE d.kind = 'blocks' AND t.status != 'closed'
  )
ORDER BY
  CASE i.type
    WHEN 'bug' THEN 0
    WHEN 'feature' THEN 1
    WHEN 'research' THEN 2
    WHEN 'epic' THEN 3
    ELSE 4 END,
  i.created_at;
```

The fixpoint over `parent-child` cascade edges (per spec v2.4) is
the only piece the planner can't trivially express — that becomes
a small recursive CTE or a precomputed `blocked` column refreshed
on rebuild.

## 5. Rebuild path (index case)

If we ever ship the index, rebuild is the same shape as the cache:

1. On every read, probe `meta.head_commit` against the bookmark
   head.
2. Mismatch → wipe the DB (or `DELETE FROM` + reinsert in a
   transaction), repopulate from the working-copy walk.
3. Match → use the index.

Post-write hooks are tempting (write the change directly to the
index on every mutation, eliminate the probe) but introduce a
correctness surface we don't want: the moment a write lands on the
bookmark via another path (`jjf pull`, an interactive `jj` op
against `issues/`, a future merge driver fixup), the index goes
stale and we wouldn't notice. The probe-on-read story has no such
failure mode — the index either matches the bookmark or gets
rebuilt.

## 6. Recommendation

**Ship the snapshot cache. Do NOT ship a real index now.**

Per the measured numbers:

- Cache hit: ~15 ms at every size up through 10k.
- Cache rebuild (working-copy walk): 5 s at N=10k. Once per bookmark
  move.

A typical interactive session: one rebuild on the first `jjf` call
after a `jjf pull` or operator-side mutation, then cheap reads for
the rest of the session. An agent loop hitting `jjf ready` 50 times
between writes: 5 s rebuild + 50 × 15 ms ≈ 5.75 s total instead of
50 × 22 s = 18 minutes.

Pin the rusqlite index design here as a fallback. If the operator
runs into a workload where rebuild costs dominate — frequent
bookmark moves, very large N — we already know which library and
schema to reach for.

**Out of scope.**

- Implementing the cache. That's the follow-up ticket
  (`storage-snapshot-cache`).
- Implementing the index. We don't file an implementation ticket
  for the index; the design above is enough to act on later if
  needed.
- A `jjf bench` verb. Measurements live here, not in the CLI.
- Replacing the op chain as the source of truth. Cache and index
  are both pure derived state. Source of truth stays the
  `issues` bookmark.

## Pointers

- `experiments/scale-index/` — the bench binary and README.
- `crates/jjf-storage/src/lib.rs` — `Storage::list_ids`,
  `Storage::list_ready`, `Storage::resolve`, `Storage::read`.
- `crates/jjf-storage/src/read.rs` — `read::read` (the per-id read
  the loops call).
- `crates/jjf-storage/src/history.rs` — `read_history_at` (the
  per-issue `jj log` walker; debug-build cross-check only, not
  load-bearing on the release-build read hot path).
- `crates/jjf-storage/src/memory.rs` — `list_memories` /
  `read_memory_from_bookmark` follow the same one-spawn-per-key
  pattern; the cache covers them too.
- `experiments/jj-cli-overhead/README.md` — the underlying ~15 ms
  per `jj` invocation cost we're working around.
