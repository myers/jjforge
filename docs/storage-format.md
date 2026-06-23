# jjforge on-disk storage format — v2.2

Status: v2.2, current. This is the contract every other crate
implements against. Verdicts pinned by:

- `dcd4b57` — Shape A (dedicated bookmark for issue data).
- `a60bb95` — `Jjf-Op:` trailers in commit descriptions are the
  audit surface.
- `2130de1` — shell out to the `jj` CLI; do not link `jj-lib`.
- `72638a0` — the `mvp-storage` epic.

## v2.1 → v2.2 changelog

Backwards-compatible additions, landed in the `agent-remember`
ticket (`81db913`). v2.1 readers tolerate v2.2 commits (the
trailer parser drops unknown ops per §5.2; the per-issue
history walker drops trailer stanzas with no `Jjf-Issue:`
field); v2.2 readers tolerate v2.1 commits (no migration, no
new bookmark).

- **New on-bookmark file family `memories/<key>.json`** — one
  file per memory. Schema in §10: `{ "key", "value",
  "created_at", "updated_at" }`. The family lives on the
  `issues` bookmark next to `issues/<id>.json`, riding the
  same git transport (so `jjf push` / `jjf pull` carry
  memories automatically).
- **New op `set-memory`** — payload fields `Jjf-Memory-Key`,
  `Jjf-Memory-Value`. The trailer carries a single-lined,
  truncated preview of the value (≤200 chars); the on-disk
  `memories/<key>.json` holds the untouched full value.
- **New op `unset-memory`** — payload field `Jjf-Memory-Key`.
  Removes the on-disk file.
- **Memory op stanzas don't carry `Jjf-Issue:`** — they're
  global to the bookmark, not per-issue. The per-issue trailer
  parser (§5.6) drops them silently for any given issue's op
  chain.

## v2 → v2.1 changelog

Backwards-compatible field additions, landed in
`issue-type-and-slug-fields`. v2 readers tolerate v2.1 records
(the trailer parser ignores unknown ops per §5.2; the record
parser tolerates extra fields because serde-derive does); v2.1
readers tolerate v2 records via serde-default on the two new
fields. No bookmark rename, no path change, no migration commit.

- **New record field `type`** (string enum, default
  `"unspecified"`) — placed between `status` and `labels` in the
  on-disk record. Values: `bug`, `feature`, `epic`, `research`,
  `roadmap`, `unspecified`. Future-extensible (parsers tolerate
  unknown strings via the standard serde failure path; add the
  variant when the older reader meets newer data).
- **New record field `slug`** (string or null, default `null`)
  — placed between `title` and `body` in the on-disk record.
  Kebab-case orientation handle. Validation: `[a-z0-9-]+`, length
  3–48, no leading/trailing/consecutive hyphens. **Uniqueness
  enforced across OPEN issues at write time**; closing an issue
  releases its slug.
- **New op `set-type`** — payload field `Jjf-Type`. Carries one
  of the wire-spelling values above.
- **New op `set-slug`** — payload field `Jjf-Slug`. Empty value
  clears the slug.
- **Create-time emission (§5.7)** — non-default `type` and
  `slug` values emit `set-type` / `set-slug` stanzas in the
  multi-op create commit. Emission order matches record field
  order: slug, body, type, labels, dependencies, assignee.

## v1 → v2 changelog

This spec revs from v1 (which spelled the storage surface as
"bugs") to v2 ("issues"). The wire/disk changes are:

- **Bookmark name:** `bugs` → `issues`.
- **File paths:** `bugs/<id>.json` → `issues/<id>.json` and
  `bugs/<id>.comments.jsonl` → `issues/<id>.comments.jsonl`.
- **Trailer field:** `Jjf-Bug:` → `Jjf-Issue:` (every op
  stanza identifies its issue by this name).
- **JSON op field:** the `Op` enum's `bug_id` field is now
  `issue_id` (relevant only to programmatic callers that JSON-
  serialize a parsed `Op`).
- **Record `version`:** new records emit `version: 2`. The
  field's meaning is unchanged.
- **Seed commit description:** `jjf: seed bugs bookmark` →
  `jjf: seed issues bookmark`.

The terminology change is the rename of "bug" to "issue" — the
project's user-facing nomenclature has been "issue" since the
post-cutover blog post; v2 catches the on-disk artifacts up.
The substantive shape of every record, every commit, and every
op is unchanged. The merge semantics (§6) are unchanged.

### Forward compatibility

- The trailer parser MUST accept BOTH `Jjf-Issue:` and the
  legacy `Jjf-Bug:` spellings on read. Existing repos with v1
  commit chains continue to op-replay through v2 readers. The
  writer only emits the v2 form.
- The Rust storage layer (`jjf-storage`) detects a v1 repo on
  `Storage::open` / `Storage::init` (the `bugs` bookmark
  exists; `issues` does not) and runs an inline migration that
  renames every `bugs/<id>.*` → `issues/<id>.*` on a single
  commit, lands the new `issues` bookmark, and deletes the old
  `bugs` bookmark. The migration is idempotent — repos that
  have already migrated (or were created fresh at v2) pass
  through without changes.

Non-goals here: CLI surface (`docs/cli-json.md`), and any
implementation choices the writer wants — those are in the
storage crate.

---

## 1. Where issues live

Issues are files on a dedicated jj/git bookmark. The bookmark
name in v2 is **`issues`**. The bookmark lives in the same repo
as the project code, but is conceptually separate from `main` —
code merges to `main` are never blocked by a contended issue
edit (blast-radius argument from `dcd4b57`).

```
<repo>/
  issues/
    aa6600b.json            ← per-issue record
    aa6600b.comments.jsonl  ← per-issue comments (one JSON per line)
    bb7700c.json
    ...
```

Files live under `issues/` in the tree, on commits reachable
from `refs/heads/issues`. Stock git remotes (Forgejo, GitHub,
bare) serve this with no jj-aware infrastructure.

### 1.1 Seed commit

`jjf init` either:

- creates the `issues` bookmark pointing at a fresh empty commit
  whose description is `jjf: seed issues bookmark` (no trailer),
  if the bookmark doesn't exist; or
- leaves an existing `issues` bookmark alone.

The seed commit's only job is to exist so the bookmark has
somewhere to point before any issue is created. Issue commits
chain off it; the parent of the first issue-create commit is
the seed.

### 1.2 Path resolution

`jj` resolves file paths relative to **cwd**, not repo root, even
with `--repository` set. **Always use the `root:` fileset prefix**
when feeding paths to `jj log` / `jj diff`:

```sh
jj log --no-graph 'root:issues/aa6600b.json' -T 'json(self)'
```

This is a hard rule. Don't rely on cwd.

---

## 2. ID format

Issue ID = **7-character lowercase hex string**, drawn from
`/[0-9a-f]{7}/`. Mirrors git short-SHA convention so users already
read them fluently.

- Generation: random 28 bits (≈268M space). On collision with an
  existing `issues/<id>.json`, re-roll. Probability is negligible
  at jjforge's scale; v1 doesn't need anything fancier.
- Prefix disambiguation: like `git-bug` and `git`, jjforge
  commands accept any unambiguous prefix. A 4-char prefix is
  typically enough; ambiguous prefixes return an error listing
  all matches.
- The ID is **not** derived from content hashes or change_ids.
  Both are too long (40 chars) and change_ids are jj-specific.
  The simpler 7-hex random ID is friendlier for prose and CLI.

The ID is stamped into the file name (`issues/<id>.json`), into
the JSON record's `id` field, and into the `Jjf-Issue:` trailer
of every commit that touches it. All three must agree.

---

## 3. Per-issue record: `issues/<id>.json`

One JSON object per file, pretty-printed with 2-space indentation
and a trailing newline. Pretty-printing is deliberate: it makes
jj's textual auto-merger more useful (per-field edits land on
separate lines).

### 3.1 Schema

| Field          | Type                  | Req? | Notes                                                          |
| -------------- | --------------------- | ---- | -------------------------------------------------------------- |
| `version`      | integer               | yes  | Schema version. v2.1 = `2` (same wire value as v2).            |
| `id`           | string (7-hex)        | yes  | Must equal the filename stem.                                  |
| `title`        | string                | yes  | Single-line. Must not be empty.                                |
| `slug`         | string \| null        | yes  | v2.1 — kebab-case orientation handle. Default `null`. See §3.4. |
| `body`         | string                | yes  | Opening description. May be empty.                             |
| `status`       | string enum           | yes  | `open` or `closed`. Extensible by adding values in later vN.   |
| `type`         | string enum           | yes  | v2.1 — `bug` \| `feature` \| `epic` \| `research` \| `roadmap` \| `unspecified`. Default `unspecified`. |
| `labels`       | array of string       | yes  | Sorted alphabetically. Empty array if none.                    |
| `dependencies` | array of string       | yes  | Issue IDs this depends on. Sorted. Empty array if none.        |
| `assignee`     | string \| null        | yes  | Free-text identifier. `null` if unassigned.                    |
| `created_at`   | string (RFC 3339)     | yes  | UTC. Set at create time; never modified.                       |
| `updated_at`   | string (RFC 3339)     | yes  | UTC. Updated on every mutation.                                |

Drops from git-bug's model, on purpose:

- **No `actors` / `participants`** — derivable from commit/comment
  authors. Not stored.
- **No `nonce`** — issue ID is the identity.
- **No per-op identity blocks** — the commit's git author/email
  is the authority.

Adds over git-bug:

- **`version`** — git-bug's format version is repo-global; ours
  is per-record so we can migrate one issue at a time.

### 3.2 Example

`issues/aa6600b.json`:

```json
{
  "version": 2,
  "id": "aa6600b",
  "title": "segfault on empty input",
  "slug": "segfault-on-empty-input",
  "body": "Running `./app` with no arguments crashes.",
  "status": "open",
  "type": "bug",
  "labels": ["bug", "p1"],
  "dependencies": [],
  "assignee": "alice",
  "created_at": "2026-06-21T12:00:00Z",
  "updated_at": "2026-06-21T15:34:48Z"
}
```

(The label `"bug"` here is just a user-chosen string in the
free-form `labels` array — defect classification. It has nothing
to do with the v1 → v2 nomenclature rename.)

### 3.3 Field-ordering rule

Writers **must** emit fields in the order above. This is not for
parsers (any JSON parser ignores order) — it's for jj's textual
auto-merger and human review of diffs. Stable ordering avoids
spurious conflicts when two clones touch different fields.

### 3.4 Slug validation (v2.1)

A non-null `slug` field must satisfy:

- Charset: `[a-z0-9-]+` (lowercase ASCII alphanumerics and
  hyphen only).
- Length: 3 ≤ N ≤ 48 characters.
- No leading `-`.
- No trailing `-`.
- No two consecutive hyphens (`--`).

Slug uniqueness is enforced **across OPEN issues at write time**.
Closing an issue releases its slug — a subsequent `jjf new` /
`jjf update --slug` may take it. (Rationale: orientation handles
are for the live workspace; archived issues don't need to hold
the keyword space hostage.) Writers SHOULD pre-validate before
constructing a commit; storage MUST validate and reject on the
write boundary.

Operators look up issues by id OR slug — `jjf show
agent-ready` resolves the open issue whose slug is
`agent-ready`. The id-or-slug resolver scans every issue
(open and closed); only the uniqueness rule is open-only.

---

## 4. Comments file: `issues/<id>.comments.jsonl`

One JSON object per line. No surrounding array, no trailing
comma. Empty file = no comments. The file is **append-only** in
the normal write path; this keeps merge conflict surface to the
last line of the file.

### 4.1 Schema (one line)

| Field        | Type              | Req? | Notes                                       |
| ------------ | ----------------- | ---- | ------------------------------------------- |
| `id`         | string (7-hex)    | yes  | Comment ID, scoped per-issue. Locally unique. |
| `author`     | string            | yes  | Git author identity (`name <email>`).       |
| `created_at` | string (RFC 3339) | yes  | UTC.                                        |
| `body`       | string            | yes  | Markdown. May contain newlines (JSON-escaped). |

### 4.2 Ordering

Comments are ordered by `created_at` ascending. Writers append
new comments in that order; readers may re-sort defensively. If
two clones concurrently append, the merge driver (`e2e473b`)
unions both lines and re-sorts.

### 4.3 Example

`issues/aa6600b.comments.jsonl`:

```jsonl
{"id":"c01a23b","author":"alice <alice@example.com>","created_at":"2026-06-21T13:00:00Z","body":"I can reproduce on macOS 14.5."}
{"id":"c02b44c","author":"bob <bob@example.com>","created_at":"2026-06-21T14:15:00Z","body":"Fix in PR #42.\n\nNeed review."}
```

(Single-line JSON per record; the wrap in the second comment's
`body` is `\n\n` inside the JSON string, not a real newline.)

### 4.4 Why JSONL instead of an array in the main record?

Two reasons:

1. **Merge surface.** A trailing-line append rarely collides
   with another trailing-line append textually. Embedding
   comments inside the record forces every comment-add to rewrite
   the whole array, which conflicts with every other field edit.
2. **Streaming.** Long comment threads can be tailed without
   parsing the whole record.

---

## 5. `Jjf-Op:` commit trailer

Every commit on the `issues` bookmark that mutates an issue
carries **one or more `Jjf-Op:` trailers** in its description.
The trailers are git-trailer-style (`Key: value` lines at the
bottom of the message, after a blank line). Git trailers survive
`jj describe` reflow and round-trip through `jj log -T description`
cleanly — this is why we chose them over JSON-on-first-line.

### 5.1 Commit-message shape

```
jjf: <human summary>

<optional free-text body>

Jjf-Op: <op-type>
Jjf-At: <rfc3339-nano>
Jjf-Issue: <issue-id>
Jjf-...: <payload field>
[Jjf-Op: <second-op-type>  ← multi-op-per-commit supported]
[Jjf-At: <rfc3339-nano>]
[Jjf-Issue: <issue-id>]
[Jjf-...: ...]
```

**`Jjf-At:` is required on every emitted stanza.** It carries an
RFC 3339 timestamp with nanosecond precision (`%Y-%m-%dT%H:%M:%S
%.9fZ`), UTC, stamped by the writer at the moment of the op. The
field appears once per stanza, between the `Jjf-Op:` line and the
payload trailers. Multiple stanzas in the same commit share the
same `Jjf-At:` value (they were issued together, see
`build_commit_message`); the trailer-index tiebreaker in §6
separates them when needed.

**Parsers MUST tolerate stanzas without `Jjf-At:`** — pre-spec-bump
trailers, hand-written fixtures, and any other forward-compat
data return `None` for the field. The §6 ordering tuple sorts
unstamped stanzas before stamped ones at the same commit-time
second, which is the desired migration semantics (older data
loses to newer data when they tie on commit-time).

**Parsers MUST also tolerate the v1 spelling `Jjf-Bug:`** in
place of `Jjf-Issue:`. The two field names carry identical
semantics — the parser maps either to the same op. When a stanza
carries both (defensive; should never happen), the v2 name
(`Jjf-Issue:`) takes precedence.

Why nanos in the trailer when the JSON record's `created_at` /
`updated_at` are second-resolution (§3.1)? Because the byte-equality
round-trip property test on the JSON record is load-bearing for
the storage layer, and bumping the record to nano-resolution
re-opens that contract. The trailer is a fresh surface — adding
nanos there is free. It also subsumes the same-second-collision
trap that §5.6's filter-on-both-files workaround papers over.

### 5.2 Op-type vocabulary

| Op-type      | Trailer-payload fields (in addition to `Jjf-Issue`)       | Notes                                                |
| ------------ | --------------------------------------------------------- | ---------------------------------------------------- |
| `create`     | `Jjf-Title`, `Jjf-Status` (always `open`)                 | Must be the first op on this issue.                  |
| `set-title`  | `Jjf-Title`                                               | Replaces title outright.                             |
| `set-status` | `Jjf-Status` (`open` \| `closed`)                         | Replaces status outright.                            |
| `set-body`   | `Jjf-Body-Hash` (sha-256 of new body, hex)                | Body itself lives in the JSON; trailer carries hash. |
| `label-add`  | `Jjf-Label` (one label string; may repeat for >1 label)   | No-op if label already present.                      |
| `label-rm`   | `Jjf-Label`                                               | No-op if not present.                                |
| `dep-add`    | `Jjf-Dep` (target issue-id)                               |                                                      |
| `dep-rm`     | `Jjf-Dep`                                                 |                                                      |
| `set-assignee` | `Jjf-Assignee` (string or empty for unassign)           |                                                      |
| `set-type`   | `Jjf-Type` (one of `bug` / `feature` / `epic` / `research` / `roadmap` / `unspecified`) | v2.1.                              |
| `set-slug`   | `Jjf-Slug` (validated kebab-case per §3.4; empty clears) | v2.1.                                                |
| `comment-add` | `Jjf-Comment-Id` (the new comment's 7-hex id)            | The comment body lives in `<id>.comments.jsonl`.     |
| `merge`      | (no extra payload fields)                                 | Used by the merge driver in `e2e473b`.               |
| `set-memory` | `Jjf-Memory-Key`, `Jjf-Memory-Value` (single-line, ≤200 chars; full value in `memories/<key>.json`) | v2.2. **No `Jjf-Issue:`** — global to the bookmark.    |
| `unset-memory` | `Jjf-Memory-Key`                                        | v2.2. **No `Jjf-Issue:`**.                            |

Unknown trailers and unknown op-types **must be tolerated** by
readers — they get logged in the audit view as
`unknown(<op-type>)` but don't fail the read. This lets us add
ops in v2.1 without breaking older readers.

### 5.3 Multi-op-per-commit ordering

A commit may carry more than one `Jjf-Op:` trailer (e.g. when a
single `jjf` invocation closes an issue *and* adds a label). Ops
in the same commit are applied **top-to-bottom in trailer order**.
The `Jjf-Issue:` immediately following a `Jjf-Op:` (and any
payload fields up to the next `Jjf-Op:` or end-of-message) belong
to that op.

Each op's payload window is delimited by the next `Jjf-Op:` line
or end-of-trailers. Implementers: split on `Jjf-Op:`, parse each
chunk independently.

### 5.4 Example: single-op commit

```
jjf: issue aa6600b - create

Jjf-Op: create
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: aa6600b
Jjf-Title: segfault on empty input
Jjf-Status: open
```

### 5.5 Example: multi-op commit

Both ops share the same `Jjf-At:` — they were issued together.

```
jjf: issue aa6600b - close + label

Closing as fixed in #42.

Jjf-Op: set-status
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: aa6600b
Jjf-Status: closed
Jjf-Op: label-add
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: aa6600b
Jjf-Label: fixed
```

Applied in order: status → closed, then label `fixed` added.

### 5.6 Reading the per-issue op chain

```sh
jj log --no-graph \
   -r 'ancestors(bookmarks(issues))' \
   'root:issues/aa6600b.json' \
   'root:issues/aa6600b.comments.jsonl' \
   -T 'change_id.short() ++ "\t" ++ json(description) ++ "\n"'
```

Returns one row per mutating commit, newest-first. The reader
parses the `Jjf-Op:` trailers out of each description to build
the typed audit view (git-bug-equivalent `CreateOp` /
`SetTitleOp` / ... chain). The audit IS the commit chain — no
side jsonl.

**Filter on both files**, not just the JSON record. If two
mutations land within the same second, the JSON record's
`updated_at` is byte-identical between commits and jj's
snapshotter records no change to that file — a JSON-only filter
silently drops the second commit. The comments-jsonl path picks
those up because every comment-add appends a new line. (The
nanosecond-resolution `Jjf-At:` trailer added in this section
makes the same-second collision case observationally rare, but
not impossible — the workaround stays in place as a belt-and-
braces guard against future regressions.)

**Anchor the revset to `ancestors(bookmarks(issues))`.** Without
the explicit revset, `jj log` defaults to a working-copy
revision that doesn't include the bookmark's history once the
4-CLI dance has stepped `@` off the bookmark.

### 5.7 Merge commits

When `jjf pull` resolves a divergent `issues` bookmark via the
merge driver, it lands ONE multi-parent merge commit on `issues`
whose description carries one `Jjf-Op: merge` trailer per resolved
issue (spec §5.2 + §5.5). The merge commit's parents are the
heads that were diverging; the trailer payload is just the
issue-id (`Jjf-Issue: <id>`), no extra fields — the resolved file
diff IS the payload. Multi-issue merges land all `merge` trailers
on the same commit.

Example single-issue merge commit:

```
jjf: issue aa6600b - merge

Jjf-Op: merge
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: aa6600b
```

Example two-issue merge commit (both ops share the same `Jjf-At:`
— they were issued together):

```
jjf: merge 2 issues

Jjf-Op: merge
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: aa6600b
Jjf-Op: merge
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Issue: bb7700c
```

The merge commit's file diff IS the resolution, and that
resolution is now produced by the op-space resolver (§6) rather
than the legacy file-bytes merge driver. The `merge` op stays a
payload-free marker — replay readers fold each parent's op chain
and the merge commit announces "here's the projection of those
chains together," not a per-field decision.

### 5.8 Create-time fields and op chains

The `create` op trailer carries only `Jjf-Title` and
`Jjf-Status`. Any other seed fields on a freshly-created issue
(`body`, `labels`, `dependencies`, `assignee`) must be recorded
as **additional ops in the same commit** — the multi-op pattern
of §5.5. Without this, a reader that re-derives state from the
op chain would miss those fields entirely; the v1-contract
correctness gate (file-read vs. op-replay equality) would fire
on every non-trivial create.

The writer emits them in this order, after the `Jjf-Op: create`
stanza: `set-slug` (if non-null), `set-body`, `set-type` (if
non-default), `label-add` (one per label, sorted), `dep-add` (one
per dependency, sorted), `set-assignee` (if present). Order
follows the record's field-declaration order (§3.1) so the
op-replay view's structural fold matches the file-read view
exactly.

---

## 6. Merge semantics

When a divergent `issues` bookmark needs to converge — two clones
both pushed concurrent edits, or any two heads exist under
`heads(bookmarks(issues))` — the op-space resolver in
`crates/jjf-storage/src/merge_ops.rs` reduces both heads' op
chains to a single rendered state. **The file is a deterministic
projection of the op chain.** Divergence resolves in op-space;
the rendered `issues/<id>.json` and `issues/<id>.comments.jsonl`
fall out as the projection of the merged op stream.

### 6.1 LWW ordering tuple

Every op across both heads sorts by

    (jjf_at if Some else commit_time, commit, trailer_index)

- **`jjf_at`** is the writer's `now_rfc3339_nanos()` stamp from
  the `Jjf-At:` trailer (§5.1). Nanosecond precision; total order
  within a single writer's process.
- **`commit_time`** is jj's `author.timestamp()`, second
  resolution. Fallback for stanzas predating the spec §5 op-time
  bump. Unstamped stanzas sort *before* stamped stanzas at the
  same commit-time second — older data loses to newer data, the
  desired migration semantics.
- **`commit`** is the full-hex commit_id. Deterministic across
  clones; the second-level tiebreaker.
- **`trailer_index`** is the 0-based stanza position within its
  commit. Every multi-op commit has at least two stanzas
  distinguishable only by this index; it's the final tiebreaker.

This tuple is total over every pair of ops the resolver will see,
so the merged state is deterministic regardless of which clone
runs the merge.

### 6.2 Field-by-field reducer

| Op | Reduction rule |
| --- | --- |
| `create` | Earliest in the sorted stream initializes the record (title, status). Predates the fork, so should agree across heads in practice. |
| `set-title`, `set-status`, `set-assignee`, `set-body` | LWW: the last op in the sorted stream wins. |
| `label-add`, `label-rm`, `dep-add`, `dep-rm` | Causal order: each add/remove tracked per (label/dep); final state is `present` iff the last write for that key was an add. |
| `comment-add` | Union of all `comment_id`s across both heads. Comments are append-only; no conflict possible. |
| `merge` | Marker; no-op for state reconstruction. The parent chains are the truth. |

### 6.3 Body-hash join

`Op::SetBody` carries only `body_hash` (§5.2). The reducer picks
the winning hash from the ordering tuple, but the body bytes
themselves live in the rendered `issues/<id>.json`, not in any
trailer. To recover the body text:

1. Pick the winning `set-body` op's `body_hash` from the sorted
   stream.
2. Look up that hash in each head's rendered `issues/<id>.json`
   (compute `sha-256(body)` on the JSON record's `body` field for
   each head).
3. The hash will match exactly one head by construction — that
   head's chain is the one whose latest `set-body` op was the
   winner. (Both heads might match if they shared the body op;
   the bytes are byte-identical either way.)
4. Take the body bytes from the matching head.

This is the join between op-space (where LWW decides which body
*op* won) and bytes (where the actual content lives). It's what
lets the resolver keep the file as a projection without
duplicating the body text in every trailer.

### 6.4 Comment union

Each `comment-add` op references a `comment_id`; the actual
comment body lives in `issues/<id>.comments.jsonl`. The resolver
reads each head's `.comments.jsonl` (via `jj file show -r
<head>`), unions them by `comment_id`, and re-renders the merged
file in `created_at` ascending order (§4.2). Same-id-different-body
collisions are a spec violation (the writer always appends the
body alongside the `comment-add` commit) and the resolver drops
silently rather than failing — there's no operator action that
could fix the underlying data.

### 6.5 What this replaces

The v1 file-bytes merge driver (`jjf-merge`) reads jj's textual
conflict markers and runs a JSON-level LWW/union policy on the
record bytes. It has a real "unmergeable" failure mode when body
text collides; `jjf pull` would exit with a human-resolution
escape hatch. The op-space resolver has no such failure mode:
`set-body` is just another LWW scalar in §6.2, and the file
falls out as a projection. `jjf-merge` stays in the workspace
as a library for non-operator callers and as a parser-behavior
fixture; `jjf pull` no longer wires it in.

---

## 7. Write path summary (informative)

The exact write path is in `crates/jjf-storage`, but the format
constrains it. The 4-CLI dance (jj 0.40–0.42 has no `file write
-r <change>`):

1. `jj new bookmarks(issues) -m '<msg with trailers>'`
2. Edit `issues/<id>.json` (and append to
   `issues/<id>.comments.jsonl` if applicable) in the working
   copy.
3. `jj bookmark set issues -r @ --allow-backwards`
4. `jj new root()` to step @ off the bookmark so the next
   mutation doesn't snapshot stale files.

Cost ≈60ms per mutation at jj's measured ~15ms/CLI call
(`2130de1`), which is acceptable for `jjf`.

---

## 10. Persistent memories (v2.2)

Memories are short declarative facts (operational rules,
codebase folklore, architectural decisions) keyed by a
kebab-case slug. They ride the `issues` bookmark like
per-issue records do — `jjf push` / `jjf pull` carry them
automatically — but they're global to the bookmark, not
scoped to any one issue.

### 10.1 File family

```
<repo>/
  memories/
    dolt-phantoms.json
    auth-jwt.json
    ...
```

One file per memory, named by its kebab-case key. The
directory lives at the repo root next to `issues/`. Empty
directory (no memories yet) is the steady state.

### 10.2 Record schema

```json
{
  "key": "dolt-phantoms",
  "value": "Dolt phantom DBs hide in three places",
  "created_at": "2026-06-23T01:23:45Z",
  "updated_at": "2026-06-23T01:23:45Z"
}
```

- `key`: kebab-case slug, validated per spec §3.4's slug rules
  (`[a-z0-9-]+`, length 3–48, no leading/trailing/consecutive
  hyphens). The key in the record agrees with the file name.
- `value`: the free-text insight. Newlines preserved. No
  length limit at the storage layer.
- `created_at`, `updated_at`: RFC 3339 second resolution, per
  spec §3.1.

The writer emits fields in this declaration order; readers
parse via serde and tolerate field reordering for forward
compatibility.

### 10.3 Op vocabulary

Two ops, both on the `issues` bookmark, both single-stanza
single-op commits (no multi-op-per-commit yet — operator path
is "one memory at a time").

```
jjf: memory dolt-phantoms - set

Jjf-Op: set-memory
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Memory-Key: dolt-phantoms
Jjf-Memory-Value: Dolt phantom DBs hide in three places
```

```
jjf: memory dolt-phantoms - unset

Jjf-Op: unset-memory
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Memory-Key: dolt-phantoms
```

The `Jjf-Memory-Value:` trailer carries a **single-lined,
truncated** preview of the value (newlines → spaces, capped
at 200 chars with a `...` suffix on truncation). The
authoritative bytes live in `memories/<key>.json`; the
trailer is for human-readable audit only.

**No `Jjf-Issue:`** on memory op stanzas. The per-issue
trailer parser drops these stanzas silently for any specific
issue's op chain — see `crate::trailer::stanza_to_op`.

### 10.4 Upsert semantics

`set-memory` is upsert by key: if `memories/<key>.json`
already exists, the writer rewrites the file (bumping
`updated_at` but preserving `created_at`) and lands a new
commit. The audit chain accumulates one `set-memory` op per
write — no dedupe.

### 10.5 Slugification

When the operator writes a memory without an explicit `--key`,
the CLI derives one from the value via `jjf_storage::slugify`:
lowercase, non-alphanumeric runs collapse to a single `-`,
trim leading/trailing `-`, take the first ~8 hyphen-separated
tokens, cap at 60 chars. Empty result (no alphanumerics in the
input) surfaces as a typed error pointing at `--key`. Port of
beads' `slugify()` from `reference/beads/cmd/bd/memory.go:23-44`.

### 10.6 Merge semantics

Memories are independent files per key, so jj's textual
auto-merger handles the common cases for free: disjoint keys
land cleanly; same key with the same bytes is a no-op merge.
Same key with divergent bytes does conflict at the file level
— the op-space resolver in §6 doesn't currently fold memory
ops, so the user resolves textually (or runs `jjf remember
--key <k> "<final value>"` to pin the winner). Op-space
memory resolution is a separate ticket if usage shows the
manual path is friction.

---

## 8. What's deliberately out of scope for v2

- **Attachments / binary blobs.** No `files` array (git-bug
  uses git blob refs; we don't need it yet).
- **Edit-comment / delete-comment.** Append-only.
- **Identity / signatures.** Git author/email is enough; PGP
  signing is a later issue.
- **Multi-bookmark / multi-project sharding.** One `issues`
  bookmark per repo.
- **Schema-level format migrations** beyond the v1→v2 inline
  rename. Once we ship a v3, the `version` field on each record
  drives a per-record migration. Not yet needed.

---

## 9. References

- `dcd4b57` — Shape A verdict (bookmark choice + blast-radius).
- `a60bb95` — `Jjf-Op:` trailer verdict (audit shape).
- `2130de1` — Shell-out verdict (we don't link `jj-lib`).
- `72638a0` — `mvp-storage` epic.
- `e2e473b` — Merge driver, which consumes this format.
- `experiments/jj-shellout-hello/src/main.rs` — round-trip
  proof of the trailer + `jj log <path>` shape.
- `experiments/storage-shape/runs/shape-a.transcript.txt` —
  distributed-edit transcript for Shape A.
