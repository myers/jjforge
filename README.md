# jjforge

> **Alpha** — written by a coding agent, has been only lightly used so far. YOU HAVE BEEN WARNED.

## Quickstart

You'll need a recent Rust toolchain and [jj](https://github.com/jj-vcs/jj)
on your PATH (jjforge shells out to `jj`).

```bash
cargo install --git https://github.com/myers/jjforge.git jjf
cd /your/jj/repo
jjf init                              # plant refs/jjf/* namespace
echo "the body" | jjf new -t "first issue" -F -
jjf ls
```

For the full walk-through (push/pull across clones, dep edges,
the agent-ergonomics verbs like `ready` and `remember`), see
[docs/quickstart.md](docs/quickstart.md). Every verb takes
`--json` and emits the envelope shape documented in
[docs/cli-json.md](docs/cli-json.md).

## What this is

A jj-native, agent-first issue tracker. CLI: `jjf`.

**Status:** post-MVP. The Rust binary at `crates/jjf/` covers
the full v1 verb set with `--json` on every command, push/pull
over standard git transport, and an op-space merge driver so
two clones can edit issues offline and converge without human
intervention. Spec pinned in `docs/storage-format.md`; output
contract in `docs/cli-json.md`.

It tracks its own work — see the roadmap (`jjf show roadmap`)
for what's open and what's next.

## Inspirations

- **[git-bug](https://github.com/git-bug/git-bug)** —
  DAG-of-operations, fast-forward push, content-hash IDs that
  survive concurrent edits, separation of identity from issue.
  Distributed semantics that work. (jjforge ran on git-bug as
  its own planner during MVP; cutover happened
  2026-06-22 — see `docs/git-bug-cutover.md`.)
- **[Beads](https://github.com/steveyegge/beads)** (Steve
  Yegge) — `ready` as a first-class agent primitive,
  `remember` for persistent project memory, `--json` on every
  command, MCP-shaped, hierarchical issues, compaction-aware.
  Agents are the primary user. (`jjf ready` is sketched in
  `epic:agent-ergonomics`.)
- **[jj](https://github.com/jj-vcs/jj)** — stable change IDs
  that survive rewrites, operation log, automatic conflict
  resolution baked in. Don't reinvent what jj already does
  better than git.

## Architecture

Issues live in a `refs/jjf/*` ref namespace alongside the
underlying jj+git repo. One ref per issue, one ref per memory,
plus a sentinel:

- `refs/jjf/issues/<7hex-id>` — each issue's commit chain. The
  tip's tree carries two files:
  - `issue.json` — the current rendered state (title, status,
    labels, dependencies, assignee, body).
  - `comments.jsonl` — one JSON object per line, append-only.
- `refs/jjf/memories/<key>` — each persistent memory's commit
  chain, same shape.
- `refs/jjf/meta/format-version` — a sentinel commit marking
  the storage format. Presence tells readers the repo has
  been `jjf init`-ed.

Every mutation is a new commit on the relevant per-issue (or
per-memory) ref, advanced via `git update-ref` with a CAS
guard against the prior tip. Git HEAD never moves, so `jjf`
verbs are safe to run alongside live source work in the same
colocated repo. The commit description carries `Jjf-Op:` and
`Jjf-At:` git-trailers documenting which op ran and when
(nanosecond resolution). All of this travels with standard
git transport — `jjf push <remote>` and `jjf pull <remote>`
round-trip the whole `refs/jjf/*` namespace via the same
ssh / https that carries `refs/heads/*`.

On divergence (two clones modify the same issue offline),
the merge driver walks both heads' op chains in op-space,
resolves field-by-field with last-write-wins-by-`Jjf-At`,
takes the set-union of labels / dependencies, unions the
comment files chronologically, and lands a single merge
commit per resolved issue. There is no "body conflict"
failure mode — the body is just another LWW field.

(Pre-v3 storage put everything on a shared `issues` bookmark
in the working tree; v3 — `bd98097`, 2026-06-24 — moved to the
per-issue ref model above so `jjf init` can run safely in any
colocated repo without snapshotting the working copy.)

For the full data shape: `docs/storage-format.md`.
For the merge model: `docs/storage-format.md` §6.
For the output contract: `docs/cli-json.md`.

## Why jj-native

- **`refs/jjf/*` is the unit of sync.** `jjf push` /
  `jjf pull` round-trip the whole namespace via standard
  git transport — no special server, no protobuf, no LFS.
  Server-side it's vanilla git; clone with `git clone`,
  serve with Forgejo / Gitea / GitLab / GitHub all the same.
- **The op log makes audit free.** Every mutation is a
  commit; the chain on each per-issue ref IS the audit
  trail, with structured `Jjf-Op:` trailers so the
  per-issue history is reconstructable without protobuf.
- **Conflicts as data.** jj's conflict model is rich and
  programmatic, so the merge driver can be deterministic
  rather than asking humans to resolve markers.
- **Change IDs over commit IDs.** jj's underlying change-id
  model keeps the per-issue ref stable across history
  rewrites in the host repo — issues survive a rebase of
  unrelated git work.

## Planning lives in jjforge

Self-hosted on this very binary. The roadmap is the entry
point in any new session:

```bash
jjf show roadmap
```

See `CLAUDE.md` for the orchestration conventions if you're
driving agent work against this repo.

## Agent skills

jjforge ships skills for agents driving it (Claude Code, Codex,
Cursor, Gemini, Copilot CLI, and others that read the SKILL.md
convention).

Install with Vercel's `skills` CLI:

```bash
npx skills add myers/jjforge
```

The CLI auto-detects which agents you have installed and asks
which to target. The two skills that ship today:

- **`using-jjforge`** — how to read the roadmap, find unblocked
  work, create / update / query issues. Auto-loads on `jjf`,
  `jjforge`, `ready`, `remember`, or any `jjf <verb>` invocation.
- **`subagent-working-a-jjforge-issue`** — the closing-comment
  recipe and boundaries for a subagent dispatched to do focused
  work on a single issue.

Claude Code users can also install via the plugin marketplace
shape — this repo's `.claude-plugin/plugin.json` makes the same
`skills/` tree loadable as a plugin.

## Repo layout

```
crates/
  jjf-storage/     # the storage primitives (init/read/write/history/list/merge)
  jjf-merge/       # the legacy file-bytes merge driver (kept as library)
  jjf/             # the `jjf` binary (clap-derive CLI over the storage layer)
skills/            # agent skills (SKILL.md per skill; Vercel + Claude Code compatible)
docs/
  quickstart.md        # five-minute new-project walkthrough
  storage-format.md    # the on-disk spec
  cli-json.md          # the CLI output contract
  git-bug-cutover.md   # bridge to the archived pre-cutover planner data
experiments/       # throwaway scratch work; see CLAUDE.md
blog/              # Zola site at jjforge.dev (or wherever we end up)
```

## Building from source

Stable Rust toolchain (1.75+); `jj` on your PATH (Quickstart
covers this).

```bash
cargo build --release -p jjf
cargo nextest run --workspace
# fall back to `cargo test --workspace` if nextest isn't installed
```

The binary lands at `target/release/jjf`. `cargo nextest` is
preferred — it isolates test processes, which matters for the
integration tests that spawn real `jj` subprocesses (install:
`cargo install cargo-nextest --locked`).

## License

TBD.
