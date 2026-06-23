# scale-index — read-path scale benchmark

Companion to `docs/storage-index-design.md` and ticket `b9f628b`.

## What

Builds a synthetic `issues`-bookmark with N records (N ∈ {17, 100, 1000,
10000}) and times the read-path verbs that today loop one `jj file
show` per issue:

- `Storage::list_ready(default)` — `jjf ready`'s headline call.
- `Storage::resolve("synth-N-1")` — `jjf show <slug>` worst case.

Plus three batched / cached alternatives:

- One `jj log` with N `--files` filters (rebuild path probe).
- `jj edit bookmarks(issues)` + working-copy directory walk
  (snapshot-cache rebuild's actual mechanic).
- One `jj log` head-commit probe + reading a pre-built single-file
  cache (steady-state hit).

## Run

```bash
cargo run --release --bin bench
# or with a custom out dir / sizes:
cargo run --release --bin bench -- /tmp/scratch --sizes=17,100,1000
```

## Results (Apple Silicon Mac, jj 0.40.0)

| N | `list_ready` | `resolve` worst | wc-walk rebuild | cache hit |
|---|-----:|-----:|-----:|-----:|
| 17 | 0.38s | 0.19s | 0.09s | 0.014s |
| 100 | 2.23s | 1.10s | 0.14s | 0.014s |
| 1000 | 22.5s | 11.2s | 0.60s | 0.015s |
| 10000 | ~225s (extrap) | ~110s (extrap) | 5.1s | 0.025s |

Per-issue cost in `list_ready` is ~22 ms — matches
`experiments/jj-cli-overhead/README.md`'s ~15 ms floor per `jj`
invocation, times two file-show spawns per issue.

## Why this matters

Snapshot cache (head-commit probe + cache file) keeps the steady-state
read at ~25 ms even at 10k. A real index isn't needed; ship the cache.
See `docs/storage-index-design.md` for the full design and the
follow-up implementation ticket.

## Construction shortcut

We materialize N issue JSON + JSONL files into the working copy in one
commit, then move the `issues` bookmark to it. This bypasses the
write-path's commit-per-mutation discipline because the read-path
benchmark only cares about WHAT'S ON THE BOOKMARK, not how it got
there. Each synthetic `IssueId` is `e<6-hex of i>` (e.g. `e000010`).

## Scratch & cleanup

The benchmark writes scratch repos to `.scratch/` (gitignored). Build
artifacts go to `target/` (gitignored). Both are removed by `make clean`
or by hand.
