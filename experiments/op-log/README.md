# op-log experiment

Hands-on probe for issue `a60bb95`. Built a fake jjforge bug
collection (`demo/`), populated it with semantic operations, and
probed what `jj op log` does and does not give us.

## Reproduce

```sh
cd demo
jj op log --no-graph           # the noisy raw stream
jj op log --no-graph -T 'json(self)'   # machine-parsable JSON per op
jj op log --no-graph -T 'if(!snapshot, id.short() ++ " | " ++ description ++ "\n", "")'   # drop snapshot ops
jj log --no-graph bugs/aa6600b.json -T 'change_id.short() ++ " | " ++ description ++ "\n"'   # per-bug history (commits, not ops)
```

`clone/` is a fresh clone of `demo/`. After `jj git fetch`:

- commits travel (per-bug history reconstructible from `jj log <path>`)
- op log does NOT travel — clone has only its own 3 local ops vs.
  the source's 13.

## What's in the op log vs. the commit log

| Property                                            | `jj op log`                          | `jj log`                       |
|-----------------------------------------------------|--------------------------------------|--------------------------------|
| Description                                         | jj-CLI-flavored ("new empty commit") | whatever jjforge wrote         |
| Snapshot noise (1 extra op per command)             | yes                                  | no                             |
| Filter by path                                      | **no** (rejected by CLI)             | **yes** (positional path arg)  |
| Filter by revset / change id                        | no                                   | yes                            |
| JSON output                                         | yes (`json(self)` template)          | yes                            |
| Travels across `jj git push`/`fetch`                | **no — local-only**                  | yes (via bookmark refs)        |
| Per-actor signature                                 | hostname+username from local env     | jj commit author/committer     |
| `jj op abandon` can prune                           | yes (it's local plumbing)            | n/a                            |

## Why this matters

The issue's three options were: (a) thin `jjf ops <id>` over `jj op
log`, (b) side jsonl, (c) embed structured op metadata in commit
descriptions and parse back out.

(a) is out: op log is local-only and not path-filterable. A jjforge
user cloning a bug repo would see an empty audit trail. That defeats
"keep git-bug's audit feature."

(c) is the win: structured op metadata lives in the commit
description (or a trailer), commits travel with `jj git push`, and
`jj log <path-to-bug-file>` already gives us a per-bug op chain. We
get audit + distribution for free, with one parser pass over commit
descriptions. The `jj op log` is still there for "what local jj
plumbing happened" — useful for `jj undo`, debugging — but not the
audit surface.
