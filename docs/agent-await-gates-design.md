# agent-await-gates — design call

Status: design-only. Decision pinned in §6. Implementation
ticket follow-up referenced from the closing comment on
`08bc9eb`.

This doc evaluates whether jjforge should ship beads-style
external-signal gates — a way for an issue to park itself on
an external condition (a PR landing, a timer expiring, a human
responding) instead of staying "open" forever, where
`jjf ready` would surface it as if it were workable today.

The TL;DR is in §6: **skip it for v1**, with a single tactical
addition (`Status::Blocked` + a slim `--reason` flag on
`jjf block`) carried by a follow-up ticket. Full reasoning
follows.

## 1. The use case, in two concrete scenarios

### Scenario A — timer-based snooze (the cheapest gate)

A subagent just filed a follow-up ticket whose body says "give
the codebase 24 h to settle before working this." The agent
wants the new ticket to be invisible to `jjf ready` until
2026-06-24T00:00Z, then surface as if it had just been filed.

Today's options:
- Leave the ticket open. `jjf ready` surfaces it immediately.
  Whoever runs `jjf ready` next may pick it up before the
  cooldown elapses.
- Close it. Re-file at the right time. Requires someone
  remembering. Loses the history.
- Add a label `wait-until:2026-06-24`. Convention-only.
  Nothing reads it; `jjf ready` ignores it.

beads' answer: `bd defer <id> --until=2026-06-24T00:00Z`
sets `defer_until` plus `status=deferred`. `bd ready` filters
on both. No daemon — the filter is evaluated on each
`bd ready` invocation against `time.Now()`.

### Scenario B — gh:pr / fj:pr signal

A subagent finished a refactor and opened a PR. The next ticket
in the chain depends on that PR landing. The agent wants:

```
jjf await <next-ticket-id> --until fj:pr:chaos-inc/jjforge/42
```

…and the next ticket to be invisible to `jjf ready` until
PR #42 hits status MERGED. Today: same three escape hatches as
Scenario A, all unsatisfying. beads' answer: a `gate` issue
type with `await_type=gh:pr`, `await_id=<owner>/<repo>/<num>`,
polled by `bd gate check` (or the next `bd ready` call,
depending on configuration).

## 2. Comparison against "good enough" alternatives

Three escape hatches already exist or could exist with near-zero
work:

| Approach | Reads & surfaces? | Auto-clears? | Spec impact | Operator burden |
| --- | --- | --- | --- | --- |
| **A. `wait-on:<thing>` label convention** | `jjf ready` ignores it; `jjf ls --label wait-on:X` finds it | No — remove label by hand | None | Discipline only; nothing enforces |
| **B. Close + re-file on signal** | Closed issues hidden from `jjf ready`; refile carries new id | No — external watcher must re-file | None | Loses history; new id breaks dep edges |
| **C. `Status::Blocked` + comment** | Need to ADD `Blocked` variant; `jjf ready` filters it like Closed | No — operator `jjf open <id>`s | Yes — spec v2.5 status-enum bump | One verb to set; one to clear |
| **D. Full beads gates (`Awaiting` + `await_type/await_id`)** | New status variant filtered from ready; `jjf check-gates` clears | Yes for `timer`, `gh:pr`, etc.; manual for `human` | Yes — spec v2.5 status + two new fields | Bring-your-own auth for GH/FJ; polling architecture decision |

Honest reads on each:

**(A) `wait-on:<thing>` label.** Costs nothing. Works as
documentation. Does NOT solve the surfacing problem — the
ticket still appears in `jjf ready` until someone manually
adds a `Status::Blocked`-equivalent. Useful as a tag IN
ADDITION to one of the other approaches; not a complete
answer on its own.

**(B) Close + re-file.** Trades durability for surfacing.
Loses the issue id (breaks deps pointing at it), loses the
comment thread continuity, requires the external signal to
trigger a new `jjf new` call. Only viable for genuinely
disposable tickets (e.g. recurring chores). Not a serious
contender for the use case.

**(C) `Status::Blocked` + comment.** This is what we ALMOST
have. Today's `Status` enum is `Open | InProgress | Closed`.
Adding `Blocked` is a single-variant bump and a v2.5 spec
revision. The status flip can carry a free-text reason
(comment, body edit, or `--reason` flag on `jjf block`). The
ticket disappears from `jjf ready` (we filter on `Status::Open`
already; adding the filter for `Blocked` is a one-line patch).
It does NOT auto-clear — an operator runs `jjf open <id>`
when the external signal fires. For scenarios where a human
or agent IS in the loop (the common case in this project), the
clear is cheap: one verb call.

**(D) Full beads gates.** Gives auto-clear via polling. Pays
for it with: spec-bump cost (status + two new fields,
`await_type`, `await_id`, possibly `timeout` and `waiters`),
runtime auth dependency on the gate provider (GitHub / Forgejo
API), polling architecture decision (pull from `jjf ready`, a
separate `jjf check-gates` verb, or a daemon), and the hostname
namespace question (`gh:pr` vs `fj:pr` vs
`pr:<host>:<owner>/<repo>/<num>`).

## 3. Polling architecture (if we did ship full gates)

Three models, in increasing order of cost:

- **Pull, in-band.** `jjf ready` itself polls the gate provider
  for any open gate it encounters. Pros: one verb, no daemon.
  Cons: `jjf ready` becomes slow and network-dependent; a
  flaky GH/FJ API makes the headline agent verb flaky; auth
  must be present in every operator's environment.

- **Pull, out-of-band.** A separate `jjf check-gates` verb the
  operator runs manually or via cron. Pros: `jjf ready` stays
  fast and offline. Cons: someone has to run the verb;
  surfacing happens with operator-set latency.

- **Push.** An external webhook (a GH/FJ post-merge hook) calls
  `jjf clear <gate-id>` directly. Pros: zero polling; surfaces
  on real-time event. Cons: requires webhook plumbing per
  remote; out of scope for v1.

beads chose pull-out-of-band: `bd gate check` is a separate
verb that shells out to `gh` to query workflow / PR state, and
the user (or a cron) decides cadence. `bd ready` does NOT
poll.

For jjforge, IF we shipped this, pull-out-of-band is the
right shape. We never want `jjf ready` to be the slow verb.

## 4. Which gate types are worth v1 (if we shipped)

Ranked by defensibility:

1. **`timer`** — cheapest, most defensible. Pure
   `time.Now() > defer_until` comparison; no external auth.
   Solves Scenario A cleanly.

2. **`human`** — equivalent to `Status::Blocked`. No polling
   needed; operator clears manually. Adds nothing beyond what
   alternative (C) already gives us.

3. **`fj:pr`** — useful but bring-your-own-auth. The Forgejo
   instance at `github.com` is THIS PROJECT's
   remote, so this is the gate type a jjforge agent loop would
   most plausibly use. Requires a `forgejo` CLI or a raw API
   call (with token in env). Polling cost is low (a few HTTP
   calls per `jjf check-gates` invocation).

4. **`gh:pr` / `gh:run`** — same shape as `fj:pr` but requires
   `gh` CLI auth. Useful for projects mirrored to GitHub; not
   currently in use here.

Naming question: beads chose `gh:pr` / `gh:run` as gate-type
labels. For multi-host support, the cleaner shape is the
generalized one: `pr:<host>:<owner>/<repo>/<num>`. Concretely:
`pr:github.com/myers/jjforge/42`. This pushes
hostname into the gate ID itself, avoids the `fj:pr` vs `gh:pr`
namespace duplication, and lets the gate-check dispatcher
match on host rather than type prefix.

(beads' choice was forced by their CLI tool selection
— `gh` vs `gitlab` are different binaries. If we go with a
single HTTP backend, the host-in-ID shape is more elegant.)

## 5. Spec impact: new `Status` variant vs flag on `Open`

Two options for the on-disk shape:

- **New variant.** `Status::Awaiting` (or `Status::Blocked` for
  approach C). v2.5 bump. Reader-tolerant: pre-v2.5 readers
  hit serde's enum-deserialize failure on `awaiting`, which
  becomes `Error::Json` — they don't crash, but they don't
  understand the issue. Acceptable per the existing record
  versioning policy (v2.1 → v2.4 add fields, don't reshape).

- **Flag on `Open`.** Keep `Status::Open` as the wire value
  but add a new optional field (`awaiting: AwaitSpec | null`).
  Pre-v2.5 readers see an Open issue with an unknown extra
  field — serde tolerates unknown fields by default (the
  current `IssueRecord` does not use
  `#[serde(deny_unknown_fields)]`). Means `jjf ready` filters
  on `Open` AND `awaiting is None`.

The flag-on-Open approach is more conservative
(backward-compatible at the parser level) but conflates
"available" and "waiting" in the status field, which makes
operator-facing output messier (every status display has to
read two fields to know what to print).

The new-variant approach is cleaner and matches beads'
choice. v2.3 already proved we can bump the status enum
(`InProgress`); v2.5 doing the same is well-trodden.

**Independent of which approach:** the `agent-claim-atomic`
ticket (`c3cc807`) already shipped `Status::InProgress` in
v2.3. A future status bump is NOT folded with that; they
landed at different times. If we shipped gates, it would be
its own v2.5 bump, no folding required.

## 6. Recommendation

**Skip the full gates feature for v1.** Ship a slim,
tactical addition instead.

### What to ship

A single new variant on the existing `Status` enum:
**`Status::Blocked`** (wire spelling `blocked`). One new CLI
shape:

```
jjf block <id> --reason "<text>"      # status=blocked + reason as a comment
jjf open <id>                         # already exists; reuse for unblocking
```

`jjf ready` filters `Blocked` out the same way it filters
`Closed`. No polling. No new fields. No external auth.
v2.5 spec bump is small and well-precedented (mirrors the
v2.3 `InProgress` addition).

### What this gives us

- Scenario A (timer): the orchestrator runs
  `jjf block <id> --reason "wait 24h"` and revisits
  manually. Loses the auto-clear, but the orchestrator
  loop already revisits the queue regularly.
- Scenario B (PR landed): an agent reading PR status via
  `gh` or `forgejo` CLI in its OWN workflow runs
  `jjf open <id>` when the PR merges. No CLI-side polling
  needed; the gate-clearing logic lives in whatever script
  the agent is already running.
- `wait-on:<thing>` labels can be added on top for tagging
  — they remain useful as filters.

### What we DON'T ship

- No `await_type` / `await_id` fields.
- No `jjf check-gates` verb.
- No GH / FJ API integration in the CLI binary.
- No `Status::Awaiting` separate from `Status::Blocked`. If
  later we want auto-clearing gates, we extend `Blocked` with
  optional gate-spec fields, but don't pay that cost up-front.

### Why skip the full feature

1. **The auto-clear is the only thing full gates buy us over
   `Status::Blocked`**, and the auto-clear costs: a polling
   architecture, an auth-dependency, a hostname-namespace
   decision, and a CLI verb that touches the network. For
   a one-agent-per-loop project, the operator (or the agent
   itself) clearing the gate manually is fine.

2. **The use cases don't yet exist at scale.** Scenario A has
   come up zero times in the project's history. Scenario B
   has come up zero times. We are designing for a hypothetical
   future workflow.

3. **The escape hatches stack.** `Status::Blocked` plus a
   `wait-on:<thing>` label gives operators a discoverable
   filter (`jjf ls --label wait-on:fj-pr-42`) and a clear
   ready-suppression mechanism. The combination covers the
   "what is blocked on what" question without polling.

4. **We can ship gates later without breaking compat.** If
   the auto-clear becomes load-bearing, we add optional
   `await_*` fields to a `Blocked` issue in a v2.6 bump.
   v1-of-Blocked is forward-compatible with v2-of-gates.

### Follow-up ticket

A feature ticket: `agent-await-gates-impl` (filed as a
follow-up to this research ticket — see the closing comment
on `08bc9eb`). Scope:

- Add `Status::Blocked` variant (wire: `blocked`).
- New CLI verb: `jjf block <id> [--reason <text>]`. Sets
  status to blocked and (if `--reason` is given) posts a
  comment in the same multi-op commit.
- Existing `jjf open <id>` is reused for clearing.
- `Storage::list_ready` filters `Blocked` like it filters
  `Closed`.
- Spec bump: v2.5 status enum addition. Mirror v2.3's
  approach for backward-compat: pre-v2.5 readers surface
  `Error::Json` on the new variant, which is the existing
  contract for unknown enum values.

### Confidence

Moderately high on the recommendation. The risk in shipping
the slim approach is that we later wish we had auto-clearing
gates. Mitigation: the slim approach is a strict subset of
the full approach; extending it doesn't require a rewrite.

### What would re-open the question

- A second concrete use case beyond Scenarios A and B
  arises (e.g. an actual agent loop that needs to fan out
  N PRs and wait for them).
- The operator finds themselves running `jjf open <id>`
  more than a handful of times per week and asks for
  automation.
- A second remote (other than the chaos-inc Forgejo) gets
  wired in such that hostname namespacing becomes a real
  question.

If any of those land, file a follow-up that revisits
this doc, adds the `await_*` fields to the `Blocked`
shape, and ships `jjf check-gates`.

## 7. Sequencing against `agent-claim-atomic`

`agent-claim-atomic` (`c3cc807`) has already shipped
(`Status::InProgress` is live as of v2.3). The followup
ticket here would be a clean v2.4 → v2.5 bump on its own; no
folding required.
