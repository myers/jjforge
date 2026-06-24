# Handoff: research the storage-out-of-tree-refs redesign

You are the orchestrator. Drive the loop in CLAUDE.md
against `487536a storage-out-of-tree-refs` — the research
ticket filed 2026-06-23 after the asterinas migration
surfaced jjforge's HEAD-drift problem.

## State of play

The host-asterinas migration (`cc2fa96`) was attempted
this session via the planned Haiku fan-out. It got as
far as `jj git init --colocate` + `jjf init` on
`~/p/asterinas-workspace`, which moved git HEAD to
`refs/jj/root` and wiped the working tree. Recovery was
manual; the operator lost some uncommitted edits.

User picked **research the redesign before re-attempting
migration**: jjforge's storage model ("files on the
`issues` bookmark") is the root cause. git-bug stored
data in `refs/bugs/*` — out-of-tree refs that never
touch HEAD. That pattern is the right long-term shape.

`487536a` (research ticket, types research, slugged
`storage-out-of-tree-refs`) carries the full problem
statement, the design space (per-issue refs vs whole-
bookmark ref vs git-bug-style commit-chain), and the
acceptance criteria. It's a `blocks` edge on `cc2fa96`
— the migration won't ship until the storage shape is
decided.

What landed this session (worth knowing):
- All four QA bugs closed: `e4e483b` `d1a01f0` `a902492`
  `277f559`. Tests 387 → 421. Commits `4aa787f` `2348d16`
  `1dce1ec` `434a2f8` + `ad924ca`.
- Trailer-injection audit hardened `assignee` and `label`
  write paths (originally unguarded), shared
  `validate_no_newlines` helper.
- Concurrent-write retry policy: `Storage::mutate` and
  `Storage::add_comment` retry once with re-read on
  `ConcurrentWrite`; slug-claim fails fast and upgrades
  to `SlugCollision`.
- `cc2fa96` got a status comment recording the migration
  pause + the rationale. Asterinas-workspace recovered
  (back at commit `2eb23ec6`, working tree restored).

## Surprises still warm in head

- **The asterinas tree is colocated now** — `.jj/`
  exists in `~/p/asterinas-workspace`. The colocate
  isn't doing anything actively but if you re-run
  `jjf init` from there it WILL re-drift HEAD. Either
  the redesign lands first, or rm `.jj/` before
  re-attempting.
- **The audit doc is still load-bearing.** It maps
  asterinas conventions to jjforge primitives. When
  the migration re-attempts (post-redesign), the
  Haiku-per-epic two-pass plan still applies — only
  the "where does jjforge write" answer changes.
- **The `JJF_ALLOW_SELF_HOST` guard + HEAD recovery
  pattern is exactly the bug the redesign kills.** If
  the redesign ships, the guard dies, the sibling-
  working-dir pattern dies, and the orchestrator's
  cognitive overhead drops a lot.
- **MEMORY.md edit + amdgpu-rs-port dir** in
  asterinas-workspace were lost during the colocate
  recovery (they were never committed; jj's snapshot
  skipped them due to large-PNG rejection). If those
  edits matter, ask the user.

## First move

`PATH="$HOME/p/jjforge/bin:$PATH" jjf show 487536a`.
The research ticket spells out the three storage shapes
and what each needs to answer. Start there. The natural
first sub-step is reading git-bug's `bug` package source
(under `refs/bugs/*` in this repo, via `git-bug bug
show`) to understand exactly how the commit-chain-per-
issue pattern works in practice.

If the research lands a clean design, the next move is
filing the implementation epic with sketched child
tickets (per the acceptance list).

## Pointers

- Research ticket: `jjf show 487536a` (or `jjf show
  storage-out-of-tree-refs`).
- Blocked migration: `jjf show cc2fa96`.
- HEAD-drift origin guard: issue `08cf14b`, CLAUDE.md
  "Operating in a colocated jj+git repo" section.
- git-bug prior art: `git-bug bug show <id>` works
  against archived `refs/bugs/*` in this repo. The
  cutover doc at `docs/git-bug-cutover.md` is the
  bridge.
- Current storage: `crates/jjf-storage/src/lib.rs` (the
  4-CLI write dance), `docs/storage-format.md` (v2
  spec), `crates/jjf-storage/src/trailer.rs` (op log
  shape — likely portable to the new refs).
- Asterinas audit: `docs/host-asterinas-audit.md`.
- This session's recovery story: `~/p/asterinas-workspace`
  is back at commit `2eb23ec6` with `.jj/` still
  present.

Next up: read 487536a, dig into git-bug's storage,
produce the design doc.
