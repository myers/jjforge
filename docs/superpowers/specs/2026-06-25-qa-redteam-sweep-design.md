# QA red-team sweep — 2026-06-25

A full-spectrum adversarial QA pass against `jjf` at `main` (commit
`bb670611`). Structured by harm class, not by surface, so each
finding has a clear "why it matters" framing and the round catches
end-to-end interaction bugs the prior two rounds (2026-06-23,
2026-06-24) didn't.

## Goal

Find reproducible, impactful defects in `jjf` that the prior QA
rounds didn't cover. File each as a ticket the orchestrator can
work through the bugs-before-features loop. Stop at filing; do
not fix in this round.

## Non-goals

- Fixing findings inline. The orchestrator owns the fix loop.
- Filing every surprise. Bar is reproducible AND impactful
  (data loss, wrong answer, panic, exit-code mismatch, silent
  corruption, spec divergence). UX paper-cuts roll into one
  ticket if any surface.
- Touching the live `issues` bookmark. Every attack runs in a
  throwaway scratch repo.

## Scope

In:

- Every CLI verb's input boundary (titles, bodies, slugs,
  labels, dep ids, comment authors, memory keys, memory
  values).
- The v3 persistence path (`refs/jjf/issues/*`,
  `refs/jjf/meta/format-version`) and its read/write cycle.
- Sync: push, pull, merge driver, the five-scenario merge in
  `sync_v3.rs`.
- Status-machine reachability across all `(status, verb)`
  pairs.
- `--json` envelope and exit-code consistency against
  `docs/cli-json.md`.

Out:

- Performance tuning, benchmarking. Oversized-input attacks
  in sub-pass 3 are looking for panics/OOM, not throughput
  regressions.
- The MCP server (`epic:agent-ergonomics` work item, but
  not part of this pass's surface).
- Pre-cutover `git-bug` data on `refs/bugs/*`.

## Attack structure: four sub-passes by harm class

Each sub-pass runs sequentially; attacks within a sub-pass are
independent and can run in any order. Each attack is a Bash
function in `experiments/qa-redteam-2026-06-25/sub<N>-*.sh`.

### Sub-pass 1 — Data loss / silent corruption (`sev:data-loss`)

Goal: prove that data written round-trips faithfully through
write → push → pull → merge → read, and that corrupt ref state
either fails fast or surfaces as a typed warning.

- **A1. Free-form-field round-trip fuzz.** For each of
  `assignee`, comment `--author`, and memory value: write a
  payload containing `\t`, `\0` (where validation allows),
  JSON metacharacters, trailing whitespace, Unicode BOM
  (U+FEFF). Verify the value read back from `show --json`
  is byte-equal to what was written. Where validation
  rejects the input, confirm exit code is 2 (preflight)
  or 1 (runtime) per `cli-json.md`.
- **A2. LWW tiebreaker fuzz.** Pin `JJF_TEST_CLOCK_SECS` to
  the same value on both sides of a simulated split-brain.
  Land two concurrent `update --title` ops on the same
  issue. Pull. Confirm both sides resolve to the same
  winner, deterministically, and that the loser isn't
  silently dropped without an audit trail.
- **A3. Trailer parser forward-compat.** Hand-craft a commit
  on `refs/jjf/issues/<id>` containing: stray non-`Jjf-`
  trailers (`Signed-off-by:`, `Co-authored-by:`), v1
  `Jjf-Bug:` lines interleaved with v3 `Jjf-Op:` lines,
  and one unknown `Jjf-Op:` op-type. Confirm the record
  parses, known ops apply, and unknown ops surface as a
  typed warning rather than silent skip.
- **A4. Corrupt-ref regression check.** Hand-write a bad
  tree at `refs/jjf/issues/<id>` (e.g., missing
  `issue.json`, wrong field types). Run `jjf ls` and
  `jjf ready`. Expected per the 2026-06-23 fix: the ref
  surfaces as an `UnreadableRef` warning on stderr, the
  result set excludes it, exit code is 0. This is a
  regression check on `90f33c40`.

### Sub-pass 2 — Wrong answer (`sev:wrong-answer`)

Goal: prove the planner's *queries* return the right set under
edge cases.

- **B1. Sort stability on `created_at` ties.** Pin the test
  clock; create 5 issues at the same second with mixed
  types (`bug`, `feature`, `epic`). Run `jjf ready` twice.
  Output must be byte-identical across runs.
- **B2. Status-machine reachability.** Enumerate every
  `(status, verb)` pair: `open`, `closed`, `abandoned`,
  `blocked` × `comment`, `update --title`, `update
  --status`, `block`, `unblock`, `close`, `open`,
  `abandon`, `label add`, `dep add`. Each pair must
  either succeed cleanly or reject with a typed error
  and a documented exit code. Specifically watch:
  `abandon` on `closed`, `update --status open` on
  `abandoned`, `block` on `abandoned`, `comment` on
  `abandoned`, `unblock` on `open`.
- **B3. Dep-graph corner cases.** Mixed-kind cycles
  (`A blocks B`, `B parent-of A`); self-dep where the
  source is given as slug and the target as the same
  issue's id; dep edge into an `abandoned` issue (should
  `jjf ready` consider the dependent unblocked?).
- **B4. `memories <search>` with regex metacharacters.**
  Search terms containing `.`, `*`, `(`, `\`, `?`, `+`.
  Confirm literal-substring semantics, not regex; no
  panic; no spurious matches.

### Sub-pass 3 — Crash / panic (`sev:panic`)

Goal: no input or ref state should crash the process. Every
input gets a typed `Error` and a documented exit code.

- **C1. Format-version sentinel pointing at a blob.**
  Hand-write `refs/jjf/meta/format-version` to point at a
  blob OID instead of a commit. Run `jjf show`, `jjf ls`,
  `jjf ready`. Expected: typed error, exit 1, no panic,
  no `git2`/`gix` error leaking.
- **C2. Adversarial Unicode through title.** Post `validate_
  title` rejection of control chars: zero-width chars
  (U+200B, U+200D), RTL override (U+202E), combining
  diacritics on emoji, Cyrillic/Latin homoglyphs
  (а vs a). Confirm storage round-trips byte-faithfully
  and `jjf ls` doesn't break terminal rendering (visual
  inspection acceptable).
- **C3. Oversized inputs.** A 10MB title (should reject at
  validation — confirm no allocation explosion before
  validation runs); a 10MB body (no documented cap —
  observe behavior); 1k labels on one issue; 1k comments
  on one issue. Look for OOM, panics, latency cliffs.
  Findings here only if behavior is *broken*, not just
  slow.
- **C4. Malformed `issue.json` in a ref.** Tree contains
  `issue.json` with: extra unknown fields (forward-compat
  check), missing required fields, wrong types
  (`"status": 42`). Confirm typed error from the storage
  crate, not raw `serde_json::Error` leaking through
  `--json` stderr.

### Sub-pass 4 — Contract drift (`sev:contract-drift`)

Goal: every `--json` envelope and exit code matches
`docs/cli-json.md`. Every error variant is mapped.

- **D1. Envelope/exit-code sweep.** For every verb (`init`,
  `new`, `show`, `ls`, `ready`, `update`, `comment`,
  `close`, `open`, `abandon`, `block`, `unblock`,
  `label`, `dep`, `remote`, `push`, `pull`, `remember`,
  `recall`, `forget`, `memories`) and every error class
  (`issue_not_found`, `slug_not_found`, `invalid_input`,
  `concurrent_write`, `slug_collision`,
  `unreadable_ref`, `not_initialized`): capture stdout,
  stderr, exit code under `--json`. Diff against
  `docs/cli-json.md`. Flag: missing envelopes, generic
  exit 1 swallowing a typed variant, text-mode message
  drift from JSON `message` field.
- **D2. `jjf abandon --json` envelope shape.** Spec
  (`cli-json.md` line 15) mentions plain-text only.
  Confirm the JSON envelope exists, matches the
  mutating-verb family (`{"ok": true, "id": "..."}`),
  and is documented somewhere.
- **D3. `ConcurrentWrite.hint` stability.** Snapshot the
  hint text. If it's substring-matchable in a way that
  invites scripts to depend on it, file as a
  contract-drift finding so the message either gets
  stabilized or made variant-shaped.

## Repro infrastructure

```
experiments/qa-redteam-2026-06-25/
├── README.md                # finding index: id → recipe → ticket
├── lib.sh                   # mk_scratch_repo, build_jjf_release, assert_*
├── sub1-dataloss.sh         # functions a1, a2, a3, a4
├── sub2-wronganswer.sh      # functions b1, b2, b3, b4
├── sub3-panic.sh            # functions c1, c2, c3, c4
└── sub4-contractdrift.sh    # functions d1, d2, d3
```

- `lib.sh` carries `mk_scratch_repo` (`jj init --git` under
  `.scratch/<attack-id>/` + `jjf init`),
  `build_jjf_release` (idempotent `cargo build --release -p
  jjf`), and assertion helpers (`assert_exit`,
  `assert_json_field`, `assert_byte_equal`,
  `assert_stderr_matches`).
- Each attack function captures `stdout`, `stderr`,
  `exit-code`, and `observed-state` to
  `.scratch/<attack-id>/`. `observed-state` is a
  free-form file the attack writes — typically the
  post-mutation `jjf show --json <id>` output.
- `.scratch/` is already ignored via
  `experiments/**/.scratch/` in `.gitignore`. Committed
  files: `README.md`, `lib.sh`, `sub*.sh`. The `.scratch/`
  garbage is local-only.

## Per-finding ticket shape

```
jjf new \
  --type bug \
  --slug qa-2026-06-25-<short-slug> \
  -l qa-redteam-2026-06-25 \
  -l sev:<class> \
  -t "<short title naming the defect>" \
  -F -
```

Body uses the four-section recipe:

1. **Repro** — `experiments/qa-redteam-2026-06-25/sub<N>-*.sh`
   path + function name + inline 5-10 line minimal command
   sequence.
2. **Observed** — exit code, stderr excerpt, post-mutation
   ref state if relevant.
3. **Expected** — what the spec or `cli-json.md` says should
   happen, with a `file:line` citation.
4. **Severity rationale** — one sentence explaining why
   the `sev:` label fits.

`blocks` edges added only for `sev:data-loss` or `sev:panic`
findings, pointing at `cc2fa96 host-asterinas-migrate`
(those are migration-killers). `sev:wrong-answer` and
`sev:contract-drift` findings go to the orchestrator queue
without blocks edges.

## Proptest follow-up ticket

Filed once, regardless of round findings:

```
jjf new \
  --type feature \
  --slug qa-proptest-harness \
  -l qa-redteam-2026-06-25 \
  -l epic:agent-ergonomics \
  -t "qa-proptest-harness: stand up proptest against storage mutate/read surface" \
  -F -
```

Body sketches: target the storage crate's public mutate/read
API, generate `(Issue, [Op])` tuples, assert round-trip +
idempotence + status-machine invariants. Notes the 4-6h
setup cost and that this 2026-06-25 round chose to skip it
in favor of harm-class coverage.

## Exit criteria

- Per-attack: 30-min budget. Over budget → log as
  "deferred — over budget" in `README.md` and move on.
- Per sub-pass: all attacks reached terminal state
  (finding filed, negative result logged, deferred) +
  `cargo nextest run --workspace` is green.
- Round: all four sub-passes complete + proptest ticket
  filed + summary comment on `epic:agent-ergonomics` +
  `README.md` indexes every attack and its finding ticket
  id (or "negative") + `git push origin main` +
  `jjf push origin`.

## Stop-early triggers

A `sev:data-loss` or `sev:panic` finding **interrupts the
round** if it meets at least one of:

- Hits the merge driver, the v3 read path, or the format-
  version sentinel (load-bearing code shared across all
  issues).
- Reproduces from a CLI input that a normal user could
  plausibly produce (not just hand-crafted ref state).
- Causes a panic in a verb run during normal orchestration
  (`ls`, `ready`, `show`, `comment`, `update`).

When interrupted: file the ticket, surface to the user,
pause for direction. Do not keep mining for adjacent
findings when a load-bearing path is broken — a fix on
that path may change what's worth attacking next.

## Negative-result discipline

"Tried this and it correctly handled the input" is a
deliverable. Negative results go into `README.md` under
the attack id, so a future round knows the recipe was
run and doesn't waste cycles re-running it. Implicit
invariants documented this way save the next QA pass
hours.

## Calibration

Prior rounds: 2026-06-23 round = 4 findings, 2026-06-24
round = 13 findings. Expected band for this round: 8-15
findings. Fewer than 5 → likely under-attacking; more
than 20 → likely filing noise. The bar is "would a fix
meaningfully change a user's experience or unblock a
real workflow?" If no, roll into a single paper-cuts
ticket or skip.

## Closing comment

After all findings are filed and the round-state is
captured:

- One summary comment on `epic:agent-ergonomics`
  (`jjf comment 5a755ec -F -`) naming the round, finding
  count by severity, the proptest follow-up id, and the
  `experiments/qa-redteam-2026-06-25/` location.
- No epic-body edits. The body remains the goal; the
  comment is the round-state.

## Surfaces explicitly probed (target list from the surface map)

Drawn from the storage-crate walk; each lands inside one
of the four sub-passes:

- `validate_title()` Unicode coverage gap
  (`crates/jjf-storage/src/lib.rs:852`) → C2.
- `validate_no_newlines()` on memory value
  (`memory.rs:130`) and assignee
  (`lib.rs:1809`) → A1.
- `slugify()` zero-alphanumeric edge case
  (`memory.rs:74`) → C4 / D1.
- v3 CAS retry semantics (`lib.rs:475`) → A2.
- Trailer parser unknown-op silent-skip
  (`trailer.rs:155`) → A3.
- Format-version sentinel content unvalidated
  (`preflight.rs:83`) → C1.
- `ConcurrentWrite.hint` text stability
  (`v3_write.rs:82`) → D3.
- `list_ready()` sort stability
  (`lib.rs:3069`) → B1.
- `set_status()` permission matrix
  (`lib.rs:2078`) — abandon-on-closed,
  reopen-from-abandoned → B2.
- JSON envelope ad-hoc per verb
  (`main.rs:1229` exit_code_for) → D1, D2.
