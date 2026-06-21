# jj-shellout-hello

Smallest reasonable Rust example proving the API shape jjforge will
actually use. Hello-world for issue `380c4e2`, **redirected from
linking `jj-lib` to shelling out to the `jj` CLI** following the
verdicts in `2130de1` and `a60bb95`.

## What this demonstrates

The original issue asked for three walkthroughs:

1. Open a jj repo from an absolute path.
2. Create a new change with a custom commit message on a named
   bookmark, **without checking that bookmark out**.
3. Read the operation log filtered to a given bookmark or change.

After `2130de1` (shell out, don't link) and `a60bb95` (audit via
commit-description trailers + `jj log <path>`, not `jj op log`),
all three map to `jj` CLI invocations:

| Issue ask | Shell-out shape |
|---|---|
| Open repo | `jj --repository <abs-path> …` on every call |
| Create change on bookmark, no checkout | `jj new <parent> -m '…trailer…'` + write file + `jj bookmark set` + `jj new root()` to step `@` off |
| Read per-bug history | `jj log <path> -T '<json template>'`, then parse `Jjf-Op:` trailers out of the descriptions |

The example runs end to end against a throwaway repo under
`$TMPDIR` and prints what it found. No fixtures are left behind in
the source tree.

## Run

```sh
cargo run --quiet
```

Expected output (change IDs / commit IDs differ each run):

```
scratch repo: /var/folders/…/jj-shellout-hello-<pid>
opened repo at /var/folders/…/jj-shellout-hello-<pid>
created change … on bookmark `bugs`: Create { … }
created change … on bookmark `bugs`: SetTitle { … }
created change … on bookmark `bugs`: AddComment { … }
working copy (@) after writes: <id> empty
--- bug history for bugs/aa6600b.json ---
  <commit>  jjf: bug aa6600b - create     parsed_op = Some(Create { … })
  <commit>  jjf: bug aa6600b - set-title  parsed_op = Some(SetTitle { … })
  <commit>  jjf: bug aa6600b - comment    parsed_op = Some(AddComment { … })

ok: round-trip Jjf-Op trailer through jj commit description works.
ok: `jj log <path>` is a usable per-bug audit surface.
cleaned up: …
```

The program asserts:

- The bug file has exactly three commits in its `jj log <path>`.
- All three carry a parseable `Jjf-Op:` trailer.
- The earliest entry parses as `JjfOp::Create`.

If `jj` changes in a way that breaks these invariants, this
example will fail loudly and tell jjforge it has work to do.

## API ergonomics — notes for the storage-shape issue (dcd4b57)

Things this example surfaced that the next issue should plan for:

- **`jj file write -r <change>` does not exist** in jj 0.40 (and
  0.42). The way to put content into a commit other than `@` is
  to stage it through the working copy. That makes "modify a bug
  file in a single CLI atomically without disturbing `@`"
  impossible — every mutation is at minimum: `jj new <parent>` →
  write file → snapshot → `jj bookmark set` → `jj new root()`,
  i.e. **4 `jj` CLIs per mutation** at the working-copy layer.
  At 14.6 ms each that's ~60 ms per mutation, still fine for
  interactive use but means batching matters for daemons.
- **`jj` resolves file path args relative to `cwd`, not the repo
  root**, even with `--repository <abs>`. Use the `root:`
  fileset prefix (e.g. `root:bugs/aa6600b.json`) to be
  unambiguous. The error message points you there, which is
  nice.
- **The `description(exact:…)` revset matches the full message
  including trailing newline**, so quoting the message exactly
  (we use Rust's `{:?}` debug-print on the string) is enough —
  but it's brittle. Capturing the new change_id by reading `@`
  immediately after `jj new` (as the current code does in the
  refactored path) is more robust.
- **Commit descriptions are stable through `jj describe` reflow
  for trailer-style `Key: value` lines.** This is the embedding
  format `a60bb95` suggested, and it works as expected.

## Dependency-graph weight

The original issue asked: "How big is the transitive crate tree
when you cargo-add `jj-lib`?"

Measured 2026-06-21 with `cargo tree --prefix none | sort -u | wc -l`:

| Crate | Transitive crates |
|---|---|
| `jj-lib = "0.42.0"` (latest)                | **193** |
| this example (`serde`, `serde_json`, std)   | **12**  |

A ~16× difference. Combined with `2130de1`'s API-stability
verdict, this reinforces the shellout call: linking `jj-lib`
adds a large transitive surface that we'd have to track-and-bump
on every `jj` minor release.

## What this example does **not** do

- It does not exercise `jj git push` / distribution. `a60bb95`
  already proved commits travel and `jj op log` does not; no
  need to re-demo here.
- It does not benchmark. See `experiments/jj-cli-overhead/`.
- It does not propose a final on-disk schema for bug files or
  for the `Jjf-Op:` trailer set. That's the next issue.

## Why this exists at all (not just close 380c4e2)

The issue's stated purpose was "a concrete API touchpoint before
we can scope the storage layer with confidence." That purpose
still holds under the new shell-out + commit-trailer model — but
the *artifact* it asked for (a `jj-lib`-linked binary) is no
longer the right thing to build. This example is the version
that actually answers the purpose given the new constraints, and
serves as documentation of the shape `jjf` will use.
