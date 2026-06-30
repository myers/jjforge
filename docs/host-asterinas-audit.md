# `host-asterinas` feature-gap audit

**Issue:** `1588625` (research). Parent epic: `ec5a0a8` (`host-asterinas`).
**Author session:** 2026-06-23.
**Scope:** which **beads primitives** does git-issues need to add so every
**asterinas-workspace convention** is queryable after we host the
`~/p/asterinas-workspace/issues/` tree (968 issues, 122 directories) on
the `issues` bookmark.

**Direction (user, 2026-06-23):** bend asterinas to fit beads-style
features in git-issues, NOT the other way around. Only ship the gaps the
actual data forces — every shipped feature is a spec bump, an enum
widening, new CLI surface, new error kinds, new tests, so the
calibration is lean-toward-skip. A convention used by 10 issues that has
a clean label workaround → label. A convention used by 200 issues that
labels can't express → ship.

## What asterinas writes

Header fields, by frequency (from `grep -rhoE "^\*\*[A-Z][A-Za-z _/&-]+:\*\*" .`):

| field | count | character |
|---|---:|---|
| `Status:` | 913 | freeform prose with date stamps, narrative ("done 2026-06-20", "active — Lane B's focus"). 11 distinct categorical roots normalize to {done 495, not-started 245, in-progress 71, filed 50, sketch 39, parked 29, fixed 23, closed 20, deferred 14, blocked 8, active 7, resolved 5, superseded 4, harness 8, workaround-in-place 1, reopened 1}. |
| `Workflow:` (alias `Tag:` 57, `Workflow tag:` 18) | 703+75 | three categorical roots: `[host+qemu]` (538), `[silicon-only]` (242), `mixed` (~30). Heavy parenthetical prose. |
| `Blocked by:` / `Blocks:` | 675/324 | cross-references — comma-separated local numerics (`02, 06`), full IDs (`as-vf2-host-10`), or `README.md` relative paths. 104 lines reference 2+ deps. Often prose-annotated ("(done)", "transitively"). |
| `Epic:` / `Parent:` / `Sub-epic:` | 486/60/14 | relative paths to `README.md` files — hierarchical parent. |
| `Phase:` / `Track:` / `Lane:` / `Milestone:` | 75/42/39/many | orthogonal classification axes; values are mostly prose like ``vf2-bringup` (correctness for a non-coherent port + a small perf cleanup)`. |
| `Estimated session budget:` / `Effort:` | 104/37 | sizing hint ("1 session", "1-2 sessions", "small / medium / large"). |
| `Found by:` / `Discovered by:` / `Surfaced by:` / `Spawned by:` / `Stems from:` / `Split from:` / `Related:` | 85+26+43+24+12+7+22 = 219 | provenance edges to other issues, with prose attribution ("pjdfstest `mkdir/00.t` tests 33-34"). |
| `Severity:` | 59 | bug-class only; freeform ("Low / latent — both gaps are unreachable today"). |
| `Class:` | 46 | `[full]` (44) vs sketch-by-convention; maturity tier. |
| `Priority:` | 12 | explicit, prose-heavy ("medium-high — collapses the diff-bed's 6-partition layout"). |
| `Found:` / `Filed:` | 26/26 | date stamps duplicating `created_at`. |
| `Kind:` | 25 | bug / sketch / plan classification — overlaps `type`. |
| `Design spec:` / `Design:` | 32+28 | external doc references. |

Tree shape: a **phase → epic → ticket** directory layout where the path
itself encodes hierarchy. README files per phase/epic carry an
issue-status table (manual mirror of the per-issue Status field) and a
dependency-graph ASCII diagram. Cross-cutting tickets live in
`cross-cutting/` — including permanent guardrails like
`dma-coherence-gate.md` that "never close" and are cited by every
DMA-touching ticket's `Blocked by:`.

## What git-issues currently has (v2.5)

- Issue record: `id` (7-hex), `title`, `slug`, `body`, `status` ∈ {Open, Blocked, InProgress, Closed}, `block_reason`, `type` ∈ {bug, feature, epic, research, roadmap, unspecified}, `labels` (free strings), `dependencies` (typed edges), `assignee`, `created_at`, `updated_at`.
- Four dep kinds: `blocks` (gates `iss ready`), `parent-child` (children inherit block when parent blocked), `related` (non-blocking soft link), `discovered-from` (provenance).
- Comments (append-only JSONL).
- Persistent memories (`iss remember`).
- Snapshot cache for fast reads at 968-issue scale (shipped in `69f3043`).
- Sync via plain `jj push/pull` on the `issues` bookmark.

## What beads has that git-issues doesn't

Beads CLI verbs surveyed via `ls cmd/bd/`. The notable ones we don't ship:

- `bd priority <n>` (numeric 0–4).
- `bd defer --until <time>` (time-based defer; new status `deferred`).
- `bd stale --days N` (find issues not touched in N days).
- `bd search <text>` (full-text body/title/comment search).
- `bd lint` (issue-body template validation).
- `bd duplicates` (content-hash dedup).
- `bd kv` (project-scoped key-value store; we already cover the rough need with `iss remember`).
- `bd issue-type` with 12 enum values (chore/task/molecule/gate/agent/role/convoy/...) vs our 6.
- `bd federation`, `bd convoy`, `bd molecule`, `bd wisp`, `bd hooked`, `bd ado` (multi-agent orchestration / external gates).
- `bd audit` (append-only JSONL of agent interactions).

## Mapping table

| asterinas convention | example | current git-issues mapping | beads alternative | verdict | follow-up ticket id |
|---|---|---|---|---|---|
| **Hierarchy (phase → epic → ticket)** | `phase-3-ethernet/epic-01-host-net/host-net-04-…` | parent-child dep edges + path-shaped labels (`epic:phase-3-ethernet`, `epic:phase-3-host-net`). | beads has bare `parent-child` (we have it); hierarchical IDs (`bd-abc.1.2`) which we deliberately reject. | n/a (labels + parent-child cover it) | — |
| **`Workflow:` tag (`[host+qemu]` / `[silicon-only]` / `mixed`)** | `**Workflow:** [silicon-only]` — 242 issues | label `workflow:host+qemu` / `workflow:silicon-only` / `workflow:mixed` | none | label | — |
| **`Phase:` field** | `**Phase:** 3 (ethernet) · epic-01-host-net` | label `phase:3` | none | label | — |
| **`Lane:` field** (Lane A / Lane B / Lane USB / `vf2-bringup` / …) | `**Lane:** B (otter, branch `mt76`)` | label `lane:b` / `lane:a` / `lane:vf2-bringup` | none | label | — |
| **`Track:` field** (L0 / L1 / L2 / L3a) — wifi-roadmap axis | `**Track:** L2 — the control surface` | label `track:l2` | none | label | — |
| **`Milestone:` field** (M0..M5) — aarch64-openwrt roadmap | `**Milestone:** M2.5` | label `milestone:m2` | none | label | — |
| **Status `not-started`** (245 issues) | `**Status:** not-started` | `Status::Open` | beads: `open` | n/a | — |
| **Status `in-progress`** (71 issues) | `**Status:** in-progress` | `Status::InProgress` (v2.3) | beads: `in_progress` | n/a | — |
| **Status `done` / `closed` / `fixed` / `resolved`** (495+20+23+5) | `**Status:** done — 2026-06-20` | `Status::Closed` | beads: `closed` | n/a | — |
| **Status `blocked`** (8 issues) | `**Status:** blocked` | `Status::Blocked` + `block_reason` (v2.5) | beads: `blocked` | n/a | — |
| **Status `parked` / `deferred` / `superseded` / `workaround-in-place`** (29+14+4+1 = 48 issues) | `**Status:** parked (hardening follow-up)` | no dedicated state. **Label workaround:** `status-note:parked` / `status-note:deferred` / `status-note:superseded` keeps the ticket Open (it's not Closed — work isn't done) and makes it queryable. The block_reason on `Blocked` doesn't fit (these aren't externally gated). | beads: `deferred` status + `bd defer --until`. `bd supersede`. | label | — |
| **Status `sketch`** (39 issues — issue-maturity, also overlaps `Class:`) | `**Status:** sketch — design hangs on unwritten APIs` | label `maturity:sketch` (and `maturity:full` for the 44 `Class: [full]` cases) | beads: none directly | label | — |
| **Status `filed`** (50 issues — recently created, no triage yet) | `**Status:** filed 2026-06-18` | `Status::Open` (filed = open + recently created). The `created_at` field carries the timestamp. | beads: none | n/a | — |
| **`Class:` (`[full]` vs sketch)** | `**Class:** `[full]` (kernel ABI gap-closure)` | label `maturity:full` / `maturity:sketch` (same axis as the `sketch` status above) | none | label | — |
| **`Blocked by: 02, 03, 04, 05, 08`** (104 multi-dep lines) | `**Blocked by:** 02, 06` | `dependencies: [{target, kind: blocks}, …]` — we already model this; migration ergonomic is the open question, not a feature gap | beads: `bd create --deps blocks:abc,def` | n/a (existing storage); ergonomic via repeated `--dep` flag | — |
| **`Blocks:` (inverse edge)** | `**Blocks:** 10; transitively as-vf2-hw-02` | derived: `iss ls --json` + filter on `dependencies.target == X` reconstructs blockers-of-X. No reverse-index verb today, but the data exists. | beads: blocked-by is computed at query time | n/a | — |
| **`Found by:` / `Discovered by:` / `Surfaced by:` / `Spawned by:` / `Stems from:` / `Split from:`** (219 edges) | `**Found by:** pjdfstest mkdir/00.t` | `discovered-from` dep edge (collapses all five aliases into one kind). The prose attribution ("pjdfstest mkdir/00.t") goes into the body, not a dedicated field. | beads: same `discovered-from` kind. | n/a | — |
| **`Related:`** (22 edges) | `**Related:** [`mt76-rs.md`](big-ideas/mt76-rs.md)` | `related` dep edge | beads: `bd relate` | n/a | — |
| **Permanent guardrail issue (`dma-coherence-gate.md`)** — "never close, must cite, cross-epic anchor" | `**Status:** not-started (permanent guardrail — stays here indefinitely)` + 5+ tickets cite it in `Blocked by:` | Open ticket + label `guardrail` + `related` edges from every DMA-touching ticket. The "never close" semantic is enforced by convention (orchestrator note), not by storage. | beads: none direct; the `pinned` status hints at this but isn't documented as guardrail-shaped | label | — |
| **`Estimated session budget:` / `Effort:`** (141 issues) | `**Estimated session budget:** 1 session` | label `size:small` / `size:medium` / `size:large` (or `size:1-session` / `size:2-session`). Migration script normalizes the prose. | beads: `estimated_minutes` (integer). | label | — |
| **`Severity:`** (59 issues, bug-class only) | `**Severity:** medium — blocks any large lab image build` | label `severity:low` / `severity:medium` / `severity:high` if the prose normalizes; otherwise leave the narrative in the body. Type `bug` already prioritizes via `iss ready`. | beads: no `severity`, only `priority`. | label | — |
| **`Priority:`** (12 issues, prose-heavy) | `**Priority:** medium-high — collapses the diff-bed's 6-partition layout` | label `priority:high` for the 12 explicit cases; type-driven ordering covers the rest. 12 / 968 ≈ 1.2% is below the ship-a-field threshold. | beads: `bd priority 0..4`. | label | — |
| **`Kind:`** (25 issues — bug/sketch/plan/refactor) | `**Kind:** bug` (6 cases) | `type` field already covers `bug`. `sketch` → label `maturity:sketch`. `tech-debt / refactor` → label `kind:refactor`. | beads: 12-value `issue_type` enum. | n/a / label | — |
| **`Design spec:`** / **`Design:`** (60 references to external docs) | `**Design spec:** docs/superpowers/specs/2026-06-04-…-design.md` | body text or comment — these are paths to docs outside the planner, no structured cross-ref needed. | beads: `external_ref` (single field). | skip | — |
| **`Found:`** / **`Filed:`** date stamps (52 references) | `**Found:** 2026-06-14` | `created_at` already tracks this. The asterinas dates are redundant (they were the planner's only timestamp record in flat-file land). | beads: `created_at`. | n/a | — |
| **Time-based defer** — Asterinas does NOT actually use time-based defer in any of the 14 `deferred` status occurrences (grepped "deferred until" → 0 hits). | — | no need | beads: `bd defer --until tomorrow` + watchers | skip | — |
| **Status-table mirror on each epic README** | epic README has a markdown table mirroring each child issue's title + status | `iss ls --label epic:<slug> --status all` — the storage IS the table | beads: same approach | n/a | — |
| **Full-text search across bodies + comments** | "what tickets discuss DMA coherence?" | `iss ls --json | jq -r ... | grep` works but doesn't scale to 968 issues for daily use | beads: `bd search <text>` | ship | `bc6b9d9` |
| **`iss stale --days N`** (issues not touched in N days) | finds abandoned in-progress and forgotten open tickets at 968-issue scale | no verb today; `iss ls --json | jq` + arithmetic on `updated_at` works | beads: `bd stale --days N` | ship | `e726cde` |
| **Issue maturity (sketch vs full) as a sortable axis in `iss ready`** | sketches outrank not-yet-full tickets for "what's actionable" | label `maturity:sketch` + manual filter: `iss ready --label maturity:full`. No code change needed; the existing `--label` filter on `iss ready` already does AND semantics. | beads: none direct | n/a (label + existing `iss ready --label` covers) | — |
| **`bd lint`** (template adherence per type) | asterinas issues follow strict body templates per phase | nice-to-have, not migration-critical. 968-issue tree's bodies vary wildly — no consistent template across the whole tree (Phase 1 vs Phase 5 templates diverge). | beads: `bd lint` | skip | — |
| **`bd duplicates`** (content-hash dedup) | asterinas has zero exact-content dups expected (each issue is hand-written prose) | nothing to dedup | beads: `bd duplicates --auto-merge` | skip | — |
| **`bd audit`** (agent-interaction JSONL) | orthogonal infra; asterinas tree has no equivalent | comments already log human/agent narrative; commit log is the audit trail | beads: `bd audit` | skip | — |
| **`bd kv`** (project-scoped key-value store) | asterinas issues store all metadata as prose; no structured per-issue metadata | `iss remember` covers the project-scoped use case | beads: `bd kv set/get` | skip | — |
| **Numeric priority 0–4 (`bd priority`)** | asterinas has 12 explicit priorities (1.2% of issues); the rest order by type | label `priority:high` for the 12 explicit cases; type-driven ordering covers the rest | beads: `bd priority` | skip | — |
| **Extended issue-type enum (chore/task/merge-request/molecule/gate/agent/role/convoy)** | asterinas: 6 `Kind: bug` uses; everything else is prose | our 6-value enum + labels for sub-categories | beads: 12-value `issue_type` | skip | — |
| **Federation (`bd federation`)** | multi-rig sync | git transport is the whole point | beads: federation | skip (per directive) | — |
| **Hierarchical IDs (`bd-abc.1.2`)** | path-based hierarchy | 7-hex flat ids are deliberate | beads: hierarchical | skip (per directive) | — |
| **Convoy / molecule / wisp (multi-agent orchestration)** | — | project-agent-orchestration, separate epic | beads: those | skip (per directive) | — |
| **Hooked status (`bd hooked` — external gate)** | asterinas: 0 uses | `Status::Blocked` + `block_reason` already covers the "gated on X" pattern | beads: `hooked` status | skip | — |

## Recommendation

### Features to ship

Two soft-ship features. Both are agent-ergonomic at 968-issue scale,
both have clean storage-layer designs, and neither blocks the migration
— operators can `iss ls --json | jq`/grep as a workaround until they
land. File as `epic:host-asterinas` children with a parent-child edge
to `ec5a0a8`. Neither carries a `blocks` edge to `cc2fa96`
(host-asterinas-migrate) because the migration itself doesn't depend on
them — they make life better after.

1. **`bc6b9d9` — `iss search <query>`: full-text search across body,
   title, and comments.** Asterinas's prose-tagged conventions
   (Workflow, Status narrative, "Found by" attribution) live in bodies.
   The snapshot cache makes this cheap. CLI shape: `iss search "dma
   coherence" --status open --json`. Storage: scan the snapshot,
   substring-match, return ids and match snippets.
2. **`e726cde` — `iss stale --days N`: surface issues not touched in
   N days.** With 71 currently-in-progress issues across the asterinas
   tree, identifying abandoned work matters for orchestration. Already
   exposed in storage via `updated_at`; just needs a verb. CLI shape:
   `iss stale --days 14 --status in-progress --json`.

### Features to skip

- **Numeric priority field.** 12 / 968 explicit uses (1.2%). Labels
  (`priority:high`) carry the 12 cases cleanly; type-driven `iss ready`
  ordering carries the other 956.
- **`Status::Deferred` / `Status::Parked` / `Status::Superseded`.** 48 /
  968 (5%). Labels (`status-note:parked` etc.) preserve queryability —
  `iss ls --label status-note:parked` works. Adding three enum values
  costs a spec bump, three new ops, error kinds, and CLI verbs for what
  one label namespace covers.
- **Time-based defer (`bd defer --until`).** Zero asterinas uses
  ("deferred until …" prose grep returns 0). Pure status-shaped defer
  is what asterinas does, and labels cover that.
- **Severity field.** 59 uses (6%), bug-only. Labels (`severity:medium`)
  carry the structured cases; the prose narrative belongs in the body
  regardless.
- **Sizing / effort field.** 141 uses (14%). Labels (`size:small`,
  `size:medium`, `size:large`) carry it without a new field. We are not
  yet sorting `iss ready` by size; if that need surfaces, a label-based
  sort works.
- **Extended issue-type enum.** Asterinas tree's `Kind:` field uses
  exactly one beads-extension value (`chore`-shape "tech-debt / refactor"
  — 1 instance). The other 24 collapse to our existing 6-value enum or
  to labels.
- **`bd lint` template enforcement.** Asterinas bodies vary across
  phases — there's no single template to enforce. Phase 1 templates
  differ from Phase 5 templates. Enforcement at the planner level would
  fight the actual writing pattern.
- **`bd duplicates` content-hash dedup.** Hand-written prose tickets do
  not collide on content hash. Asterinas tree has zero expected dupes.
- **`bd kv` per-issue or project-level key-value store.** `iss remember`
  covers the project-scope use case; per-issue arbitrary metadata is not
  used by asterinas.
- **`bd audit` agent-interaction log.** Comments + the git commit log
  already form an audit trail. Asterinas tree has no equivalent.
- **`bd hooked` external gate / `bd molecule` / `bd convoy` / `bd wisp`.**
  Project-agent-orchestration territory — separate epic.
- **Hierarchical IDs / federation.** Out per direction.

### Conventions that lose information either way

- **Status narrative.** `**Status:** done — 2026-06-20 (chip-init
  register-bus timeout RESOLVED + silicon-proven). The …` — three
  sentences of context after the status verb. Migrating, the verb maps
  to `Status::Closed` and the narrative goes into a closing comment.
  Loss: the status line no longer reads as prose at a glance; the reader
  has to open the issue. Mitigation: the closing comment is the canonical
  "what shipped" record (already the git-issues subagent recipe).
- **Multi-tag workflow.** `**Workflow:** mixed — `[host+qemu]` for prep,
  `[silicon-only]` for the campaign and fix-tail.` — split workflow
  across phases of a single ticket. Migrating: label `workflow:mixed` +
  body prose. Loss: per-phase workflow attribution. Mitigation: most
  consumers just want "does this ticket touch silicon?" — label answers
  that.
- **`Blocked by: 02, 06`** — local-numeric references inside an epic
  directory. The migrator must resolve local-numeric → 7-hex git-issues id
  per epic. Loss: terse epic-local references. Mitigation: only matters
  during migration; post-migration the 7-hex ids are stable.
- **Inline status table in epic README.** `phase-3-ethernet/epic-01-host-net/README.md`
  carries a markdown table mirroring each child's status. After
  migration, the README's status column would drift from the planner
  unless regenerated. Mitigation: drop the README status table and let
  `iss ls --label epic:host-net` be the source of truth.
- **Date stamps in prose (`**Filed:** 2026-06-18`, `**Closed:** 2026-06-19`).**
  Already covered by `created_at` / `updated_at` (Closed is just
  `updated_at` of the close transition). Loss: the explicit date stamp
  is no longer visible without a query. Mitigation: queries trivially
  surface them.
- **Prose-attribution provenance** (`**Found by:** pjdfstest mkdir/00.t
  tests 33-34, 2026-06-13.`) — the narrative WHICH-TEST-DID-THIS will
  live in the body, not in a structured field. Loss: pure-edge queries
  see only the source ID, not "which test of the source." Mitigation:
  the narrative is a click away in the body.

## Open questions for the migration ticket (`cc2fa96`)

- The migrator script needs a deterministic local-numeric → 7-hex
  mapping table for the per-epic references. Easiest: build the table
  during the directory walk, then resolve `Blocked by:` references in a
  second pass.
- Status normalization: regex-driven prose → categorical {Open,
  InProgress, Closed, Blocked} + `status-note:*` label for the
  parked/deferred/superseded/workaround tail.
- The 50 `filed` issues map to `Open` (they're just untriaged). The
  `harness` (8) and `active` (7) statuses collapse to InProgress.
  `reopened` (1) and `won` (1) and `tx-no-egress` (1) are one-offs;
  manual review.

## Acceptance check (from `1588625` body)

- [x] `docs/host-asterinas-audit.md` exists (this file).
- [x] Every distinct `**Status:**` categorical root is in the table with a mapping.
- [x] Every distinct `**Workflow:**` tag (`[host+qemu]`, `[silicon-only]`, `mixed`) is in the table.
- [x] Phase → epic → ticket hierarchy gets a verdict (labels + parent-child).
- [x] Follow-up implementation tickets filed for every "ship the feature" verdict.
