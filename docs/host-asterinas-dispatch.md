# `host-asterinas` slice-dispatch recipe

The contract between the orchestrator and each Haiku slice agent
during the bulk migration of `~/p/asterinas-workspace/issues/`
into git-issues. Builds on `docs/host-asterinas-audit.md` (the
mapping table) by pinning down the rules the audit left to
agent judgment.

Seeded from the 2026-06-28 trial run on `epic-01-qemu-first-light`
and `epic-01-host-net` (18 issues across two slices). Every rule
below resolves an ambiguity the trial agents had to invent on
the fly.

## Working directory and binary

```bash
JJF=/Users/myers/p/jjforge/bin/iss
export ISS_ACTOR=haiku-slice-<slug>     # distinct per agent
cd /Users/myers/p/asterinas-workspace
```

All `iss` commands run from the asterinas-workspace dir so they
write to its `refs/jjf/issues/*`. NEVER `cd` into `~/p/jjforge`
for a `iss` call — that contaminates the git-issues planner.

## Slug policy

Source filenames → kebab-case slug, with a collision-avoidance
prefix for sentinel names that repeat across epic dirs:

- **Sentinel filenames** matching `^(PLAN|README|NOTES|TODO|SPEC)[-_]` →
  slug = `<epic-slug>-<filename-stem-kebab>`.
  - Example: `phase-aarch64/epic-01-qemu-first-light/PLAN-first-light.md`
    → slug `aarch64-qemu-first-light-plan-first-light` if a strict
    rule is desired, or the trial form `aarch64-qemu-first-light-plan`
    if the file-stem suffix is dropped. **Prefer the strict
    form** (`<epic-slug>-<filename-stem-kebab>`) for determinism;
    the trial-run form was a one-off.
- **Epic README** → slug = the epic dir name (e.g. `epic-01-host-net`).
- **Everything else** → slug = filename stem kebab-cased. The
  asterinas convention (`host-net-04a-sshd-scp-channel-hang.md`)
  is already kebab and globally unique within the tree.

Verify uniqueness before submission: `$JJF ls --slug <candidate>`
should return empty. Slugs MUST be unique across all open issues.

## Phase-label policy

Derive `phase:*` from the dir prefix `phase-…/`:

- `phase-N-<theme>/` (numeric) → `phase:N`.
  Examples: `phase-3-ethernet/` → `phase:3`. `phase-8-audio/` →
  `phase:8`.
- `phase-<word>/` (non-numeric) → `phase:<word>` literally.
  Examples: `phase-aarch64/` → `phase:aarch64`.
  `phase-k3-framework13/` → `phase:k3-framework13`.
  `phase-aarch64-openwrt/` → `phase:aarch64-openwrt`.
- The `**Phase:**` body header is informational; the
  dir-prefix label IS the canonical phase tag (it's stable;
  body fields drift).

## Status parse

Default and explicit-field rules. Trial-run gap was "what if
there's no `**Status:**` line"; fixed here.

**Explicit `**Status:**` line:**

| source root                                       | git-issues status | extra label              |
|---------------------------------------------------|----------------|--------------------------|
| `done`/`closed`/`fixed`/`resolved`/`shipped`/`landed` | `closed`       | —                        |
| `not-started`/`filed`                             | `open`         | —                        |
| `in-progress`/`active`/`harness`                  | `in-progress`  | —                        |
| `blocked`                                         | `blocked` (with `block_reason` from parenthetical) | — |
| `parked`/`deferred`/`superseded`/`workaround-in-place` | `open`         | `status-note:<root>`     |
| `sketch`                                          | `open`         | `maturity:sketch`        |
| `reopened`/`won`/`tx-no-egress` (one-offs)        | manual review  | flag in slice report     |

**No `**Status:**` line at all:**

- If the file has a parent (any non-epic child whose parent is
  in this slice): inherit the parent epic's parsed status.
- Otherwise: `open`.

Always note "status inherited from parent" or "status defaulted
to open" in the slice report so the operator can spot-check.

## Label rules

Apply each label only when its source header is present and
parseable. Skip silently when absent.

| source header                                  | label                                                                   |
|------------------------------------------------|-------------------------------------------------------------------------|
| `**Workflow:** [host+qemu]`                    | `workflow:host+qemu`                                                    |
| `**Workflow:** [silicon-only]`                 | `workflow:silicon-only`                                                 |
| `**Workflow:** mixed`                          | `workflow:mixed`                                                        |
| `**Lane:** X`                                  | `lane:<lowercased>` (e.g. `lane:b`, `lane:vf2-bringup`)                 |
| `**Track:** L2`                                | `track:l2`                                                              |
| `**Milestone:** M2.5`                          | `milestone:m2` (drop sub-decimal)                                       |
| `**Class:** [full]`                            | `maturity:full`                                                         |
| `**Severity:** medium`                         | `severity:<low\|medium\|high>` if prose normalizes; skip otherwise      |
| `**Priority:** medium-high — <prose>`          | `priority:high` if prose normalizes to high/medium/low; skip otherwise  |
| `**Estimated session budget:** 1 session`      | `size:small` (1 session = small, 2–3 = medium, 4+ = large; skip prose)  |
| `**Kind:** bug`                                | covered by `type=bug`; no separate label                                |
| `**Kind:** sketch`                             | `maturity:sketch`                                                       |
| `**Kind:** refactor` / `tech-debt`             | `kind:refactor`                                                         |

Phase label per the dir-prefix rule above (always applied).

## Type rule

- `README.md` of an `epic-*` directory → `type=epic`.
- Sentinel `PLAN-*.md`, `NOTES-*.md` etc. inside an `epic-*` dir →
  `type=feature` unless body says "bug" in title or `**Kind:** bug`.
- Plain `host-net-*.md` style child files → `type=feature` unless
  body says "bug".
- Bug indicators (any one): title contains `bug:`, body has
  `**Kind:** bug`, body has `**Status:** fixed`.

## Parent edge

Every child in an `epic-*/` directory gets `--parent <epic-slug>`
on `iss new`. Resolve via slug — `iss new --parent` now accepts
slugs as of `fbf66a82` (commit 2026-06-28).

## `Blocked by:` resolution (two-pass)

**Pass 1 — create everything in this slice:**

1. Create the epic first (no `--parent`). Capture id.
2. Create each child with title/slug/type/status/labels/parent.
3. Build the local-numeric → git-issues-id mapping table:
   - For `host-net-04a-…` → map `04a` to the id.
   - For files with just numeric prefixes (`02-…`) → map `02`.
4. Apply `close` / `block` status changes that didn't fit on
   the `new` call.

**Pass 2 — resolve `Blocked by:` cross-refs:**

For each child, parse `**Blocked by:**`:

| source value                                      | action                                                                                                |
|---------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| `none` / `—` / empty                              | skip                                                                                                  |
| `02, 06` (pure numeric list)                      | resolve each via mapping table; `$JJF dep add --kind blocks <child> <blocker>`                        |
| `02 (DHCP), 03 (TCP path)` (numeric + parentheticals) | strip parentheticals, resolve the numerics                                                            |
| `Phase 2A host-nvme-05` (cross-epic ref)          | SKIP; record in slice report under "cross-epic refs awaiting resolution"                              |
| Prose (`the wedge isn't reproducing`)             | SKIP; record in slice report under "prose refs"                                                       |
| Reference to a known-closed local id              | **Still create the `blocks` edge.** Closed-blocker status is data; the edge survives a status change. |
| Prose phrased as "satisfied" / "consumes" / "historical" (no resolvable id) | SKIP; the narrative belongs in the body                                                               |

The "still create the edge for closed blockers" rule resolves
the trial-run 04e ambiguity (referenced 04c which was closed —
trial agent skipped, this rule says create).

## Body-cap split recipe

Per `2542fdf migrator-body-comment-split` (filed 2026-06-28).

**Trigger:** body would exceed 64KB cap.

**Detection (try both):**

1. H2 starting with a date: `^## \d{4}-\d{2}-\d{2}\b`. Most
   common pattern in the asterinas tree.
2. `**Session:** YYYY-MM-DD` line: `^\*\*Session:\*\* \d{4}-\d{2}-\d{2}`.

**Recipe:**

- Find the cut points by either regex (chronological order
  preserved).
- Body = header block + goal + active plan + the most-recent
  session block(s) that fit under the cap.
- Older session blocks become comments, one per detected block,
  appended in source-chronological order.
- If neither regex matches and the body is still over cap: split
  at the last header (`^##`) before the cap and dump the tail
  into ONE comment with marker text. Surface in the slice report
  as "fallback split — operator review."

Markers (to avoid surprising the reader):

- Trailing body line: `*[Body continues in chronological
  comment(s) — earlier session blocks moved to keep body under
  the 64KB cap.]*`
- Leading comment line: `*[Continuation of body — split at
  <YYYY-MM-DD> session boundary.]*`

## Verification (run before reporting done)

The slice agent MUST complete this checklist before claiming
the slice migrated:

1. `$JJF ls --parent <epic-slug> --status all --json | jq 'length'`
   matches the file count for the slice (epic README + N
   children = N+1 issues).
2. For each id in the slice's mapping table, `$JJF show <id>`
   succeeds (no `issue_not_found`).
3. **Byte-fidelity spot-check on 2 random children:**
   - `$JJF show --json <id> | jq -r '.body' | wc -c` should
     match `wc -c <source.md>` to within ±300 bytes (split
     markers cost ~250 bytes; whitespace normalization a few
     more). FAIL the slice if any sample diverges more.
   - Add a "split-marker accounted for" note if the file was
     body-cap split.
4. **Dep-edge spot-check:** pick one child with a numeric
   `Blocked by:` list. Confirm `$JJF show <id>` lists every
   expected blocker.
5. Record verification command output verbatim in the slice
   report.

Failure on any check → halt, do NOT close the slice; surface in
the slice report under "verification failures" and let the
orchestrator decide whether to roll back the slice or hand-fix.

## Slice report

Write to `/Users/myers/p/jjforge/scratch/migration-report-<epic-slug>.md`.
Must contain:

- **Counts.** Files processed / expected; issues created;
  `blocks` edges created; refs skipped (broken out by reason).
- **Mapping table.** Local-numeric or filename → slug → git-issues
  id, one row per issue. The orchestrator's cross-epic
  resolution pass reads these tables.
- **Per-file detail.** Source path → id, status, labels.
- **Skipped `Blocked by:`** — every skipped ref, with reason
  (cross-epic / prose / "none" / fallback).
- **Verification output.** Commands run and their results.
- **Judgment calls.** Anything not covered by the rules above —
  exactly the things this doc should grow to cover.

## Constraints (mandatory)

- Do NOT push. The orchestrator owns transport.
- Do NOT edit source markdown in `~/p/asterinas-workspace/issues/`.
- Do NOT mutate git-issues' own source tree.
- Do NOT touch other epics outside the assigned slice.
- Set `ISS_ACTOR=haiku-slice-<distinct-slug>` so attribution
  works across parallel agents.

## Slice-dispatch prompt template

Copy-paste into the agent prompt:

> You are migrating one slice of the asterinas-workspace
> markdown tree into git-issues. Read
> `~/p/jjforge/docs/host-asterinas-dispatch.md` first — every
> rule you need is in that doc. Then read
> `~/p/jjforge/docs/host-asterinas-audit.md` for the mapping
> table context.
>
> **Your slice:** `<path/to/epic-NN-name>/`. Expected files:
> `<count>`. Expected statuses: `<rough mix>`.
>
> **Working dir:** `/Users/myers/p/asterinas-workspace`. Binary:
> `/Users/myers/p/jjforge/bin/iss`. Actor:
> `ISS_ACTOR=haiku-slice-<distinct-slug>`.
>
> Apply the dispatch recipe (slug policy, phase label, status
> parse, labels, two-pass `Blocked by:`, body-cap split,
> verification checklist, slice report). Halt and report rather
> than improvise if you hit something outside the rules.
>
> Report back under 250 words: counts, edges created, refs
> skipped (by reason), judgment calls, slice-report path.
