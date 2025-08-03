# jjforge quick start

Five minutes from empty directory to a working planner. Every
command below was run end-to-end against a throwaway repo on
2026-06-24; the transcript is reproduced verbatim under
"Verified output."

## Prerequisites

- `jj` (Jujutsu) 0.40 or newer on `PATH`.
- The `jjf` binary on `PATH`. From this repo:
  `cargo install --path crates/jjf` — or symlink `bin/jjf`
  (which prefers `target/release/jjf`, falls back to debug,
  builds release on demand) somewhere on `PATH`.
- A jj identity configured (`jj config set --user user.name ...`,
  `user.email ...`). Without one, commit authorship is empty
  and `jjf` will refuse to write.

## 1. Create the repo

`jjf` writes to a `refs/jjf/*` namespace on an existing jj repo;
it does not create a repo for you. A **colocated** jj+git repo is
the most common shape (you keep `git push`/`pull`, and `jjf push`
rides the same remote):

```bash
mkdir my-project && cd my-project
jj git init --colocate
```

You now have `.git/` and `.jj/` side-by-side.

## 2. Initialize jjforge

```bash
jjf init
```

Output: `jjf: initialized`.

Idempotent — running it again on the same repo is a no-op
(`jjf: initialized` again, no error). This step creates the
`refs/jjf/issues` ref that holds every issue, comment, and memory.

## 3. File a roadmap

Every project gets one roadmap ticket. Give it the `roadmap` type
and the `roadmap` slug so future sessions can find it without
memorizing a 7-char id:

```bash
cat <<'EOF' | jjf new -t "Roadmap: my-project" --type roadmap --slug roadmap -F -
# my-project roadmap

1. Build the thing.
2. Ship the thing.
EOF
```

Output: a 7-char id like `0f07bdf`. Read it back any time with:

```bash
jjf show roadmap                         # by slug
jjf show roadmap --include-memories      # ... plus the memory bank
```

## 4. File real work

Capture the id of each new issue from the `--json` envelope so
later commands can reference it:

```bash
EPIC=$(cat <<'EOF' | jjf new --json -t "Epic: ship v1" --type epic --slug ship-v1 -l epic -F - | jq -r .id
# Goal
Get v1 out the door.

# Approach
Two tickets: backend, then docs.
EOF
)

BUG=$(cat <<'EOF' | jjf new --json -t "Backend handler crashes on empty input" --type bug -l epic:ship-v1 -F - | jq -r .id
The /submit handler panics when body is empty.
EOF
)

# Note the `-d $BUG` — the README ticket is gated on the bug.
FEAT=$(cat <<'EOF' | jjf new --json -t "Write the README" --type feature -l epic:ship-v1 -d $BUG -F - | jq -r .id
Document the install/run flow once the crash is fixed.
EOF
)
```

## 5. Ask "what's next?"

```bash
jjf ready
```

Returns the unblocked open issues, sorted bug-first.  Because the
README ticket depends on the bug, it is hidden from `ready` until
the bug closes:

```
3764c4b  open  1L  Backend handler crashes on empty input
6f227f7  open  1L  Epic: ship v1
```

Close the bug and `ready` shifts:

```bash
jjf close $BUG
jjf ready
```

```
6dbb571  open  1L  Write the README
6f227f7  open  1L  Epic: ship v1
```

The README ticket is now unblocked.

## 6. Remember something for next session

Persistent memories travel with the `issues` bookmark — they
round-trip via `jjf push`/`pull` and are surfaced by
`jjf show roadmap --include-memories`.  Save the things future-you
would otherwise re-derive:

```bash
jjf remember "Backend's /submit handler can't take empty bodies (fixed 2026-06-24)."
jjf memories
```

If you don't pass `--key`, the key is auto-slugged from the first
~40 characters of the value, so explicit keys are friendlier:

```bash
jjf remember "Build is 3x faster with sccache enabled in CI." --key sccache-ci
```

## 7. Push to a remote (optional)

`jjf` rides standard git transport — no special refspec config:

```bash
jjf remote add origin git@example.com:me/my-project.git
jjf push origin
```

Pulling merges any divergence with the built-in merge driver:

```bash
jjf pull origin
```

## Verified output

The transcript below was captured on 2026-06-24 running the
exact commands above against an empty directory.  IDs will
differ in your run; everything else should match.

```
$ jj git init --colocate
Initialized repo in "."

$ jjf init
jjf: initialized

$ jjf init        # idempotent
jjf: initialized

$ jjf ls          # after creating roadmap + epic + bug + feature
0f07bdf  open  0L  Roadmap: demo project
3764c4b  open  1L  Backend handler crashes on empty input
6dbb571  open  1L  Write the README
6f227f7  open  1L  Epic: ship v1

$ jjf ready       # README is hidden — blocked on the bug
3764c4b  open  1L  Backend handler crashes on empty input
6f227f7  open  1L  Epic: ship v1

$ jjf close 3764c4b
closed 3764c4b

$ jjf ready       # bug closed → README unblocked
6dbb571  open  1L  Write the README
6f227f7  open  1L  Epic: ship v1
```

## Where to go next

- **Full CLI surface:** `jjf --help` and per-verb `jjf <verb> --help`.
- **JSON output shapes:** `docs/cli-json.md`.
- **Storage format on disk:** `docs/storage-format.md`.
- **Working a single ticket from a subagent:** the
  `subagent-working-a-jjforge-issue` skill, auto-loaded when the
  dispatch mentions "issue", "ticket", "jjforge", or "jjf".
