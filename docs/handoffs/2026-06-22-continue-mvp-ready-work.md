# Handoff: continue MVP ready-work, you are the orchestrator

You are the orchestrator. Drive the loop in CLAUDE.md
("Orchestrating work") against the open issues in jjforge.

## State of play

The MVP storage + CLI + sync surfaces all shipped. v2.1 schema
just landed: `IssueType` enum + `slug` field on the record, all
plumbed end-to-end with `Storage::resolve` letting every
id-taking verb accept a slug. Commits `5f30706` (storage + CLI)
and `98f7dba` (docs sweep) are on `main`; 212/212 workspace
tests green.

What's open in the planner:

```
jjf ls --status all
```

The two load-bearing live ones:

- **`7100b51`** — the type+slug ticket you just merged. Still
  open. **Action needed before closing**: run the migration
  recipe to backfill type+slug on the 10 existing tickets (see
  the ticket body or the subagent's closing comment text in the
  previous session's transcript — it lists every `jjf update
  <id> --type X --slug Y` invocation). Then `jjf comment
  7100b51 -F -` with the four-section closing comment (the
  prior subagent left the text ready), `jjf close 7100b51`,
  status comment on `epic:mvp-storage` (`f21b950`), push.

- **`69d5e1b`** — `agent-ready`. The next feature ticket and
  the actual goal of this whole arc. Unblocked once `7100b51`
  closes. Its body references `Bug` types in places — those are
  stale post-rename; the subagent dispatched on it will need to
  read both the body and the comments to get the current shape
  (rename and type-field both landed since the body was
  written). The headline output `jjf ready` should now sort by
  the new `IssueType` (bug > feature > research > epic >
  unspecified, roadmap excluded — see `7100b51`'s ticket body
  for the agreed priority order).

## Open design call NOT to relitigate

The user explored "shared slugs always paired with id"
(`<id>-<slug>`) as a follow-up shape. We **decided to skip it
for now**. Don't refile it; if it comes back, the user will
raise it.

## Surprises still warm in head

- **Colocate drift is real.** Every mutating `jjf` call from
  inside this repo flips git HEAD to `refs/jj/root` and makes
  `git status` show every non-`crates/`/`experiments/` file as
  deleted. Guard in place (`refuse_self_hosted_write`) but you
  must set `JJF_ALLOW_SELF_HOST=1` for every mutating call.
  After any `jj git push` or mutating jjf call from inside this
  repo: `git symbolic-ref HEAD refs/heads/main && git reset
  --hard main` to recover. Same trick after the migration
  recipe runs.
- **The `jjf` binary needs rebuilding** after the merge —
  `cargo build -p jjf` once before the first migration call.
- The CLAUDE.md just gained a "Session handoffs" section and a
  "User-facing prompts" section (A/B/C never 1/2/3). Both
  apply to you.

## First move

1. `cargo build -p jjf` to refresh the binary.
2. Apply the migration recipe (10 `jjf update` calls). Sanity
   check with `jjf ls --type epic` after — should return 6 rows.
3. Close `7100b51` with the prior subagent's four-section
   closing comment.
4. Status-comment on `epic:mvp-storage` (`f21b950`).
5. `git push origin main` (commit `98f7dba` already pushed; the
   handoff commit you're about to make is new) and `jj git push
   --bookmark issues`.
6. Dispatch `69d5e1b` into a worktree.

Roadmap: `jjf show 9566f52`. Bugs-before-features (step 2 of
the orchestration loop) — there are no open bug-class issues
right now, so features (`agent-ready`) proceed.
