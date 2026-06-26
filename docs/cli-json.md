# `jjf --json` output contract

## v2.7 → v2.8 changelog

Backwards-compatible additions, landed in the `abandon-verb`
ticket (`c1ffea7`). See `docs/storage-format.md` v2.6 → v2.7
for the storage-layer contract.

- **New verb `jjf abandon <id>`** — soft-delete an issue.
  Lands one `set-status` op with payload
  `Jjf-Status: abandoned` on a new commit. Same envelope shape
  and non-idempotency rules as `jjf close`:

  ```json
  {"ok": true, "id": "aa6600b", "status": "abandoned"}
  ```

  Plain-text output: `abandoned <id>` — one line, no
  decoration. Re-abandoning is idempotent at the data level
  (status stays `abandoned`) but each call lands a fresh
  trailer so the audit log records intent (matching `close`'s
  contract).

- **New status enum value `abandoned`** on every verb that emits
  or accepts status. `Status::Abandoned` is wire-spelled
  `abandoned`.

  - `jjf show <id>` (and `--json`) reports `[abandoned]` /
    `"status": "abandoned"` for abandoned issues — show still
    works, so an operator can confirm what was abandoned.
  - `jjf ls --status abandoned` lists only abandoned issues.
  - `jjf ls --status all` includes them alongside everything
    else.
  - `jjf ls` (default `--status open`) HIDES them — abandoned
    is meant as a soft delete; the listing should stay clean.
  - `jjf update <id> --status abandoned` is the same write as
    `jjf abandon <id>`; `--status open` on an abandoned issue
    is the documented revival path (no `jjf unabandon` inverse
    verb).

- **`jjf ready` EXCLUDES abandoned issues UNCONDITIONALLY.** No
  `--include-abandoned` flag (unlike `--include-blocked` /
  `--include-claimed`). Abandoning means "never come up again."
  Dep targets in the abandoned state behave like closed deps —
  they don't block dependents.

- **No new error kinds.** `claim` / `block` / `unblock` on an
  abandoned issue surface the existing `invalid_input` kind
  with a "reopen before …" / "nothing to …" hint, matching the
  closed-issue rejection shape. `slug_collision` triggers on
  attempts to reuse an abandoned issue's slug — the existing
  v2.6 rule that slug uniqueness spans every status holds.

## v2.6 → v2.7 changelog

Backwards-compatible additions, landed in the
`corrupt-ref-silent-drop` ticket (`4928ae6`).

- **`jjf ls` and `jjf ready` now emit a ref-corruption warning
  on stderr.** When the snapshot-cache rebuild encounters one
  or more refs under `refs/jjf/issues/*` or `refs/jjf/memories/*`
  that don't resolve to a commit carrying the expected blob
  (e.g. a ref pointed at a blob by `git update-ref`, a commit
  whose tree is empty, or a `serde_json` parse failure on the
  record), the affected refs are dropped from the result set
  but listed in a stderr warning. Exit status is still 0; the
  warning is informational. Before this change the corrupt
  ids vanished from `ls` / `ready` with no diagnostic
  (indistinguishable from "issue genuinely doesn't exist").
- **Plain-text stderr shape:**
  ```text
  jjf: warning: 2 ref(s) unreadable: refs/jjf/issues/eed62d7, refs/jjf/memories/foo (skipped from listing)
  ```
  Capped at 5 ref names inline; beyond that the tail elides
  with `, ... and N more`. The warning is one line so it
  composes cleanly with operator log capture.
- **JSON stderr shape:** one single-line JSON envelope per
  call, regardless of how many refs are unreadable:
  ```json
  {"warning":"unreadable_refs","count":2,"refs":["refs/jjf/issues/eed62d7","refs/jjf/memories/foo"]}
  ```
  The `refs` array always carries the full list (no inline
  cap) under `--json` — machine consumers don't need elision.
- **STDOUT shape unchanged.** `jjf ls --json` and `jjf ready
  --json` still emit a bare array of `Issue` records on
  stdout. Wrapping the success payload into a `{issues, warnings}`
  envelope would have broken every existing `--json` caller
  (`jq '.[] | .id'`, scripts using `--limit 1 | jq '.[0]'`,
  etc.); riding the warning on stderr keeps stdout
  back-compat-stable. Consumers that don't read stderr stay
  oblivious; consumers that want the diagnostic parse one
  extra JSON object per stream.
- **Public storage API:** `Storage::unreadable_refs() ->
  Result<Vec<UnreadableRef>>` returns the same per-ref
  diagnostics. Each `UnreadableRef` carries `name` (full ref
  name) and `reason` (one-line human-readable cause). The
  CLI uses this; downstream tools (a future `jjf doctor`
  verb) will too.

## v2.5 → v2.6 changelog

Backwards-compatible additions, landed in the
`dep-cycle-undetected` ticket (`43c7615`).

- **New error kind `dependency_cycle`** (preflight, exit 2):
  `jjf dep add <source> <target>` would close a cycle in the
  `blocks`-edge graph. Self-deps (`source == target`) still
  surface as `self_dependency`; multi-step cycles surface as
  the new kind. `details.source` is the proposed edge's source
  id, `details.target` is the proposed target, and
  `details.cycle` is the existing chain `[target, ..., source]`
  that, combined with the new edge, would close. Echoed back
  so an operator can pinpoint which existing edges to break.
- **No new verbs.** Only `dep add` learned to reject cycles.
  `dep rm` is unaffected (removing edges can't create them).
  `jjf new -d` currently creates issues with their initial
  dep set in a single commit — cycles there require two
  pre-existing issues plus the new one, so the smallest
  cycle reachable via `-d` is two-step and lands fine for
  now (the new issue can't be the target of any pre-existing
  edge before it's created). A follow-up may extend cycle
  detection to the on-create path; out of scope for this
  ticket.
- **Scope: `blocks`-kind edges only.** `parent-child`,
  `related`, and `discovered-from` edges are not cycle-
  checked. `blocks` is the load-bearing case (`jjf ready`
  walks `blocks`-deps), so a `blocks` cycle silently hides
  every node in it from the ready set. The other kinds
  don't affect ready computation; their cycles are a follow-
  up if they ever become harmful.

## v2.4 → v2.5 changelog

Backwards-compatible additions, landed in the
`agent-await-gates-impl` ticket (`8ddf3fb`). Strict subset of
the beads-style gates feature (see
`docs/agent-await-gates-design.md` §6 for the verdict).

- **Two new verbs** `jjf block <id> [--reason <text>]` and
  `jjf unblock <id>`. Both land one multi-op commit on the
  `issues` bookmark:
  - `block`: sets `status = blocked` AND records
    `block_reason` as a scalar. Two trailers (`set-status` +
    `set-block-reason`), one commit.
  - `unblock`: sets `status = open` AND clears
    `block_reason`. Inverse of `block`. Idempotent when
    already open + unblocked.
  Reasons MUST be single-line — newlines surface as exit 1
  `invalid_input` from storage.
- **`jjf ready` gains `--include-blocked`** (mirror of the
  v2.3 `--include-claimed` flag). Without it, `blocked` issues
  are excluded from the ready set (an idle agent shouldn't see
  parked work). With it, blocked issues appear alongside open
  ones for "what's parked" views.
- **New status enum value `blocked`** on every verb that emits
  or accepts status (`show`, `ls --status blocked`, `update
  --status blocked`). `Status::Blocked` is wire-spelled
  `blocked`.
- **New record field `block_reason`** on the `Issue` JSON
  payload (string-or-null, default null). Visible on `jjf show
  --json` and on every `--json` verb that emits an Issue
  (`ls`, `ready`). Plain-text `jjf show` renders a
  `block-reason: <text>` line immediately under the status when
  the issue is blocked; non-blocked statuses don't render the
  line at all.
- **`jjf block --json` mutating envelope**:

  ```json
  {"ok": true, "id": "aa6600b", "status": "blocked", "reason": "waiting on PR-42", "blocked": true}
  ```

  When `--reason` is omitted (or whitespace-only), `reason` is
  `null`.

- **`jjf unblock --json` mutating envelope**:

  ```json
  {"ok": true, "id": "aa6600b", "status": "open", "blocked": false}
  ```

- **No new error kinds.** A multi-line `--reason` surfaces the
  existing `invalid_input` kind from storage (exit 1).
  Blocking a closed issue surfaces the same kind. Claiming a
  blocked issue (`jjf update <id> --claim`) surfaces
  `invalid_input` with a "unblock before claiming" hint.

## v2.3 → v2.4 changelog

Backwards-compatible additions, landed in the `agent-dep-types`
ticket (`b6d066b`).

- **New subcommand family `jjf dep add|rm|tree`** — manage
  typed dependency edges between issues. Four edge kinds:
  `blocks`, `parent-child`, `related`, `discovered-from`.
  See "Dep verbs" below for envelope shapes.
- **`jjf new -d` accepts `<kind>:<id>` inline** in addition
  to the v1 bare-id shape. The bare-id form keeps the v1
  default (`blocks`); the typed form lets you say
  `-d parent-child:abc1234`. Bad kind → `bad_dep_kind`
  (preflight, exit 2).
- **`jjf show` plain-text dependency section** changes from
  a single `dependencies: <id1>, <id2>` line to a typed
  section:

  ```text
  dependencies:
    blocks: abc1234, def5678
    parent-child: ghi9012
  ```

  An empty dep set still renders `dependencies: (none)`.
  `--json` output of `show` carries the new
  `[{target, kind}, ...]` array shape under `dependencies`
  (v2.4 spec §3).

- **`jjf ready` cascade.** With v2.4 edge kinds, an issue is
  blocked if any `blocks`-kind dep target is active, OR any
  `parent-child` target is itself blocked (fixpoint cascade).
  `related` and `discovered-from` edges are ignored.
  `jjf ready` continues to return JSON-array-of-Issue.
- **New error kind `bad_dep_kind`** (preflight, exit 2):
  `--dep <kind>:<id>` carried an unknown kind token.
  `details.value` is the raw spec; `details.kind` is the bad
  kind token; `details.field` is `"dep"`.

## v2.2 → v2.3 changelog

Backwards-compatible additions, landed in the
`agent-claim-atomic` ticket (`c3cc807`).

- **`jjf update` gains `--claim` / `--unclaim`** atomic
  shorthand flags. `--claim` sets assignee = current jj
  `user.name` AND status = `in-progress` in one multi-op
  commit. `--unclaim` clears the assignee AND sets status =
  `open` in one multi-op commit. Mutually exclusive with each
  other AND with `--assignee` / `--unset-assignee` /
  `--status`.
- **`jjf ready` gains `--include-claimed` and `--claim`.**
  Without `--include-claimed`, `in-progress` issues are
  excluded from the ready set (an idle agent shouldn't see
  another's claimed work). With `--include-claimed`, they
  appear alongside open issues. `--claim` is the
  "pick-and-claim-atomically" shorthand: `ready --claim
  --limit 1` returns one top result AND atomically claims it
  in a single CLI call. Requires `--limit 1`; other values
  exit 2 with `claim_requires_limit_one`.
- **New status enum value `in-progress`** on every verb that
  emits or accepts status (`show`, `ls`, `ready`, `update
  --status`). `jjf ls --status in-progress` and `jjf update
  --status in-progress` are valid.
- **Three new error kinds.**
  - `already_claimed` — preflight, exit 2. `--claim` was
    asked to claim an issue already InProgress with a
    different assignee. `details.by` is the existing
    assignee.
  - `no_current_user` — preflight, exit 2. `--claim` had no
    `jj user.name` configured.
  - `claim_requires_limit_one` — preflight, exit 2. `ready
    --claim` was used with `--limit` other than 1.
- **`jjf update --claim` mutating envelope**:

  ```json
  {"ok": true, "id": "aa6600b", "assignee": "alice", "status": "in-progress", "claimed": true}
  ```

- **`jjf update --unclaim` mutating envelope**:

  ```json
  {"ok": true, "id": "aa6600b", "status": "open", "claimed": false}
  ```

- **`jjf ready --claim --limit 1` mutating envelope** (when
  a claim landed):

  ```json
  {"ok": true, "id": "aa6600b", "assignee": "alice", "status": "in-progress", "claimed": true}
  ```

  When the ready set is empty:

  ```json
  {"ok": true, "id": null, "claimed": false}
  ```

## v2.1 → v2.2 changelog

Backwards-compatible additions, landed in the `agent-remember`
ticket (`81db913`).

- **Four new verbs** for persistent memories: `remember`,
  `memories`, `recall`, `forget`. Envelope shapes below in
  the "Memory verbs" section.
- **`jjf show` gains `--include-memories`** — plain-text
  affordance only. `--json` output of `show` is unchanged
  (memories are reachable via `jjf memories --json`).
- **Three new error kinds** — `missing_memory_value`
  (preflight, exit 2), `empty_memory_key` (preflight, exit 2,
  with `details.value`), `memory_not_found` (runtime, exit 1,
  with `details.key`).

## Dep verbs (v2.4)

### `jjf dep add <child> <parent> [--kind <kind>]`

Mutating envelope:

```json
{"ok": true, "child": "abc1234", "parent": "def5678", "kind": "blocks", "action": "added"}
```

- `child` / `parent`: the resolved 7-hex ids.
- `kind`: one of `blocks` / `parent-child` / `related` /
  `discovered-from`. Defaults to `blocks` if `--kind` is
  omitted.
- `action`: `"added"`.

### `jjf dep rm <child> <parent> [--kind <kind>]`

Same envelope shape as `dep add`, but `action: "removed"`.
Only edges with the matching `(target, kind)` are removed;
other-kind edges to the same target stay.

### `jjf dep tree <id>`

Read verb. Plain-text output is an indented tree:

```text
abc1234 [open] epic A
  def5678 [open] child B
    ghi9012 [closed] grandchild C
```

`--json` envelope is the structured `DepTree`:

```json
{
  "root": {
    "id": "abc1234",
    "title": "epic A",
    "status": "open",
    "children": [
      {"id": "def5678", "title": "child B", "status": "open",
       "children": [], "cycle": false}
    ],
    "cycle": false
  }
}
```

- `cycle: true` flags a node reached a second time via a
  cycle; recursion stops there and `children: []` for that
  node.

## Memory verbs (v2.2)

### `jjf remember [<value>] [--key <slug>] [-F <path|->]`

Mutating envelope:

```json
{"ok": true, "key": "dolt-phantoms", "action": "remembered"}
```

- `key`: the kebab-case key (either the operator's `--key`
  or the slugified value).
- `action`: `"remembered"` for a fresh memory; `"updated"`
  for the upsert path.

### `jjf memories [<search>]`

Bare payload — array of `Memory` records (no envelope):

```json
[
  {"key": "alpha", "value": "a", "created_at": "...", "updated_at": "..."},
  {"key": "beta",  "value": "b", "created_at": "...", "updated_at": "..."}
]
```

Empty result is `[]`, never silence (matches `ls --json` /
`remote ls --json`'s "valid empty array" rule).

The `<search>` substring filter is case-insensitive over the
key + value combo (any match counts).

### `jjf recall <key>`

Envelope shape — `{key, value, found}`:

```json
{"key": "dolt-phantoms", "value": "the insight", "found": true}
```

When the key doesn't exist, the verb exits 1 with the
`memory_not_found` error envelope on stderr:

```json
{"ok": false, "error": {"kind": "memory_not_found", "message": "...", "details": {"key": "no-such-thing"}}}
```

### `jjf forget <key>`

Mutating envelope:

```json
{"ok": true, "key": "dolt-phantoms", "action": "forgot"}
```

Missing key → exit 1 + `memory_not_found` error envelope.

## v2 → v2.1 changelog

Backwards-compatible additions, landed in
`issue-type-and-slug-fields`:

- **`Issue` payload gains `type` and `slug`** — `show --json`,
  `ls --json`, and the per-issue arrays embed the new fields
  verbatim. `type` is the lowercase wire spelling (`bug` |
  `feature` | `epic` | `research` | `roadmap` | `unspecified`);
  `slug` is a string or `null`.
- **`jjf new --json` accepts `--type` and `--slug` flags**; the
  envelope shape is unchanged (`{"ok":true,"id":"..."}`).
- **`jjf update --json` accepts `--type`, `--slug`,
  `--unset-slug`**. The `fields` array surfaces the populated
  field names — values include `"slug"` and `"type"` alongside
  the existing `"title"`/`"status"`/`"body"`/`"assignee"`.
- **`jjf ls --json` accepts `--type` (repeatable, OR-semantics)
  and `--slug` (substring match)** as filter flags.
- **Every id-taking verb (`show`, `update`, `close`, `open`,
  `comment`, `label add|rm`) accepts a slug in place of the
  7-hex id.** Resolution happens at the CLI boundary; verbs
  surface `slug_not_found` (exit 2) when the handle doesn't
  match an id or a known slug.
- **Three new error kinds** — `invalid_slug` (with
  `details.slug` + `details.reason`), `slug_collision` (with
  `details.slug` + `details.conflicts_with`), `slug_not_found`
  (with `details.handle`).
- **`jjf ready` verb landed** (`agent-ready` ticket) — bare
  JSON-array payload of `Issue` records (same shape as
  `ls --json`), filtered to the open + unblocked set and
  sorted by type priority (bug > feature > research > epic >
  unspecified; roadmap excluded). Accepts `--label` (AND),
  `--type` (OR), `--limit`. The canonical agent-loop call is
  `jjf ready --json --limit 1`.

This document is the canonical reference for what `jjf <verb> --json`
emits. The contract here is what scripts, the `mvp-sync` orchestrator,
and the `agent-ergonomics` MCP server are entitled to rely on. Changes
to the shapes below are breaking changes — they require a deprecation
note here and a parallel test update.

The CLI binary lives in `crates/jjf/src/main.rs`. The
integration-test pins for each shape live in
`crates/jjf/tests/<verb>.rs` under names containing
`json_envelope_shape` or `json_error_envelope`.

## v1 → v2 changelog

The nomenclature rename in the storage spec (`docs/storage-format.md`
"v1 → v2 changelog") carries through here. Breaking changes for any
script that pattern-matches the wire shape:

- **Error kinds renamed** (full table in the next section):
  - `bug_not_found` → `issue_not_found`
  - `missing_bugs_bookmark` → `missing_issues_bookmark`
  - The `bad_id` kind's `details.field` value `"id"` is unchanged
    (`bad_id` previously sourced from `BadBugId` / `BadDepId`; v2
    sources from `BadIssueId` / `BadDepId`, same wire shape).
- **Error `details` field renames** on the two legacy
  merge-driver kinds (`unmergeable`, `comment_file_conflict`): the
  `bug_id` key under `details` is now `issue_id`. Both kinds are
  unreachable from `jjf pull` post-`bfc732b`; only external callers
  of `jjf_merge::resolve` can surface them.
- **`jjf pull` envelope:** the `resolved_bugs` field is now
  `resolved_issues`. The `bookmark` field's value changes from
  `"bugs"` to `"issues"`.
- **`jjf push` envelope:** the `bookmark` field's value changes
  from `"bugs"` to `"issues"`.
- **`jjf init` envelope:** the `bookmark` field's value changes
  from `"bugs"` to `"issues"`.
- **Plain-text messages** (not contract, but observable):
  `pushed bugs -> X` → `pushed issues -> X`, etc.

The terminology rationale: jjforge tracks issues (the broad set —
roadmap, epic, defect, research note), not just bugs (the defect
subset). The user-facing prose has been "issue" since the cutover
blog post; v2 catches the wire shape up.

The `Issue` record's JSON shape (the bare payload `jjf show` /
`jjf ls` emit) was already documented as the `Bug` rust type's
serde projection in v1; the field names inside the record didn't
change in v2 (the record itself never used `bug_id` — its `id`
field has always been called `id`).

## Two envelope shapes

`jjf` has two distinct output shapes under `--json`, intentionally.
Reads and mutations look different on purpose; do not flatten them.

### Mutating verbs — `{"ok": true, ...}` envelope

Every verb that writes lands its output as an "ok envelope" — a JSON
object with `ok: true` plus verb-specific fields. The envelope tells a
caller "yes, the requested mutation landed"; the verb-specific fields
identify what it landed.

Verbs in this family: `init`, `new`, `close`, `open`, `update`,
`comment`, `label add`, `label rm`, `remote add`, `remote rm`,
`push`, `pull`.

### Read verbs — bare payload

The read verbs emit the structured payload directly, with no envelope:

- `show` emits the `Issue` record verbatim.
- `ls` emits a JSON array of `Issue` records (possibly empty: `[]`).
- `remote ls` emits a JSON array of `{name, url}` objects (possibly
  empty: `[]`).

The reasoning, from the in-source comment on `run_show`: the `Issue`
struct IS the structured payload. Wrapping it in `{"ok": true,
"issue": {...}}` would force every caller into one extra unwrap step
with no benefit — `show` either succeeds and emits an `Issue`, or
fails and emits an error envelope (see below). The success/failure
distinction is already carried by the exit code and stderr shape.

### Error envelope — `{"ok": false, "error": {...}}`

Every verb under `--json`, mutating or reading, renders failures as a
single shape on **stderr**:

```json
{
  "ok": false,
  "error": {
    "kind": "<machine-greppable-kind>",
    "message": "<human-readable>",
    "details": { /* variant-specific, optional */ }
  }
}
```

- `kind` is a stable lowercase snake_case identifier. The table in
  the next section enumerates every kind the binary emits today.
- `message` is the human-readable string (the same text the plain
  `jjf: <message>` stderr would print). Format is not stable; don't
  pattern-match on it. Use `kind`.
- `details` is variant-specific structured context. Either absent
  (no structured fields beyond kind+message) or an object whose
  fields are documented per-variant. The kind table below names the
  keys to expect.

Errors always go to **stderr**, never stdout. Exit codes are unchanged
from plain-text mode (see the top comment in `main.rs`: `0` success,
`1` runtime, `2` preflight / argument failure).

## Error-kind table

| `kind`                       | Exit | Source variant                | `details` keys           |
|------------------------------|------|-------------------------------|--------------------------|
| `not_a_jj_repo`              | 2    | `Storage::NotAJjRepo`         | `path`                   |
| `corrupt_sentinel`           | 1    | `Storage::CorruptSentinel`    | `oid`, `object_type`     |
| `missing_issues_bookmark`    | 2    | `MissingIssuesBookmark`       | `path`                   |
| `issue_not_found`            | 1    | `Storage::IssueNotFound`      | `id`                     |
| `bad_id`                     | 2    | `BadIssueId` / `BadDepId`     | `value`, `field`         |
| `bad_dep_kind`               | 2    | `BadDepKind` (v2.4)           | `value`, `kind`, `field` |
| `empty_body`                 | 2    | `EmptyCommentBody`            | —                        |
| `empty_label`                | 2    | `EmptyLabel`                  | —                        |
| `missing_author`             | 2    | `MissingAuthor`               | —                        |
| `no_update_fields`           | 2    | `NoUpdateFields`              | —                        |
| `remote_already_exists`      | 2    | `RemoteAlreadyExists`         | `name`                   |
| `remote_not_found`           | 2    | `RemoteNotFound`              | `name`                   |
| `body_read_error`            | 2    | `BodyRead`                    | `from`                   |
| `cwd_error`                  | 2    | `Cwd`                         | —                        |
| `probe_error`                | 1    | `Probe`                       | —                        |
| `jj_git_remote_error`        | 1    | `JjGitRemote`                 | —                        |
| `push_network_failure`       | 1    | `PushNetworkFailure`          | `remote`                 |
| `push_auth_failure`          | 1    | `PushAuthFailure`             | `remote`                 |
| `push_rejected`              | 1    | `PushRejected`                | `remote`, `hint`, `refs_rejected`, `stderr_raw` |
| `jj_git_push_error`          | 1    | `JjGitPush`                   | —                        |
| `pull_network_failure`       | 1    | `PullNetworkFailure`          | `remote`                 |
| `pull_auth_failure`          | 1    | `PullAuthFailure`             | `remote`                 |
| `jj_git_fetch_error`         | 1    | `JjGitFetch`                  | —                        |
| `unmergeable`                | 1    | `Unmergeable`                 | `issue_id`, `detail`     |
| `comment_file_conflict`      | 1    | `CommentFileConflict`         | `issue_id`               |
| `invalid_slug`               | 2    | `Storage::InvalidSlug` / `InvalidSlug` | `slug`, `reason`        |
| `invalid_title`              | 2    | `Storage::InvalidTitle` / `InvalidTitle` | `title`, `reason`, `codepoint`* |
| `body_too_large`             | 2    | `Storage::InvalidBody` / `InvalidBody` | `limit`, `got`         |
| `slug_collision`             | 2    | `Storage::SlugCollision` / `SlugCollision` | `slug`, `conflicts_with` |
| `slug_not_found`             | 2    | `Storage::SlugNotFound` / `SlugNotFound` | `handle`                 |
| `invalid_input`              | 1    | `Storage::Invalid`            | —                        |
| `clock_error`                | 1    | `Storage::Clock`              | —                        |
| `io_error`                   | 1    | `Storage::Io`                 | —                        |
| `json_error`                 | 1    | `Storage::Json`               | —                        |
| `jj_error`                   | 1    | `Storage::Jj`                 | —                        |
| `already_claimed`            | 2    | `Storage::AlreadyClaimed` / `AlreadyClaimed` | `by` |
| `no_current_user`            | 2    | `NoCurrentUser`               | —                        |
| `claim_requires_limit_one`   | 2    | `ClaimRequiresLimitOne`       | —                        |
| `self_dependency`            | 2    | `Storage::SelfDependency` / `SelfDependency` | `id`                 |
| `dependency_cycle`           | 2    | `Storage::DependencyCycle` / `DependencyCycle` | `source`, `target`, `cycle` |
| `concurrent_write`           | 1    | `Storage::ConcurrentWrite` / `ConcurrentWrite` | `hint`             |

Adding a new variant to `CliError`? Pick a stable kind, add it to
the `kind()` match in `main.rs`, add a row above, and add a
test in the relevant `tests/<verb>.rs` file that pins it.

\* The `invalid_title` envelope carries a `codepoint` key in
`details` ONLY when `reason` is `control_char`; for `empty`,
`newline`, and `null_byte` the field is omitted.

### Note on `invalid_title`

Emitted by `jjf new -t` and `jjf update --title` when the supplied
title contains a control character that would corrupt downstream
surfaces, or is empty after trim. Preflight failure (exit 2).
Added in `qa-title-validation` (issue `e4e483b`) after a QA
red-team round found embedded `\0` was silently truncated before
storage (data loss) and embedded `\n` corrupted `jjf ls` text
rows (the tab-separated row format has no escape rule).

`details.reason` is one of:

- `empty` — title was empty or whitespace-only after trim.
- `newline` — title contained `\n` (U+000A) or `\r` (U+000D).
- `null_byte` — title contained `\0` (U+0000).
- `control_char` — title contained any other control character
  per `char::is_control` (tabs included — `\t` breaks the
  `jjf ls` row format too). `details.codepoint` carries the
  offending Unicode scalar as an unsigned integer.

```sh
$ jjf new --json -t $'foo\nbar'
{"ok":false,"error":{"kind":"invalid_title","message":"...","details":{"title":"foo\nbar","reason":"newline"}}}

$ jjf new --json -t $'a\tb'
{"ok":false,"error":{"kind":"invalid_title","message":"...","details":{"title":"a\tb","reason":"control_char","codepoint":9}}}
```

The `null_byte` reason is reachable only via programmatic
callers of `Storage::create_issue` / `Storage::update` (e.g. a
Python or MCP client constructing the call directly). POSIX
argv is a NUL-terminated C string array, so a shell-typed
`jjf new -t $'a\x00b'` actually loses the bytes after the
null in the shell's argv expansion before `jjf` sees them —
the storage-side guard catches it for every other entry point.

### Note on `body_too_large`

Emitted by `jjf new -F`, `jjf update --body-file`, and `jjf
comment -F` when the supplied body exceeds 65,536 bytes
(raw UTF-8 byte length). Preflight failure (exit 2). The
cap matches GitHub's documented issue-body limit (and
Forgejo's, which mirrors it) so jjforge's surface is
predictable to anyone who already knows the prior art.
Added in issue `679444a` (QA red-team 2026-06-25 sub-pass 4
C3), where a multi-megabyte body landed silently with no
declared bound.

`details.limit` is the configured cap (always 65,536 today)
and `details.got` is the measured byte length of the
offending body — both are JSON integers, not strings, so
scripted callers can branch on them directly. The same cap
applies to comment bodies (`jjf comment -F`) for the same
reasons: comment bodies are free-form markdown stored as
JSONL records on the per-issue `.comments.jsonl` blob, with
the same on-disk shape and per-write resource model as the
issue body.

```sh
$ head -c 70000 /dev/urandom | base64 > big.md
$ jjf new --json -t "demo" -F big.md
{"ok":false,"error":{"kind":"body_too_large","message":"...","details":{"limit":65536,"got":94668}}}
```

Measurement is `body.len()` — raw UTF-8 bytes. Not character
count, not grapheme cluster count, not after-trim. A body
that's 65,537 bytes is rejected even if it's fewer than
65,536 Unicode scalars (multi-byte content gets the worst-
case-ASCII budget, matching the GitHub semantic).

### Note on `self_dependency`

Emitted by `jjf dep add <child> <target>` (and the inline
`jjf new -d <self-id>` / `jjf new --dep <kind>:<self-id>`
on-create forms) when `<child> == <target>`. Preflight failure
(exit 2). Added in `qa-dep-validation` (issue `d1a01f0`) after
a QA red-team round found that `jjf dep add A A` would silently
land a `blocks`-edge from A to itself — making A permanently
blocked-by-itself and excluding it from `jjf ready` forever (a
one-line DoS).

The check applies to every dep kind: `blocks` is the
load-bearing case (the self-block DoS), but `parent-child`,
`related`, and `discovered-from` self-edges are nonsense in
all four cases and reject uniformly.

`details.id` is the offending issue id (the resolved child id,
which equals the resolved target id by definition).

The companion validation — phantom dep targets — reuses the
existing `issue_not_found` kind (no new kind needed): the
target failed to resolve on the bookmark, so it's surfaced the
same way as `jjf show <bogus-id>`. That kind is exit 1
(runtime: well-formed input, just doesn't exist), not exit 2
like `self_dependency`. Scripts pattern-match on the kind;
the exit code distinguishes preflight from runtime.

```sh
$ jjf --json dep add a3f9c01 a3f9c01
{"ok":false,"error":{"kind":"self_dependency","message":"issue a3f9c01 cannot depend on itself","details":{"id":"a3f9c01"}}}

$ jjf --json dep add a3f9c01 deadbee   # phantom target
{"ok":false,"error":{"kind":"issue_not_found","message":"issue not found in working copy: deadbee","details":{"id":"deadbee"}}}

$ jjf --json new -t "child" -d deadbee -F -
{"ok":false,"error":{"kind":"issue_not_found","message":"issue not found in working copy: deadbee","details":{"id":"deadbee"}}}
```

`jjf dep rm` is intentionally permissive against phantom
targets — removing an edge that doesn't exist is a useful
cleanup primitive and never lands a dangling edge.

### Note on `dependency_cycle`

Emitted by `jjf dep add <source> <target>` (v2.6,
`dep-cycle-undetected`, issue `43c7615`) when adding the
proposed `blocks`-edge would close a cycle in the existing
`blocks`-edge graph. Preflight failure (exit 2). Added after
QA found that `jjf dep add` silently accepted edges that
closed multi-step cycles, hiding every node in the cycle
from `jjf ready` with no diagnostic.

Self-deps (`source == target`) still surface as
`self_dependency`, not `dependency_cycle` — the older check
runs first and is more specific.

`details.source` and `details.target` are the proposed
edge's endpoints (the values the operator passed to
`jjf dep add`). `details.cycle` is the existing chain of ids
`[target, ..., source]`: walking forward over `blocks`-deps
from `target`, that's the path that ends at `source`. The
proposed `source -> target` edge would extend it to
`[source, target, ..., source]`, which is the back-edge.

```sh
$ jjf --json dep add a3f9c01 c001cab
{"ok":false,"error":{"kind":"dependency_cycle","message":"adding blocks-edge a3f9c01 -> c001cab would close a dependency cycle","details":{"source":"a3f9c01","target":"c001cab","cycle":["c001cab","b00b00b","a3f9c01"]}}}
```

Scope: the check covers `--kind blocks` only. The other dep
kinds (`parent-child`, `related`, `discovered-from`) don't
affect `jjf ready` computation, so cycles among them aren't
silent landmines. A future ticket may extend cycle detection
to `parent-child` (which `jjf dep tree` recurses on, though
that walker already has its own visited-set guard).

### Note on `concurrent_write`

Emitted by any mutating verb (`new`, `update`, `comment`, `close`,
`open`, `block`, `unblock`, `label add|rm`, `dep add|rm`,
`remember`, `forget`) when a sibling jjforge writer landed first
and the 4-CLI write dance hit jj's "Concurrent checkout" failure.
Runtime failure (exit 1): the command was well-formed, the loser
just has to re-run.

The storage layer auto-retries ONCE on non-slug-claim mutations
(comments, updates, status changes, etc.) before surfacing this
— the dominant race shape is a single sibling racer that
completes its dance in the time it takes us to spin our retry
back up, and the retry re-reads bookmark state so any landed
content is preserved (the concurrent-comment test pins this).
If you see `concurrent_write` despite the retry, either both
attempts raced (rare) or the failure was a slug-claim create
(where retry would re-race the same slug indefinitely and the
fail-fast surface is preferred).

For slug-claim creates, the post-failure probe upgrades this to
the more-specific `slug_collision` envelope when the failure
race was specifically two writers fighting for the same slug
slot and the slug is now visibly taken. The fallback to
`concurrent_write` happens when the probe timing missed the
winner's commit (legitimate concurrent failure without an
identifiable winner yet).

`details.hint` carries an operator-facing one-line message,
rendered verbatim by the text renderer. The hint distinguishes
"first attempt raced; retry" from "auto-retry exhausted; retry
the command yourself."

Added in `qa-concurrent-write-ux` (issue `277f559`) after a QA
red-team round found the loser of a concurrent `jjf new --slug
<s>` saw a 12-line jj-internal cascade including "Internal
error: Failed to check out commit … Caused by: Concurrent
checkout" — useless to an agent in an automated loop.

```sh
$ jjf --json comment a3f9c01 -F -    # sibling write raced and retry also raced
{"ok":false,"error":{"kind":"concurrent_write","message":"concurrent write conflict; another writer landed first; retried once and still raced. Retry your command.","details":{"hint":"another writer landed first; retried once and still raced. Retry your command."}}}

$ jjf --json new -t winner --slug taken    # slug-claim race upgraded to slug_collision
{"ok":false,"error":{"kind":"slug_collision","message":"slug \"taken\" already in use by issue a3f9c01","details":{"slug":"taken","conflicts_with":"a3f9c01"}}}
```

### Note on `push_rejected`

Emitted by `jjf push <remote>` when the remote rejected the
update (non-fast-forward — another writer landed first; or a
remote-side hook rejection). Runtime failure (exit 1).

`message` is a short, deterministic, single-line phrase (no
raw git stderr, no version-dependent advisory tokens like
`fetch first` or git's own multi-line `hint:` preamble). As
with every other kind: scripts must use `kind`, not `message`.

The structured surface is `details`:

- `details.remote` — the remote name the operator passed.
- `details.hint` — operator-facing one-line advisory,
  rendered verbatim by the text renderer. Currently
  `"run \`jjf pull <remote>\` first, then retry the push"`.
  Mirrors `concurrent_write`'s `details.hint` shape so a
  caller handling both error paths can read the same key.
- `details.refs_rejected` — array of destination refs git
  rejected (parsed from `! [rejected]   <src> -> <dst>` lines
  in stderr). Example: `["refs/jjf/issues/bfcfe03"]`. Useful
  for callers that want to surface "which issue conflicted?"
  without scraping the raw stderr.
  Surfaces as `null` (not an empty array) when the parser
  recognised no rejected lines in stderr — better to be
  honest about uncertainty than to ship a sometimes-wrong
  list. Callers should treat `null` as "unknown, fall back
  to stderr_raw or just tell the user to pull."
- `details.stderr_raw` — the original git stderr blob, kept
  available for debugging callers without putting it on the
  `message` contract surface. Includes the multi-line `hint:`
  preamble and any other version-dependent text git emits.

Added in `qa-2026-06-25-push-rejected-raw-git` (issue
`88e4d6b`) after a QA red-team round found `message` embedded
raw git stderr — including the internal
`refs/jjf/issues/*` refspec and git's version-dependent
advisory phrases — creating pressure to parse `message`
because `details` was too sparse to identify the conflicting
refs.

```sh
$ jjf --json push origin
{"ok":false,"error":{"kind":"push_rejected","message":"push to origin rejected (non-fast-forward); the remote moved since you last pulled","details":{"remote":"origin","hint":"run `jjf pull origin` first, then retry the push","refs_rejected":["refs/jjf/issues/bfcfe03"],"stderr_raw":"To file:///.../bare.git\n ! [rejected]        refs/jjf/issues/bfcfe03 -> refs/jjf/issues/bfcfe03 (fetch first)\n..."}}}
```

## Per-verb examples

Every example below is one success path and the most representative
error path for the verb. The integration tests under
`crates/jjf/tests/<verb>.rs` pin these shapes; if you change one
here, change the test too.

### `init`

```sh
$ jjf init --json
{"ok":true,"bookmark":"issues"}
```

Error path — running in a directory that isn't a jj repo:

```sh
$ jjf init --json
{"ok":false,"error":{"kind":"not_a_jj_repo","message":"not a jj repo: /tmp/foo","details":{"path":"/tmp/foo"}}}
```

### `new`

```sh
$ echo "body" | jjf new --json -t "fix the thing" -F -
{"ok":true,"id":"a3f9c01"}
```

Optional flags for v2.1 fields:

```sh
$ jjf new --json -t "agent-ready" --type feature --slug agent-ready
{"ok":true,"id":"a3f9c01"}
```

Error path — invalid slug (`bad_charset` is one of `bad_charset` /
`too_short` / `too_long` / `leading_hyphen` / `trailing_hyphen` /
`consecutive_hyphens`):

```sh
$ jjf new --json -t x --slug Bad_Slug
{"ok":false,"error":{"kind":"invalid_slug","message":"...","details":{"slug":"Bad_Slug","reason":"bad_charset"}}}
```

Error path — slug collision with an open issue:

```sh
$ jjf new --json -t x --slug taken
{"ok":false,"error":{"kind":"slug_collision","message":"...","details":{"slug":"taken","conflicts_with":"a3f9c01"}}}
```

Error path — invalid title (embedded newline corrupts `jjf ls`
text rows; embedded null byte was silently truncated before
`qa-title-validation`). `reason` is one of `empty` / `newline` /
`null_byte` / `control_char`:

```sh
$ jjf new --json -t $'foo\nbar'
{"ok":false,"error":{"kind":"invalid_title","message":"...","details":{"title":"foo\nbar","reason":"newline"}}}
```

Error path — `issues` bookmark missing (didn't run `jjf init` first):

```sh
$ echo body | jjf new --json -t x -F -
{"ok":false,"error":{"kind":"missing_issues_bookmark","message":"the `issues` bookmark does not exist in /repo; run `jjf init` first","details":{"path":"/repo"}}}
```

### `show`

Success path emits the `Issue` record verbatim — no envelope:

```sh
$ jjf show --json a3f9c01
{
  "id": "a3f9c01",
  "title": "fix the thing",
  "slug": "agent-ready",
  "body": "body\n",
  "status": "open",
  "type": "feature",
  "labels": [],
  "dependencies": [],
  "assignee": null,
  "comments": [],
  "created_at": "2026-06-21T22:00:00Z",
  "updated_at": "2026-06-21T22:00:00Z"
}
```

`show` also accepts a slug in place of the id:

```sh
$ jjf show --json agent-ready
{ ... same payload ... }
```

A handle that's neither a parseable id nor a known slug surfaces
the `slug_not_found` envelope:

```sh
$ jjf show --json nope
{"ok":false,"error":{"kind":"slug_not_found","message":"no issue with handle \"nope\"","details":{"handle":"nope"}}}
```

Error path — nonexistent id:

```sh
$ jjf show --json deadbee
{"ok":false,"error":{"kind":"issue_not_found","message":"issue not found in working copy: deadbee","details":{"id":"deadbee"}}}
```

### `ls`

Success path is a bare JSON array (possibly empty):

```sh
$ jjf ls --json
[
  {
    "id": "a3f9c01",
    "title": "fix the thing",
    "status": "open",
    ...
  }
]
```

Empty result is `[]`, not silence — scripts piping to `jq length`
get a useful value either way.

v2.1 filters:

- `--type <kind>` — repeatable, OR-semantics across the listed
  types. An issue matches if its `type` field equals any listed
  type.
- `--slug <pattern>` — case-sensitive substring match against
  the `slug` field. Issues without a slug never match.

```sh
$ jjf ls --json --type bug --type feature
[ ... open issues whose type is bug OR feature ... ]

$ jjf ls --json --slug agent
[ ... open issues whose slug contains "agent" ... ]
```

Error path — running outside a jj repo:

```sh
$ jjf ls --json
{"ok":false,"error":{"kind":"not_a_jj_repo","message":"not a jj repo: /tmp/foo","details":{"path":"/tmp/foo"}}}
```

### `ready`

Success path is a bare JSON array (possibly empty) of `Issue`
records — the same per-element shape as `ls --json` and `show
--json`:

```sh
$ jjf ready --json --limit 1
[
  {
    "id": "a3f9c01",
    "title": "fix the thing",
    "status": "open",
    "type": "bug",
    ...
  }
]
```

Filters:

- `--label <NAME>` — repeatable, AND-semantics. Mirrors
  `jjf ls --label`.
- `--type <KIND>` — repeatable, OR-semantics. Mirrors
  `jjf ls --type`. Note that `Roadmap`-typed issues are
  excluded from the ready set entirely — they're the planning
  surface, not work to do — regardless of this filter.
- `--limit <N>` — truncate to the first N entries AFTER the
  priority sort. Omit for unlimited.
- `--include-claimed` (v2.3) — also include `in-progress`
  issues in the result. Off by default so idle agents don't
  see another agent's claimed work as available.
- `--claim` (v2.3) — atomic shorthand: pick the top result
  AND claim it. Requires `--limit 1`; other values exit 2
  with `claim_requires_limit_one`. Emits the mutating
  envelope shown in the v2.3 changelog at the top of this
  file.

Selection criteria — an issue is "ready" iff:

- Its `status` is `open`. With `--include-claimed`,
  `in-progress` issues are included too.
- Its `type` is not `roadmap`.
- Every `dependencies[]` id either points at a CLOSED issue or
  at a non-existent issue id (a dangling reference is treated
  as unblocking — a deleted dep doesn't wedge progress). An
  InProgress dep still BLOCKS — it's not closed yet.
- It passes all `--label` / `--type` filters.

Sort order:

1. **Type priority** (descending): `bug` > `feature` >
   `research` > `epic` > `unspecified`. Higher priority first.
2. **Tiebreaker**: `created_at` ascending (FIFO — agents grind
   the oldest unblocked work down first).

Empty result is `[]`, matching `ls`'s convention so scripts
piping to `jq length` get a useful value.

```sh
$ jjf ready --json --label backend
[ ... open + unblocked + label=backend ... ]

$ jjf ready --json --type bug --limit 1
[ ... the one highest-priority unblocked bug ... ]
```

Error path — running outside a jj repo:

```sh
$ jjf ready --json
{"ok":false,"error":{"kind":"not_a_jj_repo","message":"not a jj repo: /tmp/foo","details":{"path":"/tmp/foo"}}}
```

### `update`

```sh
$ jjf update --json a3f9c01 --title "renamed" --status closed
{"ok":true,"id":"a3f9c01","fields":["title","status"]}
```

The `fields` array lists the populated fields in field-declaration
order — the same order the storage layer lands the corresponding
trailers on the resulting commit. The full ordering (v2.1):

1. `title`
2. `slug`
3. `status`
4. `type`
5. `body`
6. `assignee`

`update` accepts a slug in place of the id (`jjf update
agent-ready --title ...` works the same as the 7-hex variant).
`--slug new-handle` and `--unset-slug` are mutually exclusive
(clap enforces).

```sh
$ jjf update --json a3f9c01 --type bug --slug fix-the-thing
{"ok":true,"id":"a3f9c01","fields":["slug","type"]}

$ jjf update --json a3f9c01 --unset-slug
{"ok":true,"id":"a3f9c01","fields":["slug"]}
```

v2.3 added `--claim` / `--unclaim` shorthand:

```sh
$ jjf update --json a3f9c01 --claim
{"ok":true,"id":"a3f9c01","assignee":"alice","status":"in-progress","claimed":true}

$ jjf update --json a3f9c01 --unclaim
{"ok":true,"id":"a3f9c01","status":"open","claimed":false}
```

`--claim` is mutually exclusive with `--unclaim`, `--assignee`,
`--unset-assignee`, and `--status` (clap-enforced). Same-user
re-claim is a no-op (exit 0, no new commit). Different-user
re-claim exits 2 with `already_claimed`:

```sh
$ jjf update --json a3f9c01 --claim
{"ok":false,"error":{"kind":"already_claimed","message":"issue already claimed by \"alice\"","details":{"by":"alice"}}}
```

Error path — nonexistent id:

```sh
$ jjf update --json deadbee --title x
{"ok":false,"error":{"kind":"issue_not_found","message":"issue not found in working copy: deadbee","details":{"id":"deadbee"}}}
```

### `comment`

```sh
$ echo "looks good to me" | jjf comment --json a3f9c01 -F -
{"ok":true,"id":"a3f9c01","comment_id":"b71f02a"}
```

Error path — empty body:

```sh
$ echo -n "" | jjf comment --json a3f9c01 -F -
{"ok":false,"error":{"kind":"empty_body","message":"comment body is empty; pipe non-empty content via -F - or pass -F <path>"}}
```

### `close` / `open`

```sh
$ jjf close --json a3f9c01
{"ok":true,"id":"a3f9c01","status":"closed"}

$ jjf open --json a3f9c01
{"ok":true,"id":"a3f9c01","status":"open"}
```

Error path — nonexistent id:

```sh
$ jjf close --json deadbee
{"ok":false,"error":{"kind":"issue_not_found","message":"issue not found in working copy: deadbee","details":{"id":"deadbee"}}}
```

### `label add` / `label rm`

```sh
$ jjf label add --json a3f9c01 needs-review
{"ok":true,"id":"a3f9c01","label":"needs-review","action":"added"}

$ jjf label rm --json a3f9c01 needs-review
{"ok":true,"id":"a3f9c01","label":"needs-review","action":"removed"}
```

Error path — empty label:

```sh
$ jjf label add --json a3f9c01 ""
{"ok":false,"error":{"kind":"empty_label","message":"label must not be empty"}}
```

### `remote add`

Mutating verb — `{"ok": true, ...}` envelope with the name and URL
just recorded. `remote add` does NOT talk to the URL; it only
records it. URL validation is jj's responsibility — whatever jj
rejects, we surface as `jj_git_remote_error` (exit 1).

```sh
$ jjf remote add --json origin https://example.com/repo.git
{"ok":true,"name":"origin","url":"https://example.com/repo.git"}
```

Error path — name already exists:

```sh
$ jjf remote add --json origin https://example.com/other.git
{"ok":false,"error":{"kind":"remote_already_exists","message":"git remote already exists: origin","details":{"name":"origin"}}}
```

### `remote ls`

Read verb — bare JSON array of `{name, url}` objects. Empty result
is `[]`, not silence (same `jq length` rationale as `ls`).

```sh
$ jjf remote ls --json
[
  {
    "name": "origin",
    "url": "https://example.com/repo.git"
  }
]
```

Error path — running outside a jj repo:

```sh
$ jjf remote ls --json
{"ok":false,"error":{"kind":"not_a_jj_repo","message":"not a jj repo: /tmp/foo","details":{"path":"/tmp/foo"}}}
```

### `remote rm`

Mutating verb — `{"ok": true, "name": "..."}` envelope. Note that
jj also forgets bookmarks tracked from that remote (its own
behavior); we surface its successful exit verbatim.

```sh
$ jjf remote rm --json origin
{"ok":true,"name":"origin"}
```

Error path — name not found:

```sh
$ jjf remote rm --json nope
{"ok":false,"error":{"kind":"remote_not_found","message":"git remote not found: nope","details":{"name":"nope"}}}
```

### `push`

Mutating verb — `{"ok": true, ...}` envelope. Wraps
`jj git push --bookmark issues --remote <remote>` and translates the
common failure modes (network, auth, non-fast-forward rejection,
unknown remote) into typed kinds so scripts can branch.

Preflight is the full `issues_bookmark` probe — there's nothing to
push if the local bookmark doesn't exist. Unknown remote is exit 2
(preflight); network/auth/reject are exit 1 (runtime — the command
was well-formed, the remote just said no).

```sh
$ jjf push --json origin
{"ok":true,"remote":"origin","bookmark":"issues"}
```

Error path — unknown remote:

```sh
$ jjf push --json nope
{"ok":false,"error":{"kind":"remote_not_found","message":"git remote not found: nope","details":{"name":"nope"}}}
```

Error path — non-fast-forward (operator should pull first):

```sh
$ jjf push --json origin
{"ok":false,"error":{"kind":"push_rejected","message":"push to origin rejected (non-fast-forward); the remote moved since you last pulled","details":{"remote":"origin","hint":"run `jjf pull origin` first, then retry the push","refs_rejected":["refs/jjf/issues/bfcfe03"],"stderr_raw":"To file:///.../bare.git\n ! [rejected]        refs/jjf/issues/bfcfe03 -> refs/jjf/issues/bfcfe03 (fetch first)\n..."}}}
```

See the [Note on `push_rejected`](#note-on-push_rejected) for the
contract on `details.hint`, `details.refs_rejected`, and
`details.stderr_raw`.

### `pull`

Mutating verb — `{"ok": true, ...}` envelope. Three success
shapes, distinguished by `remote_present` (bool) and
`resolved_issues` (non-negative integer):

- **remote has no `issues` bookmark yet** (first push from the
  other side hasn't happened) — exit 0, `remote_present: false`,
  `resolved_issues: 0`.
- **clean fetch, no divergence** (jj fast-forwarded or there was
  nothing new) — exit 0, `remote_present: true`,
  `resolved_issues: 0`.
- **divergence, op-space resolver ran** (`Storage::
  resolve_divergence` reduced N issues across the divergent
  heads) — exit 0, `remote_present: true`, `resolved_issues: N`.

Every success envelope carries `merge_strategy: "op_space"` to pin
which driver ran. The field exists for forward-compat — a future
`jjf` may grow alternate strategies (e.g. a `file_bytes` escape
hatch, see `bfc732b`); today the only value is `op_space`.

Preflight is jj-repo-only (not the full `issues_bookmark` probe) —
a fresh clone has `issues@<remote>` but no local `issues` yet, and
`pull` is what materializes the local bookmark via the
`jj bookmark track` step.

```sh
$ jjf pull --json origin
{"ok":true,"remote":"origin","bookmark":"issues","remote_present":true,"merge_strategy":"op_space","resolved_issues":0}
```

Empty-remote variant — first time anyone pulls from a remote whose
issues bookmark hasn't been pushed yet:

```sh
$ jjf pull --json origin
{"ok":true,"remote":"origin","bookmark":"issues","remote_present":false,"merge_strategy":"op_space","resolved_issues":0}
```

With merges:

```sh
$ jjf pull --json origin
{"ok":true,"remote":"origin","bookmark":"issues","remote_present":true,"merge_strategy":"op_space","resolved_issues":2}
```

Error path — unknown remote:

```sh
$ jjf pull --json nope
{"ok":false,"error":{"kind":"remote_not_found","message":"git remote not found: nope","details":{"name":"nope"}}}
```

#### Unreachable error kinds on the v2 operator path

The legacy v1 file-bytes merge driver (`jjf-merge`) had two
human-surface failure modes — `unmergeable` (body-text collision)
and `comment_file_conflict` (jj content-merge marker in a
`.comments.jsonl` file). The `jjf pull` v1 path could surface
both.

**As of `bfc732b` (sync-conflict-fallback), `jjf pull` uses the
op-space resolver in `crates/jjf-storage/src/merge_ops.rs`. That
resolver has no failure mode that maps to either error kind:
`set-body` is just another LWW scalar (`storage-format.md` §6.2),
and `.comments.jsonl` is rebuilt as a union of pristine bytes
from each head, never read with conflict markers.** The error
kinds stay defined for shape stability — external callers of
`jjf_merge::resolve` (the library that stays in the workspace as
a non-operator-path tool) can still surface them, and the JSON
envelope contract pins the enum — but `jjf pull` will not raise
them.

The two error kinds' historic shape (kept for reference):

```sh
$ jjf pull --json origin
{"ok":false,"error":{"kind":"unmergeable","message":"…","details":{"issue_id":"aa6600b","detail":"…"}}}
```

```sh
$ jjf pull --json origin
{"ok":false,"error":{"kind":"comment_file_conflict","message":"…","details":{"issue_id":"aa6600b"}}}
```

## The clap arg-error exception

There is **one** place the JSON envelope does not apply: errors raised
by `clap` while parsing the command line itself (unknown flag, missing
required positional, value-type mismatch on a `ValueEnum`, etc.).

`clap` runs before `main()` ever sees the parsed `Cli`, so the
`--json` flag isn't observable at the point clap renders its error.
Those errors render in clap's default plain-text form (typically to
stderr, exit 2), even when the user passed `--json`. A representative
shape:

```sh
$ jjf --not-a-real-flag
error: unexpected argument '--not-a-real-flag' found

Usage: jjf [OPTIONS] <COMMAND>

For more information, try '--help'.
```

Callers parsing `--json` output should treat exit-2 stderr that
**doesn't** start with `{"ok":false` as a clap arg-parse failure and
either re-render or pass through unchanged. Everything past arg
parsing (preflight, IO, storage, runtime) honors the JSON envelope.

## Exit-code convention

Cross-link to the top-of-file comment in `crates/jjf/src/main.rs`,
which is the canonical statement:

- `0` — success.
- `1` — runtime failure (storage error, IO error, "we tried, it
  didn't work").
- `2` — argument or preflight failure (bad flags, missing input,
  "this isn't a jj repo"). Includes every clap arg-parse error.

The error envelope's `kind` is the machine-readable channel for
the *category* of failure; the exit code is the binary signal a
shell pipeline reacts to. The two are correlated in the kind
table above.
