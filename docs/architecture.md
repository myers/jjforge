# git-issues architecture

How issues are stored, how mutations land, and why git-issues leans
on jj rather than git alone.

## Storage layout

Issues live in a `refs/jjf/*` ref namespace alongside the underlying
jj+git repo. One ref per issue, one ref per memory, plus a sentinel:

- `refs/jjf/issues/<7hex-id>` — each issue's commit chain. The tip's
  tree carries two files:
  - `issue.json` — the rendered state (title, status, labels,
    dependencies, assignee, body).
  - `comments.jsonl` — one JSON object per line, append-only.
- `refs/jjf/memories/<key>` — each persistent memory's commit chain,
  same shape.
- `refs/jjf/meta/format-version` — a sentinel commit marking the
  storage format. Presence tells readers the repo has been
  `iss init`-ed.

> **Vestigial tokens:** `refs/jjf/*`, `Jjf-Op:`, `Jjf-At:`, and the
> `jjf_at` field in JSON are named from before the project was renamed
> from `jjforge`/`jjf` to `git-issues`/`iss`. They are wire-format
> tokens frozen for backwards compatibility and must not be renamed.

## Write path

Every mutation is a new commit on the relevant per-issue (or
per-memory) ref, advanced via `git update-ref` with a CAS guard
against the prior tip. Git HEAD never moves, so `iss` verbs are
safe to run alongside live source work in the same colocated repo.

The commit description carries `Jjf-Op:` and `Jjf-At:` git-trailers
documenting which op ran and when (nanosecond resolution). All of
this travels with standard git transport — `iss push <remote>` and
`iss pull <remote>` round-trip the whole `refs/jjf/*` namespace via
the same ssh / https that carries `refs/heads/*`.

## Merge model

On divergence (two clones modify the same issue offline), the merge
driver walks both heads' op chains in op-space, resolves field-by-
field with last-write-wins-by-`Jjf-At`, takes the set-union of
labels / dependencies, unions the comment files chronologically,
and lands a single merge commit per resolved issue. There is no
"body conflict" failure mode — the body is just another LWW field.

## Why jj-native

- **`refs/jjf/*` is the unit of sync.** `iss push` / `iss pull`
  round-trip the whole namespace via standard git transport — no
  special server, no protobuf, no LFS. Server-side it's vanilla
  git; clone with `git clone`, serve with Forgejo / Gitea /
  GitLab / GitHub all the same.
- **The op log makes audit free.** Every mutation is a commit;
  the chain on each per-issue ref IS the audit trail, with
  structured `Jjf-Op:` trailers so the per-issue history is
  reconstructable without protobuf.
- **Conflicts as data.** jj's conflict model is rich and
  programmatic, so the merge driver can be deterministic rather
  than asking humans to resolve markers.
- **Change IDs over commit IDs.** jj's underlying change-id model
  keeps the per-issue ref stable across history rewrites in the
  host repo — issues survive a rebase of unrelated git work.

## See also

- [docs/cli-json.md](cli-json.md) — output contract for `--json`.
- [docs/quickstart.md](quickstart.md) — five-minute walkthrough.
- [`crates/iss-storage/src/lib.rs`](../crates/iss-storage/src/lib.rs) —
  the storage layer (source-of-truth for the on-disk record shape).
- [`crates/iss-storage/src/merge_ops.rs`](../crates/iss-storage/src/merge_ops.rs) —
  the op-space merge resolver.
