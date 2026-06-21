# jjforge on-disk storage format — v1

Status: v1, draft for `mvp-storage`. This is the contract every
other `mvp-storage` and `mvp-sync` ticket implements against.
Verdicts pinned by:

- `dcd4b57` — Shape A (dedicated `bugs` bookmark).
- `a60bb95` — `Jjf-Op:` trailers in commit descriptions are the
  audit surface.
- `2130de1` — shell out to the `jj` CLI; do not link `jj-lib`.
- `72638a0` — the `mvp-storage` epic that lists this spec as a
  ticket.

Non-goals here: CLI surface (that's `mvp-cli`'s territory),
sync/merge resolution (that's `e2e473b`), and any implementation
choices the writer wants — those land in `storage-write` and
peers.

---

## 1. Where bugs live

Bugs are files on a dedicated jj/git bookmark. The bookmark name
in v1 is **`bugs`**. The bookmark lives in the same repo as the
project code, but is conceptually separate from `main` — code
merges to `main` are never blocked by a contended bug edit
(blast-radius argument from `dcd4b57`).

```
<repo>/
  bugs/
    aa6600b.json            ← per-bug record
    aa6600b.comments.jsonl  ← per-bug comments (one JSON per line)
    bb7700c.json
    ...
```

Files live under `bugs/` in the tree, on commits reachable from
`refs/heads/bugs`. Stock git remotes (Forgejo, GitHub, bare)
serve this with no jj-aware infrastructure.

### 1.1 Seed commit

`jjf init` either:

- creates the `bugs` bookmark pointing at a fresh empty commit
  whose description is `jjf: seed bugs bookmark` (no trailer), if
  the bookmark doesn't exist; or
- leaves an existing `bugs` bookmark alone.

The seed commit's only job is to exist so the bookmark has
somewhere to point before any bug is created. Bug commits chain
off it; the parent of the first bug-create commit is the seed.

### 1.2 Path resolution

`jj` resolves file paths relative to **cwd**, not repo root, even
with `--repository` set. **Always use the `root:` fileset prefix**
when feeding paths to `jj log` / `jj diff`:

```sh
jj log --no-graph 'root:bugs/aa6600b.json' -T 'json(self)'
```

This is a hard rule. Don't rely on cwd.

---

## 2. ID format

Bug ID = **7-character lowercase hex string**, drawn from
`/[0-9a-f]{7}/`. Mirrors git short-SHA convention so users already
read them fluently.

- Generation: random 28 bits (≈268M space). On collision with an
  existing `bugs/<id>.json`, re-roll. Probability is negligible
  at jjforge's scale; v1 doesn't need anything fancier.
- Prefix disambiguation: like `git-bug` and `git`, jjforge
  commands accept any unambiguous prefix. A 4-char prefix is
  typically enough; ambiguous prefixes return an error listing
  all matches.
- The ID is **not** derived from content hashes or change_ids.
  Both are too long (40 chars) and change_ids are jj-specific.
  The simpler 7-hex random ID is friendlier for prose and CLI.

The ID is stamped into the file name (`bugs/<id>.json`), into the
JSON record's `id` field, and into the `Jjf-Bug:` trailer of every
commit that touches it. All three must agree.

---

## 3. Per-bug record: `bugs/<id>.json`

One JSON object per file, pretty-printed with 2-space indentation
and a trailing newline. Pretty-printing is deliberate: it makes
jj's textual auto-merger more useful (per-field edits land on
separate lines).

### 3.1 Schema

| Field          | Type                  | Req? | Notes                                                          |
| -------------- | --------------------- | ---- | -------------------------------------------------------------- |
| `version`      | integer               | yes  | Schema version. v1 = `1`.                                      |
| `id`           | string (7-hex)        | yes  | Must equal the filename stem.                                  |
| `title`        | string                | yes  | Single-line. Must not be empty.                                |
| `body`         | string                | yes  | Opening description. May be empty.                             |
| `status`       | string enum           | yes  | `open` or `closed`. Extensible by adding values in later vN.   |
| `labels`       | array of string       | yes  | Sorted alphabetically. Empty array if none.                    |
| `dependencies` | array of string       | yes  | Bug IDs this depends on. Sorted. Empty array if none.          |
| `assignee`     | string \| null        | yes  | Free-text identifier. `null` if unassigned.                    |
| `created_at`   | string (RFC 3339)     | yes  | UTC. Set at create time; never modified.                       |
| `updated_at`   | string (RFC 3339)     | yes  | UTC. Updated on every mutation.                                |

Drops from git-bug's model, on purpose:

- **No `actors` / `participants`** — derivable from commit/comment
  authors. Not stored.
- **No `nonce`** — bug ID is the identity.
- **No per-op identity blocks** — the commit's git author/email
  is the authority.

Adds over git-bug:

- **`version`** — git-bug's format version is repo-global; ours
  is per-record so we can migrate one bug at a time.

### 3.2 Example

`bugs/aa6600b.json`:

```json
{
  "version": 1,
  "id": "aa6600b",
  "title": "segfault on empty input",
  "body": "Running `./app` with no arguments crashes.",
  "status": "open",
  "labels": ["bug", "p1"],
  "dependencies": [],
  "assignee": "alice",
  "created_at": "2026-06-21T12:00:00Z",
  "updated_at": "2026-06-21T15:34:48Z"
}
```

### 3.3 Field-ordering rule

Writers **must** emit fields in the order above. This is not for
parsers (any JSON parser ignores order) — it's for jj's textual
auto-merger and human review of diffs. Stable ordering avoids
spurious conflicts when two clones touch different fields.

---

## 4. Comments file: `bugs/<id>.comments.jsonl`

One JSON object per line. No surrounding array, no trailing
comma. Empty file = no comments. The file is **append-only** in
the normal write path; this keeps merge conflict surface to the
last line of the file.

### 4.1 Schema (one line)

| Field        | Type              | Req? | Notes                                       |
| ------------ | ----------------- | ---- | ------------------------------------------- |
| `id`         | string (7-hex)    | yes  | Comment ID, scoped per-bug. Locally unique. |
| `author`     | string            | yes  | Git author identity (`name <email>`).       |
| `created_at` | string (RFC 3339) | yes  | UTC.                                        |
| `body`       | string            | yes  | Markdown. May contain newlines (JSON-escaped). |

### 4.2 Ordering

Comments are ordered by `created_at` ascending. Writers append
new comments in that order; readers may re-sort defensively. If
two clones concurrently append, the merge driver (`e2e473b`)
unions both lines and re-sorts.

### 4.3 Example

`bugs/aa6600b.comments.jsonl`:

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

Every commit on the `bugs` bookmark that mutates a bug carries
**one or more `Jjf-Op:` trailers** in its description. The
trailers are git-trailer-style (`Key: value` lines at the bottom
of the message, after a blank line). Git trailers survive `jj
describe` reflow and round-trip through `jj log -T description`
cleanly — this is why we chose them over JSON-on-first-line.

### 5.1 Commit-message shape

```
jjf: <human summary>

<optional free-text body>

Jjf-Op: <op-type>
Jjf-Bug: <bug-id>
Jjf-...: <payload field>
[Jjf-Op: <second-op-type>  ← multi-op-per-commit supported]
[Jjf-Bug: <bug-id>]
[Jjf-...: ...]
```

### 5.2 Op-type vocabulary

| Op-type      | Trailer-payload fields (in addition to `Jjf-Bug`)         | Notes                                                |
| ------------ | --------------------------------------------------------- | ---------------------------------------------------- |
| `create`     | `Jjf-Title`, `Jjf-Status` (always `open`)                 | Must be the first op on this bug.                    |
| `set-title`  | `Jjf-Title`                                               | Replaces title outright.                             |
| `set-status` | `Jjf-Status` (`open` \| `closed`)                         | Replaces status outright.                            |
| `set-body`   | `Jjf-Body-Hash` (sha-256 of new body, hex)                | Body itself lives in the JSON; trailer carries hash. |
| `label-add`  | `Jjf-Label` (one label string; may repeat for >1 label)   | No-op if label already present.                      |
| `label-rm`   | `Jjf-Label`                                               | No-op if not present.                                |
| `dep-add`    | `Jjf-Dep` (target bug-id)                                 |                                                      |
| `dep-rm`     | `Jjf-Dep`                                                 |                                                      |
| `set-assignee` | `Jjf-Assignee` (string or empty for unassign)           |                                                      |
| `comment-add` | `Jjf-Comment-Id` (the new comment's 7-hex id)            | The comment body lives in `<id>.comments.jsonl`.     |
| `merge`      | (no extra payload fields)                                 | Used by the merge driver in `e2e473b`.               |

Unknown trailers and unknown op-types **must be tolerated** by
readers — they get logged in the audit view as
`unknown(<op-type>)` but don't fail the read. This lets us add
ops in v1.1 without breaking older readers.

### 5.3 Multi-op-per-commit ordering

A commit may carry more than one `Jjf-Op:` trailer (e.g. when a
single `jjf` invocation closes a bug *and* adds a label). Ops in
the same commit are applied **top-to-bottom in trailer order**.
The `Jjf-Bug:` immediately following a `Jjf-Op:` (and any payload
fields up to the next `Jjf-Op:` or end-of-message) belong to that
op.

Each op's payload window is delimited by the next `Jjf-Op:` line
or end-of-trailers. Implementers: split on `Jjf-Op:`, parse each
chunk independently.

### 5.4 Example: single-op commit

```
jjf: bug aa6600b - create

Jjf-Op: create
Jjf-Bug: aa6600b
Jjf-Title: segfault on empty input
Jjf-Status: open
```

### 5.5 Example: multi-op commit

```
jjf: bug aa6600b - close + label

Closing as fixed in #42.

Jjf-Op: set-status
Jjf-Bug: aa6600b
Jjf-Status: closed
Jjf-Op: label-add
Jjf-Bug: aa6600b
Jjf-Label: fixed
```

Applied in order: status → closed, then label `fixed` added.

### 5.6 Reading the per-bug op chain

```sh
jj log --no-graph \
   'root:bugs/aa6600b.json' \
   -T 'change_id.short() ++ "\t" ++ json(description) ++ "\n"'
```

Returns one row per mutating commit, newest-first. The reader
parses the `Jjf-Op:` trailers out of each description to build
the typed audit view (git-bug-equivalent `CreateOp` /
`SetTitleOp` / ... chain). The audit IS the commit chain — no
side jsonl.

---

## 6. Write path summary (informative)

The exact write path is `storage-write`'s ticket, but the format
constrains it. The 4-CLI dance (jj 0.40–0.42 has no `file write
-r <change>`):

1. `jj new bookmarks(bugs) -m '<msg with trailers>'`
2. Edit `bugs/<id>.json` (and append to `bugs/<id>.comments.jsonl`
   if applicable) in the working copy.
3. `jj bookmark set bugs -r @ --allow-backwards`
4. `jj new root()` to step @ off the bookmark so the next
   mutation doesn't snapshot stale files.

Cost ≈60ms per mutation at jj's measured ~15ms/CLI call
(`2130de1`), which is acceptable for `jjf`.

---

## 7. What's deliberately out of scope for v1

- **Attachments / binary blobs.** No `files` array (git-bug
  uses git blob refs; we don't need it yet).
- **Edit-comment / delete-comment.** Append-only in v1.
- **Identity / signatures.** Git author/email is enough; PGP
  signing is a later issue.
- **Multi-bookmark / multi-project sharding.** One `bugs`
  bookmark per repo.
- **Format migrations.** Once we ship a v2, the `version` field
  on each record drives a per-record migration. Not yet needed.

---

## 8. References

- `dcd4b57` — Shape A verdict (bookmark choice + blast-radius).
- `a60bb95` — `Jjf-Op:` trailer verdict (audit shape).
- `2130de1` — Shell-out verdict (we don't link `jj-lib`).
- `72638a0` — `mvp-storage` epic.
- `e2e473b` — Merge driver, which consumes this format.
- `experiments/jj-shellout-hello/src/main.rs` — round-trip
  proof of the trailer + `jj log <path>` shape.
- `experiments/storage-shape/runs/shape-a.transcript.txt` —
  distributed-edit transcript for Shape A.
