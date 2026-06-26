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
| B1     | _pending_ |     |     |
| B2     | _pending_ |     |     |
| B3     | _pending_ |     |     |
| B4     | _pending_ |     |     |
| C1     | _pending_ |     |     |
| C2     | _pending_ |     |     |
| C3     | _pending_ |     |     |
| C4     | _pending_ |     |     |
| D1     | _pending_ |     |     |
| D2     | _pending_ |     |     |
| D3     | _pending_ |     |     |

Verdict values: `finding` (ticket id in next col), `negative` (correctly
handled — recipe acts as future regression check), `deferred` (over
30-min budget; capture the partial state in evidence/).
