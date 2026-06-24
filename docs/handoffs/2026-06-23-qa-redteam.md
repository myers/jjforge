# Handoff: ship the QA round, then unblock the asterinas migration

You are the orchestrator. Drive the loop in CLAUDE.md
("Orchestrating work") against the four open QA bugs.
Bugs-before-features per the standing rule; these block
the asterinas migration.

## State of play

A QA red-team round (2026-06-23, this session) tried to
break jjforge with adversarial input. Four issues filed,
labeled `qa-redteam`:

- **`e4e483b qa-title-validation`** — titles with
  embedded `\n` or `\0` accepted silently. Newline
  corrupts `jjf ls` text rows; null byte truncates the
  title before storage (data loss). Bug.
- **`d1a01f0 qa-dep-validation`** — `jjf dep add`
  accepts (a) phantom target ids, (b) self-deps. The
  self-dep case is a one-line DoS against any open
  ticket on a shared bookmark. Bug.
- **`a902492 qa-trailer-injection`** — a crafted title
  containing `\n\nJjf-Op: …\nJjf-Issue: …` lands as
  literal trailer lines in the commit description.
  Current parser strict enough to reject the injected
  op, but the surface is wrong. Blocked on `e4e483b`
  (the title-validation closes this by construction).
- **`277f559 qa-concurrent-write-ux`** — concurrent-write
  race produces a 12-line jj-internal "Internal error:
  Concurrent checkout" vomit instead of a typed
  `slug_collision` / `concurrent_write` error. UX bug,
  not data-loss. Soft-ship (not blocking migration).

The first three are blocks-edges on `cc2fa96
host-asterinas-migrate`. `jjf ready --label
epic:host-asterinas` correctly excludes the migration
until they close.

What landed earlier today (worth knowing, won't need to
re-touch):
- `agent-await-gates-impl` shipped (`Status::Blocked` +
  `jjf block/unblock`).
- `storage-scale-index` research + `storage-snapshot-cache`
  shipped. `jjf ready` warm = 52ms (was ~1.1s); cache
  rebuilds correctly under corruption.
- `host-asterinas` epic filed; `host-asterinas-audit`
  research closed with two soft-ship follow-ups
  (`bc6b9d9 jjf search`, `e726cde jjf stale`) and the
  big takeaway: 2 features ship, 30+ conventions
  collapse to labels.
- Skill renamed `subagent-working-a-git-bug-issue` →
  `subagent-working-a-jjforge-issue`. The new skill
  carries `JJF_ALLOW_SELF_HOST` + HEAD-recovery; don't
  re-explain those in dispatch prompts.

State of tests at handoff: 387/387 green; `read_history_walks_same_second_comment_appends`
was a parallel-load flake earlier, fixed via
`JJF_TEST_CLOCK_SECS` env-var clock pin (commit `ea18828`).

## Surprises still warm in head

- **The asterinas migration is currently `ready`-eligible
  if you ignore the blocks edges.** The dep cascade
  works: don't manually claim it; let `jjf ready` choose.
- **The QA tests created garbage in `/private/tmp/qa-test/scratch/`** —
  no impact, but if you re-run the red-team round, start
  fresh.
- **The QA-test repo has injection-shaped commit
  descriptions in its history.** Production planner does
  not; the audit verified. No backfill needed.
- **`a902492 qa-trailer-injection` is blocked by
  `e4e483b qa-title-validation`.** Land title-validation
  first; it likely closes most of trailer-injection by
  construction. Trailer-injection's acceptance section
  spells out the "verification + audit + defensive test"
  remaining work even after title-validation lands.
- **`277f559` is NOT blocking migration.** It's a UX
  improvement; ship it whenever convenient. It also
  proposes a one-retry policy on `Storage::add_comment`
  for race-resilience — modest scope but a real
  behavior change. Worth a closer read before
  dispatching.

## First move

Order of dispatch:

1. **`e4e483b qa-title-validation`** — small, isolated,
   storage + CLI validation. Closes most of `a902492`
   too. Dispatch first.
2. **`d1a01f0 qa-dep-validation`** — independent of
   title-validation; can run in parallel if you want a
   second worker.
3. **`a902492 qa-trailer-injection`** — after
   title-validation lands. Body is verification +
   audit + defensive tests; not a big rewrite.
4. **`277f559 qa-concurrent-write-ux`** — independent;
   touches the jj-shell-out error mapping path.

After all four close: `jjf ready --label epic:host-asterinas`
will surface `cc2fa96 host-asterinas-migrate`. That's
the next big move — the Haiku fan-out migration the user
asked for. Don't dispatch the migration in the same
round as the bug fixes; commit, push, fresh session.

## Pointers

- Roadmap entry: `jjf show 9566f52`.
- Epic context: `jjf show ec5a0a8`. Status comment with
  the bug list landed 2026-06-23.
- Audit doc: `docs/host-asterinas-audit.md` (the verdict
  doc from the closed `1588625`).
- Snapshot cache impl: `crates/jjf-storage/src/cache.rs`
  (just landed; useful to read before doing read-path
  perf work).
- QA test repo (if you want to repro): `/private/tmp/qa-test/scratch/`
  was where the round happened. Re-run from scratch if
  needed.
- Dispatch the subagent skill auto-loads on keywords
  "issue" / "ticket" / "jjforge" / "jjf" and carries the
  `JJF_ALLOW_SELF_HOST` + HEAD-recovery dance. Don't
  re-explain; the skill carries it.

Next up: ship the QA bugs in order, then the asterinas
migration.
