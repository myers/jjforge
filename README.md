# jjforge

A jj-native, agent-first issue tracker. CLI: `jjf`.

**Status:** post-MVP. The Rust binary at `crates/jjf/` covers
the full v1 verb set with `--json` on every command, push/pull
over standard git transport, and an op-space merge driver so
two clones can edit issues offline and converge without human
intervention. 178 workspace tests green; spec pinned in
`docs/storage-format.md`; output contract in `docs/cli-json.md`.

The project is self-hosted at
[github.com/myers/jjforge](https://github.com/myers/jjforge)
on a Forgejo instance, and it tracks its own work — see the
roadmap (`jjf show 9566f52`) for what's open and what's next.

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
- **zfs-workspace markdown-in-repo convention** — disciplines
  emerge from incidents, closure ties to a physical artifact,
  "no parser depends on the format."

## What jjforge tracks

The artifacts are **issues** (matching Gitea / GitHub / Beads
terminology — same word everyone else uses; we don't call them
"bugs" because most of what we track is roadmap and epic work,
not defects). One issue per work item: roadmap, epic, story,
research note, defect, whatever. Each issue has an id, title,
an optional slug (kebab-case orientation handle), a coarse
type (`bug` / `feature` / `epic` / `research` / `roadmap` /
`unspecified`), status (open / closed), body, labels,
dependencies, assignee, and an append-only comment thread.

As of `199ed91` (v1 → v2 storage spec) the Rust types, wire
trailers, bookmark, and on-disk paths all say "issue" too —
the type-level rename catches the code up to the prose. The
v2.1 update (`issue-type-and-slug-fields`) added the `type`
and `slug` fields on top of v2 without breaking the wire shape.

## First-time setup

Stable Rust toolchain (1.75+; whatever rustup gives you).

Install once:

```bash
# jj — Jujutsu, the substrate jjf shells out to
brew install jj   # or: cargo install --git https://github.com/jj-vcs/jj.git --locked --bin jj

# nextest — preferred test runner; isolates test processes,
# which matters for the integration tests that spawn real
# `jj` subprocesses
cargo install cargo-nextest --locked
```

Build and verify:

```bash
cargo build --release -p jjf
cargo nextest run --workspace
# fall back to `cargo test --workspace` if nextest isn't available
```

The binary lands at `target/release/jjf`.

## Quick tour

Initialize a jj repo (if you don't have one already), then
bootstrap jjforge on top of it:

```bash
jj git init my-project
cd my-project
jjf init                              # creates the `issues` bookmark
```

Create, list, read:

```bash
jjf new -t "the title" -F body.md -l epic            # body from file
echo "the body" | jjf new -t "the title" -F -        # body from stdin
jjf new --type feature --slug agent-ready -t "..."    # v2.1 — typed + slugged
jjf ls                                                # open issues
jjf ls --status all                                   # everything
jjf ls --label epic                                   # filter by label
jjf ls --type bug                                     # filter by type
jjf ls --slug agent                                   # substring-match slug
jjf show <id>                                         # one issue by id
jjf show agent-ready                                  # one issue by slug
jjf show <id> --json                                  # machine-readable
```

Every id-taking verb (`show`, `update`, `close`, `open`,
`comment`, `label add|rm`) accepts a slug in place of the 7-hex
id.

Mutate:

```bash
jjf update <id> --title "new title" --status closed   # multi-field, one commit
jjf close <id>                                        # convenience
jjf open <id>
jjf comment <id> -F note.md                           # append
jjf label add <id> backend
jjf label rm <id> backend
```

Sync across clones:

```bash
jjf remote add origin <url>                           # configure a remote
jjf push origin                                       # publish
jjf pull origin                                       # fetch + merge
```

Every verb takes `--json` and emits the envelope shape
documented in `docs/cli-json.md`. Errors under `--json` come
back as `{"ok": false, "error": {"kind": "...", "message": "...", "details": {...}}}`
so scripts can branch on `kind`.

## Architecture

Issues live as commits on a dedicated `issues` bookmark in
the underlying jj repo. Each issue is two files on that
bookmark:

- `issues/<7hex-id>.json` — the current rendered state
  (title, status, labels, dependencies, assignee, body).
- `issues/<7hex-id>.comments.jsonl` — one JSON object per
  line, append-only.

Every mutation lands as a new commit on the `issues`
bookmark. The commit description carries `Jjf-Op:` and
`Jjf-At:` git-trailers documenting which op ran and when
(nanosecond resolution). Both files and the trailers
travel automatically with standard `git push` / `git fetch`
of the bookmark.

On divergence (two clones modify the same issue offline),
the merge driver walks both heads' op chains in op-space,
resolves field-by-field with last-write-wins-by-`Jjf-At`,
takes the set-union of labels / dependencies, unions the
comment files chronologically, and lands a single merge
commit with one `Jjf-Op: merge` trailer per resolved issue.
There is no "body conflict" failure mode — the body is
just another LWW field.

For the full data shape: `docs/storage-format.md`.
For the merge model: `docs/storage-format.md` §6.
For the output contract: `docs/cli-json.md`.

## Why jj-native

- **Bookmarks are the unit of sync.** Push/pull a bookmark
  and you've round-tripped a whole issue tracker. No
  separate `refs/issues/*` namespace, no special transport.
- **The op log makes audit free.** `jj op log` already
  records every mutation; we just add structured trailers
  to commit descriptions so the per-issue history is
  reconstructable without protobuf.
- **Conflicts as data.** jj's conflict model is rich and
  programmatic, so the merge driver can be deterministic
  rather than asking humans to resolve markers.
- **Change IDs over commit IDs.** Issues survive history
  rewrites because the bookmark moves over the same files,
  not over the same commits.

## Planning lives in jjforge

Self-hosted on this very binary. The roadmap is the entry
point in any new session:

```bash
jjf show 9566f52
```

See `CLAUDE.md` for the orchestration conventions if you're
driving agent work against this repo.

## Repo layout

```
crates/
  jjf-storage/     # the storage primitives (init/read/write/history/list/merge)
  jjf-merge/       # the legacy file-bytes merge driver (kept as library)
  jjf/             # the `jjf` binary (clap-derive CLI over the storage layer)
docs/
  storage-format.md    # the on-disk spec
  cli-json.md          # the CLI output contract
  git-bug-cutover.md   # bridge to the archived pre-cutover planner data
experiments/       # throwaway scratch work; see CLAUDE.md
blog/              # Zola site at jjforge.dev (or wherever we end up)
```

## License

TBD.
