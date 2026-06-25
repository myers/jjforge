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
| A1     | _pending_ |     |     |
| A2     | _pending_ |     |     |
| A3     | _pending_ |     |     |
| A4     | _pending_ |     |     |
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
