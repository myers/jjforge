# jj CLI per-invocation overhead

Microbenchmark for the jjforge "link vs shell-out" decision (issue 2130de1).

## Method

1. `jj git init --colocate test-repo` (one commit on top of root)
2. Loop a trivial read-only command 1000 times, wall clock with `/usr/bin/time -p`
3. Divide.

## Results (Apple Silicon Mac, jj 0.40.0)

| Command | 1000 runs (real) | Per-call |
|---|---|---|
| `jj log --no-graph -T commit_id -r @` | 14.63s | **14.6 ms** |
| `jj op log --no-graph --limit 1` | 14.78s | **14.8 ms** |

Smaller-N spot checks (5x each): every command was 10-20 ms. Floor is process
startup + repo discovery + git ref read, not whatever the command actually does.

## Implication for jjforge

At ~15 ms per call, a "list 100 open bugs" operation that fires one CLI invocation
per bug = 1.5 s. A "list 100 bugs" that batches into one revset = ~15 ms. The
design-on-the-CLI-side answer is: batch via revsets/templates, don't loop.

For an interactive `jjf` CLI: invisible.
For a sync daemon doing dozens of touches per second: visible — embed instead.
