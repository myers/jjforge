# Handoff: fix the two --parent bugs surfaced by the drop-epic-label feature

You are the orchestrator. Two open bugs sit above the line
under `epic:agent-ergonomics` (`5a755ec`); bugs-before-features
puts them ahead of the feature backlog. Both scoped tight
enough for one subagent each.

## State of play

This session shipped `epic:agent-ergonomics`'s "drop epic:<slug>
labels + add --parent filter" work via the plan at
`docs/superpowers/plans/2026-06-28-drop-epic-label.md`. Nine
commits landed on `main` (`852f2a38..5690c905`), pushed to
`origin`. The migration ran cleanly: 64→0 `epic:*` labels,
9→0 bare `epic` labels, 20 parent-child edges added. 633/633
workspace tests green. Final review: ready-to-merge, 0
Critical, 0 Important, 4 Minor (all filed as follow-ups).

While filing those follow-ups, I hit a brand-new bug: `jjf new
--parent <slug>` only accepts 7-hex ids. The other verbs that
gained `--parent` this session (`ls`/`ready`/`search`) DO
resolve slugs; `jjf new --parent` (from fj#3, predates this
session) doesn't. Filed as `b417864`.

## What's next (concrete first move)

```bash
cd ~/p/jjforge && ./bin/jjf ready --parent agent-ergonomics --type bug
```

Returns:

- **`b417864 new-parent-slug-broken`** — fix `run_new`
  (`crates/jjf/src/main.rs:2107-2114`) to call
  `resolve_handle(&storage, &raw)?` instead of bare
  `IssueId::parse`. Storage is opened AFTER dep-parsing today,
  so the fix may need a preflight reorder.
- **`095d60c parent-flag-bad-hex-silent`** — `--parent
  <bad-7-hex>` returns empty instead of `issue_not_found`.
  Mechanical: add `storage.read(&parent_id)?` after the
  resolver in each of `run_ls`/`run_ready`/`run_search`.

Both TDD-friendly. Run serially — both touch
`crates/jjf/src/main.rs`.

## Feature backlog under this epic (post-bugs)

`jjf ls --parent agent-ergonomics --status open --type feature`:

- `03df0f3 parent-matches-option-ref` — `Option<&IssueId>`
  clippy idiom. ~5min.
- `956a8d5 test-helpers-shared-module` — dedupe test helpers
  into `tests/common/mod.rs`.
- `4fe0406 drop-epic-label-migration-archive` — design call:
  commit `experiments/drop-epic-labels/run.sh` or leave
  gitignored. User decision.

## Pointers

- Spec: `docs/superpowers/specs/2026-06-28-drop-epic-label-design.md`.
- Plan + ledger: `docs/superpowers/plans/2026-06-28-drop-epic-label.md`,
  `.superpowers/sdd/progress.md`.
- The `--parent` flag is the new convention; CLAUDE.md's
  "Label scheme" section was rewritten to drop `epic:<slug>`.
- `experiments/drop-epic-labels/run.sh` is on disk (gitignored).
  The self-attribution edge case (epics carrying
  `epic:<their-own-slug>`) is the gotcha if you re-run this
  pattern elsewhere.
