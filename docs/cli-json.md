# `jjf --json` output contract

This document is the canonical reference for what `jjf <verb> --json`
emits. The contract here is what scripts, the upcoming `mvp-sync`
orchestrator, and the `agent-ergonomics` MCP server are entitled to
rely on. Changes to the shapes below are breaking changes — they
require a deprecation note here and a parallel test update.

The CLI binary lives in `crates/jjf/src/main.rs`. The
integration-test pins for each shape live in
`crates/jjf/tests/<verb>.rs` under names containing
`json_envelope_shape` or `json_error_envelope`.

## Two envelope shapes

`jjf` has two distinct output shapes under `--json`, intentionally.
Reads and mutations look different on purpose; do not flatten them.

### Mutating verbs — `{"ok": true, ...}` envelope

Every verb that writes lands its output as an "ok envelope" — a JSON
object with `ok: true` plus verb-specific fields. The envelope tells a
caller "yes, the requested mutation landed"; the verb-specific fields
identify what it landed.

Verbs in this family: `init`, `new`, `close`, `open`, `update`,
`comment`, `label add`, `label rm`.

### Read verbs — bare payload

The read verbs emit the structured payload directly, with no envelope:

- `show` emits the `Bug` record verbatim.
- `ls` emits a JSON array of `Bug` records (possibly empty: `[]`).

The reasoning, from the in-source comment on `run_show`: the `Bug`
struct IS the structured payload. Wrapping it in `{"ok": true, "bug":
{...}}` would force every caller into one extra unwrap step with no
benefit — `show` either succeeds and emits a Bug, or fails and emits
an error envelope (see below). The success/failure distinction is
already carried by the exit code and stderr shape.

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

| `kind`                  | Exit | Source variant                | `details` keys           |
|-------------------------|------|-------------------------------|--------------------------|
| `not_a_jj_repo`         | 2    | `Storage::NotAJjRepo`         | `path`                   |
| `missing_bugs_bookmark` | 2    | `MissingBugsBookmark`         | `path`                   |
| `bug_not_found`         | 1    | `Storage::BugNotFound`        | `id`                     |
| `bad_id`                | 2    | `BadBugId` / `BadDepId`       | `value`, `field`         |
| `empty_body`            | 2    | `EmptyCommentBody`            | —                        |
| `empty_label`           | 2    | `EmptyLabel`                  | —                        |
| `missing_author`        | 2    | `MissingAuthor`               | —                        |
| `no_update_fields`      | 2    | `NoUpdateFields`              | —                        |
| `body_read_error`       | 2    | `BodyRead`                    | `from`                   |
| `cwd_error`             | 2    | `Cwd`                         | —                        |
| `probe_error`           | 1    | `Probe`                       | —                        |
| `invalid_input`         | 1    | `Storage::Invalid`            | —                        |
| `clock_error`           | 1    | `Storage::Clock`              | —                        |
| `io_error`              | 1    | `Storage::Io`                 | —                        |
| `json_error`            | 1    | `Storage::Json`               | —                        |
| `jj_error`              | 1    | `Storage::Jj`                 | —                        |

Adding a new variant to `CliError`? Pick a stable kind, add it to
the `kind()` match in `main.rs`, add a row above, and add a
test in the relevant `tests/<verb>.rs` file that pins it.

## Per-verb examples

Every example below is one success path and the most representative
error path for the verb. The integration tests under
`crates/jjf/tests/<verb>.rs` pin these shapes; if you change one
here, change the test too.

### `init`

```sh
$ jjf init --json
{"ok":true,"bookmark":"bugs"}
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

Error path — `bugs` bookmark missing (didn't run `jjf init` first):

```sh
$ echo body | jjf new --json -t x -F -
{"ok":false,"error":{"kind":"missing_bugs_bookmark","message":"the `bugs` bookmark does not exist in /repo; run `jjf init` first","details":{"path":"/repo"}}}
```

### `show`

Success path emits the `Bug` record verbatim — no envelope:

```sh
$ jjf show --json a3f9c01
{
  "id": "a3f9c01",
  "title": "fix the thing",
  "status": "open",
  "labels": [],
  "assignee": null,
  "dependencies": [],
  "body": "body\n",
  "comments": [],
  "created_at": "2026-06-21T22:00:00Z",
  "updated_at": "2026-06-21T22:00:00Z"
}
```

Error path — nonexistent id:

```sh
$ jjf show --json deadbee
{"ok":false,"error":{"kind":"bug_not_found","message":"bug not found in working copy: deadbee","details":{"id":"deadbee"}}}
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

Error path — running outside a jj repo:

```sh
$ jjf ls --json
{"ok":false,"error":{"kind":"not_a_jj_repo","message":"not a jj repo: /tmp/foo","details":{"path":"/tmp/foo"}}}
```

### `update`

```sh
$ jjf update --json a3f9c01 --title "renamed" --status closed
{"ok":true,"id":"a3f9c01","fields":["title","status"]}
```

The `fields` array lists the populated fields in field-declaration
order (`title`, `status`, `body`, `assignee`) — the same order the
storage layer lands the corresponding trailers on the resulting
commit.

Error path — nonexistent id:

```sh
$ jjf update --json deadbee --title x
{"ok":false,"error":{"kind":"bug_not_found","message":"bug not found in working copy: deadbee","details":{"id":"deadbee"}}}
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
{"ok":false,"error":{"kind":"bug_not_found","message":"bug not found in working copy: deadbee","details":{"id":"deadbee"}}}
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
