# distributed-edit

Empirical probe for issue `8d3e045` — *what does jj actually do
when two clones concurrently edit the same bug data on Shape A
(the `bugs` bookmark)?*

`dcd4b57` recommended Shape A and explicitly scoped this issue to
Shape A only (Shape B is structurally infeasible; Shape C is
behaviorally identical to A but on the wrong bookmark).

## Scenarios

`test-five-scenarios.sh` runs the five scenarios listed in
`8d3e045`'s body plus a sixth scenario added during the probe:

| ID         | Setup                                                            | Layout |
| ---------- | ---------------------------------------------------------------- | ------ |
| S1         | Same field (title), different values                             | blob   |
| S2-blob    | Different fields (title vs status)                               | blob   |
| S2-lines   | Different fields (title vs status)                               | lines  |
| S3         | Identical idempotent edit (same field, same value)               | blob   |
| S4         | Concurrent `set-status closed`, both clones                      | blob   |
| S5-blob    | Comment in A, close in B                                         | blob   |
| S5-lines   | Comment in A (append), close in B (overwrite)                    | lines  |
| S6-lines   | Different comments in A and B (both append)                      | lines  |

`test-followup-distance-and-recovery.sh` adds two follow-ups
prompted by the five-scenario results:

| ID | Question                                                       |
| -- | -------------------------------------------------------------- |
| A  | Same as S2-lines but with 6 padding lines between title & status |
| B  | Walk through what jj exposes for an agent resolving S1 by hand   |

## Layouts

- **blob:** one JSON object per bug, written as a single physical
  line. `{"title":"...","status":"...","comments":[...]}`. What
  the prototype currently emits.
- **lines:** key-per-line layout, comments appended at EOF.
  ```
  title: ...
  status: ...
  comment: ...
  comment: ...
  ```

## Run

```sh
bash test-five-scenarios.sh
bash test-followup-distance-and-recovery.sh
```

Each script is hermetic — builds throwaway bare git remotes and
jj clones under `.scratch*/` (gitignored) and tees its full
stdout to `runs/*.transcript.txt`.

## Summary of findings

Posted as a comment on `8d3e045`. Headline: **jj's automatic
merge is not enough for bug data.** It conflicts on any edit
within a few lines of another edit, including edits to different
fields. Same-field, identical-value idempotent edits *do* merge
cleanly but both commits are kept in the audit log (not
deduplicated). The conflict markers are machine-parseable and an
agent can run a per-field merge policy on them without human
intervention. We need a higher-layer merge strategy.
