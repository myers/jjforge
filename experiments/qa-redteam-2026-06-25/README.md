# QA red-team sweep — 2026-06-25

Spec: `docs/superpowers/specs/2026-06-25-qa-redteam-sweep-design.md`
Plan: `docs/superpowers/plans/2026-06-25-qa-redteam-sweep.md`
Target commit: `338c2616`

## How to run

```bash
# Build jjf once, then run a sub-pass:
bash experiments/qa-redteam-2026-06-25/sub1-dataloss.sh

# Or run a single attack by sourcing the script and calling its function:
( source experiments/qa-redteam-2026-06-25/sub1-dataloss.sh; a1 )
```

Each attack writes evidence to
`experiments/qa-redteam-2026-06-25/.scratch/<attack-id>/evidence/`.
The `.scratch/` directory is gitignored.

## Finding index

| Attack | Verdict | Ticket | Notes |
| ------ | ------- | ------ | ----- |
| A1     | negative  |     | assignee+BOM+tab, comment author JSON metachar, memory BOM value all round-trip clean |
| A2     | negative  |     | LWW converges after full push→reject→pull→merge→push cycle; tiebreaker deterministic by commit SHA |
| A3     | negative  |     | set-title in injected trailer not applied; unknown op silently skipped per spec §5.2 |
| A4     | negative  |     | ls+ready both exit 0, emit unreadable-ref warning, exclude corrupt ref from output |
| B1     | negative  |     | ready --json byte-stable across two runs on pinned clock (5 issues, mixed types) |
| B2     | negative  |     | 40-cell matrix clean; closed/abandoned block/unblock yield typed `invalid input` errors (exit 1) |
| B3     | finding   | `121f48b` | mixed-kind cycle (A blocks B + B parent-of A) accepted, locks both out of ready; self-dep via slug correctly rejected; abandoned-blocker correctly frees dependent |
| B4     | negative  |     | all regex metacharacters (`q.ick`, `.`, `\bfox\b`, `(fox)`, `f*x`) treated as literals; no spurious matches |
| C1     | finding   | `de59159` | blob sentinel accepted as V3; `ls`/`ready` exit 0 despite corrupt ref — spec says "resolves to a commit" |
| C2     | negative  |     | all 5 Unicode titles (ZWSP, ZWJ, RTL-override, combining-acute, Cyrillic-а) round-trip byte-faithfully; `ls` exits 0 |
| C3     | negative  |     | 10MB title → ARG_MAX (OS harness limit, not jjf); 10MB body accepted (no cap, no panic); 1k labels + 1k comments: `ls`/`show` exit 0 within 30s |
| C4     | negative  |     | extra-field accepted (serde default, good forward-compat); missing-status + status-int both return typed `json_error` envelope, exit 1, no panic |
| D1     | negative  |     | all 6 error-class cases correct: issue_not_found×3 (exit 1), slug_not_found (exit 2), close-on-closed→invalid_input (exit 1), label-bad-id→slug_not_found (exit 2 per slug-resolution path) |
| D2     | negative  |     | `abandon --json` emits `{"ok":true,"id":"...","status":"abandoned"}` exactly per v2.7 spec |
| D3     | finding   | `88e4d6b` | `push_rejected` message embeds raw git stderr (version-dependent hint text + internal refspec paths); `details` only carries `remote`; hint placement inconsistent vs `concurrent_write` |

Verdict values: `finding` (ticket id in next col), `negative` (correctly
handled — recipe acts as future regression check), `deferred` (over
30-min budget; capture the partial state in evidence/).
