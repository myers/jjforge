# git-bug → jjforge cutover (2026-06-22)

This project ran on `git-bug` for three sessions (2026-06-21 through
2026-06-22) while jjforge was being built. On **2026-06-22**, the
planner switched from `git-bug` to `jjforge` per decision
`d12031c` (option B — start fresh). Historical data stays at
`refs/bugs/*` in git history and remains readable via
`git-bug bug show <id>`; future work goes into the `bugs` bookmark
(`jjf ls`, `jjf show <id>`).

This document is the **bridge** between the two worlds.

## What travelled — old → new ID mapping

These seven bugs were re-filed in jjforge during the cutover. The
body of each new bug is the original body, with subagent-flagged
follow-ups and key findings preserved. The status-update *comments*
that accumulated on each git-bug ticket did **not** travel — they
live in archived git history. Use `git-bug bug show <old-id>` to
read them.

| Old (`refs/bugs/`) | New (`bugs/`) | Title                                                                    |
| ------------------ | ------------- | ------------------------------------------------------------------------ |
| `6a65c0d`          | `9566f52`     | Roadmap: jjforge open epics in priority order                            |
| `72638a0`          | `f21b950`     | Epic: mvp-storage — bug records on the bugs bookmark                     |
| `436ac0a`          | `54caedd`     | Epic: mvp-sync — distributed edit + merge driver                         |
| `c4f7fcb`          | `a5f8122`     | Epic: mvp-cli — the jjf Rust binary                                      |
| `7cb5c14`          | `5a755ec`     | Epic: agent-ergonomics — ready, remember, MCP, subagent protocol         |
| `820998d`          | `99f01ed`     | Epic: multi-client — daemon, PWA, voice, VR                              |
| `303e15b`          | `585b84b`     | Epic: project-agent-orchestration — the long-running loop                |

The roadmap (`9566f52`) was re-filed last so its body could reference
the *new* epic ids in its priority-order list.

## What did NOT travel

### The migration decision ticket (`d12031c`)

Closed in git-bug, captures the three-option (migrate / start fresh /
dual-track) decision and the user's pick of option B. Record-only —
not re-filed because the decision is captured here and in the new
roadmap's "History" section. Read it via:

```sh
git-bug bug show d12031c
```

### The cutover ticket itself (`176f3c6`)

The git-bug-native ticket that did this cutover. Closed in git-bug
with the mapping table above in its closing comment. Not re-filed —
its purpose was the cutover, and that's done. Read via:

```sh
git-bug bug show 176f3c6
```

### The research record (`dcd4b57`)

The five 2026-06-21 research tickets (`a60bb95`, `2130de1`,
`8d3e045`, `dcd4b57`, `2120c06`) pinned the load-bearing storage
and sync decisions. Their verdicts are quoted in the new epic
bodies (`f21b950` for storage, `54caedd` for sync). The tickets
themselves stay frozen in git history at `refs/bugs/<id>*`:

| Old (`refs/bugs/`) | Verdict pinned in                                |
| ------------------ | ------------------------------------------------ |
| `dcd4b57`          | `bugs/f21b950.json` (mvp-storage)                |
| `a60bb95`          | `bugs/f21b950.json` (mvp-storage, audit shape)   |
| `2130de1`          | `bugs/f21b950.json` (mvp-storage, shell-out)     |
| `8d3e045`          | `bugs/54caedd.json` (mvp-sync, merge surprise)   |
| `2120c06`          | `bugs/54caedd.json` (mvp-sync, conflict parser)  |

`dcd4b57` was the one open research ticket at cutover — it stays
open in git-bug for historical accuracy but is record-only.

### Closed-but-load-bearing child tickets

Every closed git-bug ticket that shipped during the three pre-cutover
sessions stays in archived git history. The most load-bearing are:

| Old (`refs/bugs/`) | What it was                                          |
| ------------------ | ---------------------------------------------------- |
| `9a83841`          | storage-format-spec → `docs/storage-format.md`       |
| `da72aee`          | storage-write → `crates/jjf-storage/` (write path)   |
| `b650d74`          | storage-read-single → `crates/jjf-storage/` (read)   |
| `2f7e085`          | storage-read-history → `history.rs`                  |
| `8b12f9d`          | storage-bootstrap → `Storage::init`                  |
| `e2e473b`          | sync merge driver (file-bytes, legacy library)       |
| `07780aa`          | sync-remote-setup → `remote add/ls/rm` verbs         |
| `ed7f46b`          | sync-push-pull → `push`/`pull` verbs                 |
| `bfc732b`          | sync-conflict-fallback → op-space resolver           |
| `2bada67`          | cli-skeleton → `crates/jjf/` binary crate            |
| `1eeb83a`          | cli-new                                              |
| `85f4d42`          | cli-show + probe dedupe                              |
| `6b2b555`          | cli-ls + `Storage::list_ids`                         |
| `e346388`          | cli-status (close/open)                              |
| `26c25d2`          | cli-comment                                          |
| `537c65a`          | cli-label                                            |
| `fdd0c7f`          | cli-update                                           |
| `4f36251`          | cli-json-output + `docs/cli-json.md`                 |

Each landed as a commit on `main`; the commit message references the
ticket id. `git log --grep '<id>'` finds them.

## Reading historical bugs after cutover

`git-bug` continues to work against the archived `refs/bugs/*`
namespace as long as we never delete those refs (and we will never
delete them). Useful commands:

```sh
# Show one bug by id (any 7-char prefix works)
git-bug bug show 6a65c0d

# Enumerate every archived bug
git-bug bug

# List by old label
git-bug bug --label epic
git-bug bug --label research
git-bug bug --label epic:mvp-sync --status closed

# Find a commit by ticket id
git log --grep '8b12f9d' --oneline
```

The `bin/jjf` shell shim that was the git-bug front door
(`bin/jjf ls`, `bin/jjf show <id>`, etc.) stays in place
**read-only** until `cli-replace-shim` lands. Its payload changes
from "the planner" to "a read-only window into archived git-bug
data."

## Why we picked B (start fresh) and not A (migrate)

Decision recorded in `d12031c` (use `git-bug bug show d12031c`).
Short version: a real conversion tool that maps git-bug protobuf
ops to `Jjf-Op:` trailers is real engineering effort for a
one-time use; the closed bugs are historically interesting but
not load-bearing for next-session work; the open work is small
enough to re-file by hand; and the migration story matters more
for *future jjforge consumers leaving git-bug* than for jjforge
itself.

## Catastrophic safety rule

**Never delete `refs/bugs/*`.** Don't run `git-bug wipe`. Don't
prune those refs. They are the only copy of the pre-cutover
ticket history; once gone, the closing-comment narratives, the
research verdicts in full, and the subagent findings are gone
with them. The data is small (the entire `refs/bugs/*` namespace
weighs <1 MB); keep it forever.
