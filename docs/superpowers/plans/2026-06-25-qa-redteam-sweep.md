# QA red-team sweep — 2026-06-25 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Execute a full-spectrum QA red-team pass against `jjf` at `main` (`338c2616`), filing reproducible findings as `qa-redteam-2026-06-25`-labeled tickets, leaving fixes to the orchestrator.

**Architecture:** Bash harness under `experiments/qa-redteam-2026-06-25/` with `lib.sh` (helpers) + four sub-pass scripts (`sub1-dataloss.sh` … `sub4-contractdrift.sh`). Each attack runs against a fresh scratch repo, captures evidence to `.scratch/<attack-id>/`, and on a real finding the executor files a ticket via `jjf new` with the four-section recipe (Repro / Observed / Expected / Severity rationale). One follow-up feature ticket is filed for the proptest C-plan.

**Tech Stack:** Bash 5.x (the harness language — every macOS dev box has it), `jj` 0.40, `git`, the `jjf` release binary (built via `cargo build --release -p jjf`). Findings landed via `./bin/jjf` (which prefers `target/release/jjf`).

## Global Constraints

- All attacks run against scratch repos under `experiments/qa-redteam-2026-06-25/.scratch/<attack-id>/`. Never touch the live `issues` bookmark or this repo's working tree.
- `.scratch/` is already ignored via `experiments/**/.scratch/` in `.gitignore`. Committed files: `README.md`, `lib.sh`, `sub*.sh`.
- The finding bar is **reproducible AND impactful** (data loss, wrong answer, panic, exit-code mismatch, silent corruption, spec divergence). UX paper-cuts → roll into one ticket if any surface.
- Per-attack timebox: 30 min. Over budget → log in `README.md` as "deferred — over budget" and move on.
- Stop-early trigger: a `sev:data-loss` or `sev:panic` finding meeting any of {hits merge driver / v3 read path / format-version sentinel; reproduces from a plausible CLI input; panics in a verb run during normal orchestration} **interrupts the round** — file ticket, surface to user, pause.
- Calibration band: 8–15 findings. <5 → likely under-attacking. >20 → likely filing noise.
- Per-finding ticket shape (mandatory): `jjf new --type bug --slug qa-2026-06-25-<short> -l qa-redteam-2026-06-25 -l sev:<class> -t "<title>" -F -`. Body uses the four-section recipe (Repro / Observed / Expected / Severity rationale). `blocks` edge to `cc2fa96 host-asterinas-migrate` only for `sev:data-loss` or `sev:panic`.
- Severity label vocabulary, exact strings: `sev:data-loss`, `sev:wrong-answer`, `sev:panic`, `sev:contract-drift`.
- Spec file is the source of truth: `docs/superpowers/specs/2026-06-25-qa-redteam-sweep-design.md`. When in doubt, re-read it.

## File structure (what lands by end of round)

- `experiments/qa-redteam-2026-06-25/README.md` — finding index: per-attack-id row (`A1 | <verdict> | <ticket-id or "negative" or "deferred">`).
- `experiments/qa-redteam-2026-06-25/lib.sh` — shared helpers (`mk_scratch_repo`, `build_jjf_release`, `run_jjf`, `assert_*`, `record_evidence`).
- `experiments/qa-redteam-2026-06-25/sub1-dataloss.sh` — attacks `a1`, `a2`, `a3`, `a4`.
- `experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh` — attacks `b1`, `b2`, `b3`, `b4`.
- `experiments/qa-redteam-2026-06-25/sub3-panic.sh` — attacks `c1`, `c2`, `c3`, `c4`.
- `experiments/qa-redteam-2026-06-25/sub4-contractdrift.sh` — attacks `d1`, `d2`, `d3`.
- `.scratch/` directories (gitignored) — per-attack evidence.

Plus, on the `issues` bookmark: N finding tickets + 1 proptest feature ticket + 1 closing summary comment on `epic:agent-ergonomics` (`5a755ec`).

---

## Task 1: Harness scaffolding (lib.sh + README + dir)

**Files:**
- Create: `experiments/qa-redteam-2026-06-25/README.md`
- Create: `experiments/qa-redteam-2026-06-25/lib.sh`

**Interfaces:**
- Consumes: none (this is the bottom of the stack).
- Produces (these helpers are sourced by every sub-pass script):
  - `build_jjf_release()` — idempotent `cargo build --release -p jjf`; exit 1 on failure.
  - `mk_scratch_repo <attack-id>` — creates `.scratch/<attack-id>/`, runs `jj init --git` and `jjf init` inside it, prints the absolute path on stdout, sets globals `SCRATCH=$dir` and `EVIDENCE=$dir/evidence/`.
  - `run_jjf <args...>` — runs the release `jjf` binary against `$SCRATCH`, captures stdout/stderr/exit to `$EVIDENCE/last-{stdout,stderr,exit}`, returns the exit code.
  - `assert_exit <expected>` — checks `$EVIDENCE/last-exit` equals `<expected>`; on mismatch, prints both files and returns 1.
  - `assert_stderr_contains <substring>` — greps `$EVIDENCE/last-stderr`; on miss, prints the file and returns 1.
  - `assert_json_field <jq-path> <expected>` — runs `jq -r <jq-path>` on `$EVIDENCE/last-stdout`; compares string-equal.
  - `assert_byte_equal <file-a> <file-b>` — `cmp` of two files.
  - `record_evidence <name>` — copies `$EVIDENCE/last-*` to `$EVIDENCE/$name-{stdout,stderr,exit}` so multiple jjf calls per attack can be retained.
  - `pin_clock <seconds-since-epoch>` — exports `JJF_TEST_CLOCK_SECS=$1`.

- [ ] **Step 1: Create the directory**

```bash
mkdir -p experiments/qa-redteam-2026-06-25
```

- [ ] **Step 2: Write `lib.sh`**

Create `experiments/qa-redteam-2026-06-25/lib.sh` with this exact content:

```bash
#!/usr/bin/env bash
# QA red-team sweep — 2026-06-25 — shared helpers.
# Source from each sub-pass script: `source "$(dirname "$0")/lib.sh"`.

set -uo pipefail

# Where the round's harness lives. Resolved from this file's location.
QA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QA_REPO_ROOT="$(cd "$QA_ROOT/../.." && pwd)"
JJF_BIN="$QA_REPO_ROOT/target/release/jjf"

SCRATCH=""
EVIDENCE=""

build_jjf_release() {
  if [[ -x "$JJF_BIN" ]]; then return 0; fi
  echo "[lib] building jjf release binary"
  (cd "$QA_REPO_ROOT" && cargo build --release -p jjf >/dev/null) || {
    echo "[lib] FATAL: jjf release build failed" >&2
    return 1
  }
}

mk_scratch_repo() {
  local id="$1"
  local dir="$QA_ROOT/.scratch/$id"
  rm -rf "$dir"
  mkdir -p "$dir"
  EVIDENCE="$dir/evidence"
  mkdir -p "$EVIDENCE"
  (cd "$dir" && jj init --git >/dev/null) || {
    echo "[lib] FATAL: jj init failed in $dir" >&2
    return 1
  }
  (cd "$dir" && "$JJF_BIN" init >/dev/null) || {
    echo "[lib] FATAL: jjf init failed in $dir" >&2
    return 1
  }
  SCRATCH="$dir"
  echo "$dir"
}

run_jjf() {
  (cd "$SCRATCH" && "$JJF_BIN" "$@" \
    >"$EVIDENCE/last-stdout" 2>"$EVIDENCE/last-stderr")
  local rc=$?
  echo "$rc" > "$EVIDENCE/last-exit"
  return $rc
}

assert_exit() {
  local expected="$1"
  local got
  got="$(cat "$EVIDENCE/last-exit")"
  if [[ "$got" != "$expected" ]]; then
    echo "[assert_exit] FAIL: expected $expected, got $got" >&2
    echo "--- stdout ---" >&2; cat "$EVIDENCE/last-stdout" >&2
    echo "--- stderr ---" >&2; cat "$EVIDENCE/last-stderr" >&2
    return 1
  fi
}

assert_stderr_contains() {
  local needle="$1"
  if ! grep -qF "$needle" "$EVIDENCE/last-stderr"; then
    echo "[assert_stderr_contains] FAIL: needle '$needle' not in stderr" >&2
    cat "$EVIDENCE/last-stderr" >&2
    return 1
  fi
}

assert_json_field() {
  local path="$1"
  local expected="$2"
  local got
  got="$(jq -r "$path" "$EVIDENCE/last-stdout" 2>/dev/null || echo "<jq-error>")"
  if [[ "$got" != "$expected" ]]; then
    echo "[assert_json_field] FAIL: $path expected '$expected', got '$got'" >&2
    cat "$EVIDENCE/last-stdout" >&2
    return 1
  fi
}

assert_byte_equal() {
  if ! cmp -s "$1" "$2"; then
    echo "[assert_byte_equal] FAIL: $1 != $2" >&2
    diff "$1" "$2" >&2 || true
    return 1
  fi
}

record_evidence() {
  local name="$1"
  cp "$EVIDENCE/last-stdout" "$EVIDENCE/$name-stdout"
  cp "$EVIDENCE/last-stderr" "$EVIDENCE/$name-stderr"
  cp "$EVIDENCE/last-exit"   "$EVIDENCE/$name-exit"
}

pin_clock() {
  export JJF_TEST_CLOCK_SECS="$1"
}
```

- [ ] **Step 3: Write `README.md` with the finding index skeleton**

```markdown
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
```

- [ ] **Step 4: Verify the harness sources cleanly and builds the binary**

```bash
bash -c 'source experiments/qa-redteam-2026-06-25/lib.sh && build_jjf_release && echo OK'
```
Expected: `OK` printed. If the binary already exists, no Cargo work runs.

- [ ] **Step 5: Smoke-test `mk_scratch_repo` against a throwaway id**

```bash
bash -c '
  source experiments/qa-redteam-2026-06-25/lib.sh
  build_jjf_release
  mk_scratch_repo smoke
  run_jjf --version
  assert_exit 0
  echo OK
'
```
Expected: `OK` printed. The scratch dir at `.scratch/smoke/` will exist.

- [ ] **Step 6: Commit**

```bash
git add experiments/qa-redteam-2026-06-25/README.md experiments/qa-redteam-2026-06-25/lib.sh
git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: harness scaffolding

lib.sh provides mk_scratch_repo / run_jjf / assert_* helpers. README
carries the finding index skeleton. Sub-pass scripts land in
subsequent commits.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

---

## Task 2: Sub-pass 1 — Data loss / silent corruption

**Files:**
- Create: `experiments/qa-redteam-2026-06-25/sub1-dataloss.sh`
- Modify: `experiments/qa-redteam-2026-06-25/README.md` (update finding-index rows for A1–A4)
- File on `issues` bookmark: 0–N tickets labeled `qa-redteam-2026-06-25` + `sev:data-loss`

**Interfaces:**
- Consumes: `lib.sh` helpers (`build_jjf_release`, `mk_scratch_repo`, `run_jjf`, `assert_*`, `record_evidence`, `pin_clock`).
- Produces: per-attack verdicts in `README.md`; 0+ tickets on the planner.

### Attack recipes (the body of each function in `sub1-dataloss.sh`)

- **A1. Free-form-field round-trip fuzz.** Iterate over `assignee`, `--author`, memory value. For each, write a payload containing `\t`, `\0` (where validation allows), `","`, U+FEFF BOM, and a trailing space. Compare `show --json` and `recall --json` output to the input byte-for-byte.
- **A2. LWW tiebreaker fuzz.** Pin `JJF_TEST_CLOCK_SECS`. Create one issue. Clone the scratch dir to a sibling. In each, land `update --title` with different titles. Push both back to a shared bare remote. Pull each side. Confirm both sides converge to the same title (deterministic) and that the loser's title is auditable via comment trail.
- **A3. Trailer parser forward-compat.** Use `git plumbing` to write a commit on `refs/jjf/issues/<id>` containing trailer lines interleaved as: `Signed-off-by: x <x@y>`, `Jjf-Bug: <id>` (v1), `Jjf-Op: set-title v=foo`, `Jjf-Op: unknown-op-type v=bar`. Run `jjf show --json <id>`. Verify the known op applied, the unknown op surfaced as a warning on stderr (or recorded in `issue.json`), exit code is 0 or 1 (not panic).
- **A4. Corrupt-ref regression check.** After creating one issue, hand-write its tree to be missing `issue.json`. Run `jjf ls` and `jjf ready`. Verify both exit 0, stderr contains an `unreadable_ref` warning naming the ref, and the ref is excluded from the result set.

- [ ] **Step 1: Write `sub1-dataloss.sh` with the four attacks**

Create `experiments/qa-redteam-2026-06-25/sub1-dataloss.sh`:

```bash
#!/usr/bin/env bash
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
build_jjf_release || exit 1

# -----------------------------------------------------------------------------
# A1. Free-form-field round-trip fuzz — assignee, --author, memory value
# -----------------------------------------------------------------------------
a1() {
  mk_scratch_repo a1 >/dev/null

  # 1.a — assignee with embedded tab + BOM + trailing space.
  run_jjf new -t "a1 assignee target" -a $'\xef\xbb\xbfalice\tbob '
  if [[ "$(cat "$EVIDENCE/last-exit")" != "0" ]]; then
    record_evidence "assignee-rejected"
    echo "[a1] assignee with tab+BOM rejected — capture verdict in README"
  else
    record_evidence "assignee-accepted"
    local id
    id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/assignee-accepted-stdout" | head -1)"
    run_jjf show --json "$id"
    record_evidence "assignee-show"
    # Compare assignee byte-for-byte with the input we sent.
    local got
    got="$(jq -r '.assignee' "$EVIDENCE/assignee-show-stdout")"
    local expected=$'\xef\xbb\xbfalice\tbob '
    if [[ "$got" != "$expected" ]]; then
      echo "[a1] FINDING: assignee round-trip lossy."
      echo "[a1]   expected: $(printf '%q' "$expected")"
      echo "[a1]   got:      $(printf '%q' "$got")"
    fi
  fi

  # 1.b — comment --author with embedded JSON metachar and tab.
  run_jjf new -t "a1 author target"
  local cid
  cid="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  echo "comment body" | run_jjf comment "$cid" -F - --author $'eve","x":"y'
  record_evidence "author-set"
  run_jjf show --json "$cid"
  record_evidence "author-show"
  local author_got
  author_got="$(jq -r '.comments[-1].author' "$EVIDENCE/author-show-stdout")"
  local author_expected=$'eve","x":"y'
  if [[ "$author_got" != "$author_expected" ]]; then
    echo "[a1] FINDING: comment author round-trip lossy."
    echo "[a1]   expected: $(printf '%q' "$author_expected")"
    echo "[a1]   got:      $(printf '%q' "$author_got")"
  fi

  # 1.c — memory value with BOM + trailing space.
  run_jjf remember $'\xef\xbb\xbfBOM-prefixed memory value ' --key bom-memory
  record_evidence "memory-set"
  run_jjf recall bom-memory
  record_evidence "memory-recall"
  local mem_got
  mem_got="$(cat "$EVIDENCE/memory-recall-stdout")"
  local mem_expected=$'\xef\xbb\xbfBOM-prefixed memory value '
  if [[ "$mem_got" != "$mem_expected" ]]; then
    echo "[a1] FINDING: memory value round-trip lossy."
    echo "[a1]   expected: $(printf '%q' "$mem_expected")"
    echo "[a1]   got:      $(printf '%q' "$mem_got")"
  fi

  echo "[a1] done; evidence at $EVIDENCE"
}

# -----------------------------------------------------------------------------
# A2. LWW tiebreaker fuzz under JJF_TEST_CLOCK_SECS pin
# -----------------------------------------------------------------------------
a2() {
  pin_clock 1735000000
  mk_scratch_repo a2 >/dev/null
  run_jjf new -t "a2 original title"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  record_evidence "create"

  # Set up a bare remote and two siblings.
  local bare="$QA_ROOT/.scratch/a2-bare.git"
  rm -rf "$bare"
  git init --bare "$bare" >/dev/null
  (cd "$SCRATCH" && "$JJF_BIN" remote add origin "$bare" >/dev/null)
  (cd "$SCRATCH" && "$JJF_BIN" push origin >/dev/null)

  local side_a="$QA_ROOT/.scratch/a2-side-a"
  local side_b="$QA_ROOT/.scratch/a2-side-b"
  rm -rf "$side_a" "$side_b"
  cp -R "$SCRATCH" "$side_a"
  cp -R "$SCRATCH" "$side_b"

  SCRATCH="$side_a" EVIDENCE="$side_a/evidence" run_jjf update "$id" --title "A-title"
  SCRATCH="$side_b" EVIDENCE="$side_b/evidence" run_jjf update "$id" --title "B-title"
  SCRATCH="$side_a" EVIDENCE="$side_a/evidence" run_jjf push origin >/dev/null || true
  SCRATCH="$side_b" EVIDENCE="$side_b/evidence" run_jjf push origin >/dev/null || true
  SCRATCH="$side_a" EVIDENCE="$side_a/evidence" run_jjf pull origin >/dev/null
  SCRATCH="$side_b" EVIDENCE="$side_b/evidence" run_jjf pull origin >/dev/null

  SCRATCH="$side_a" EVIDENCE="$side_a/evidence" run_jjf show --json "$id"
  local title_a
  title_a="$(jq -r '.title' "$side_a/evidence/last-stdout")"
  SCRATCH="$side_b" EVIDENCE="$side_b/evidence" run_jjf show --json "$id"
  local title_b
  title_b="$(jq -r '.title' "$side_b/evidence/last-stdout")"

  if [[ "$title_a" != "$title_b" ]]; then
    echo "[a2] FINDING: LWW tiebreaker non-deterministic across sides."
    echo "[a2]   side A: $title_a"
    echo "[a2]   side B: $title_b"
  elif [[ "$title_a" != "A-title" && "$title_a" != "B-title" ]]; then
    echo "[a2] FINDING: LWW resolved to neither input value."
    echo "[a2]   merged: $title_a"
  else
    echo "[a2] NEGATIVE: both sides converged to '$title_a'"
  fi
  unset JJF_TEST_CLOCK_SECS
}

# -----------------------------------------------------------------------------
# A3. Trailer parser forward-compat — stray + v1 + unknown-op interleave
# -----------------------------------------------------------------------------
a3() {
  mk_scratch_repo a3 >/dev/null
  run_jjf new -t "a3 trailer target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # Build a commit by hand on refs/jjf/issues/<id> with a hostile trailer block.
  local ref="refs/jjf/issues/$id"
  local parent
  parent="$(cd "$SCRATCH" && git rev-parse "$ref")"
  local tree
  tree="$(cd "$SCRATCH" && git rev-parse "${parent}^{tree}")"

  local msg_file="$EVIDENCE/a3-commit-msg.txt"
  cat > "$msg_file" <<EOF
qa-a3: hostile trailer block

Signed-off-by: Tester <t@t.com>
Co-authored-by: Mallory <m@evil.io>
Jjf-Bug: $id
Jjf-Op: set-title v=hijacked-by-trailer
Jjf-Op: completely-unknown-op-type v=irrelevant
Jjf-Issue: $id
Jjf-At: 1735000000
EOF

  local new_commit
  new_commit="$(cd "$SCRATCH" && git commit-tree "$tree" -p "$parent" -F "$msg_file")"
  (cd "$SCRATCH" && git update-ref "$ref" "$new_commit" "$parent")

  run_jjf show --json "$id"
  record_evidence "post-injection-show"
  local rc
  rc="$(cat "$EVIDENCE/last-exit")"
  if [[ "$rc" != "0" && "$rc" != "1" ]]; then
    echo "[a3] FINDING: unexpected exit $rc (expected 0 or 1, not panic)."
  fi

  local title_after
  title_after="$(jq -r '.title' "$EVIDENCE/post-injection-show-stdout" 2>/dev/null || echo "<no-json>")"
  case "$title_after" in
    "hijacked-by-trailer")
      echo "[a3] FINDING: set-title op in injected trailer applied — title hijacked."
      ;;
    "a3 trailer target"|"<no-json>")
      echo "[a3] NEGATIVE: known set-title op did not hijack (title='$title_after')"
      ;;
    *)
      echo "[a3] FINDING: title corrupted to unexpected value '$title_after'"
      ;;
  esac

  if grep -qiE "unknown.?op|warn|unrecognized" "$EVIDENCE/post-injection-show-stderr"; then
    echo "[a3] OK: unknown-op-type surfaced as warning on stderr"
  else
    echo "[a3] FINDING-CANDIDATE: unknown op silently skipped (no stderr warning)"
  fi
}

# -----------------------------------------------------------------------------
# A4. Corrupt-ref regression check
# -----------------------------------------------------------------------------
a4() {
  mk_scratch_repo a4 >/dev/null
  run_jjf new -t "a4 corruption target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  local ref="refs/jjf/issues/$id"

  # Replace the issue's tree with an empty tree (drops issue.json).
  local empty_tree
  empty_tree="$(cd "$SCRATCH" && git mktree </dev/null)"
  local parent
  parent="$(cd "$SCRATCH" && git rev-parse "$ref")"
  local broken
  broken="$(cd "$SCRATCH" && git commit-tree "$empty_tree" -p "$parent" -m "a4: drop issue.json")"
  (cd "$SCRATCH" && git update-ref "$ref" "$broken" "$parent")

  run_jjf ls
  record_evidence "ls-after-corrupt"
  assert_exit 0 || echo "[a4] FINDING: ls non-zero after corrupt-ref (regression of 90f33c40)"
  if grep -qiE "unreadable|warning|skipped" "$EVIDENCE/ls-after-corrupt-stderr"; then
    echo "[a4] OK: ls surfaced unreadable-ref warning"
  else
    echo "[a4] FINDING: ls silently dropped corrupt ref (no stderr warning)"
  fi

  run_jjf ready
  record_evidence "ready-after-corrupt"
  assert_exit 0 || echo "[a4] FINDING: ready non-zero after corrupt-ref"

  if grep -qF "$id" "$EVIDENCE/ls-after-corrupt-stdout"; then
    echo "[a4] FINDING: corrupt ref still appeared in ls output"
  fi
}

# When invoked as a script, run all four serially.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  a1; a2; a3; a4
fi
```

- [ ] **Step 2: Run sub-pass 1**

```bash
bash experiments/qa-redteam-2026-06-25/sub1-dataloss.sh 2>&1 | tee experiments/qa-redteam-2026-06-25/.scratch/sub1.log
```

Watch for lines starting with `[aN] FINDING:` — those are real findings. Lines starting with `[aN] NEGATIVE:` or `[aN] OK:` are clean. Lines starting with `[aN] FINDING-CANDIDATE:` need a 5-minute judgment call before promoting to a ticket.

- [ ] **Step 3: For each real finding, file a ticket**

For each `FINDING` line, capture the finding and file it. Substitute the right values per finding:

```bash
cat <<'BODY' | ./bin/jjf new \
  --type bug \
  --slug qa-2026-06-25-<short-kebab> \
  -l qa-redteam-2026-06-25 \
  -l sev:data-loss \
  -t "<short title naming the defect>" \
  -F - --json
# Repro

Path: `experiments/qa-redteam-2026-06-25/sub1-dataloss.sh`
Function: `aN`
Minimal recipe:

```bash
# 5-10 line minimal command sequence pulled from the attack function
```

# Observed

Exit code: <code>
Stderr excerpt:
```
<paste from .scratch/aN/evidence/>
```
Post-mutation state (from `jjf show --json <id>`):
```json
<paste>
```

# Expected

<what the spec / cli-json.md / source file:line says should happen>

# Severity rationale

<one sentence: why sev:data-loss applies>
BODY
```

Capture the ticket id from the JSON envelope. For each filed ticket, **also add the migration-blocking edge**:

```bash
./bin/jjf dep add <new-ticket-id> -d blocks:cc2fa96
```

- [ ] **Step 4: Update README.md with the verdict rows**

Edit each of A1–A4's row in the table to one of:
- `finding` + the ticket id (e.g., `a3f9c01`)
- `negative` (recipe ran, no defect — keep as regression check)
- `deferred` + a one-line reason (over the 30-min budget)

- [ ] **Step 5: Run workspace tests to confirm no regressions from the scratch work**

```bash
cargo nextest run --workspace 2>&1 | tail -5
```
Expected: all green. If anything is red, **stop and surface to user** before continuing to sub-pass 2 — the harness shouldn't have touched the source tree.

- [ ] **Step 6: Commit the sub-pass artifacts**

```bash
git add experiments/qa-redteam-2026-06-25/sub1-dataloss.sh experiments/qa-redteam-2026-06-25/README.md
git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: sub-pass 1 (data loss)

A1-A4 attacks landed; README finding index updated with verdicts.
Tickets filed for each reproducible finding; negative results
retained as future regression checks.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

- [ ] **Step 7: Check for the stop-early condition**

If any A1–A4 finding hits the stop-early trigger (data-loss in merge driver / v3 read path / format-version sentinel, or panic from a normal verb), **stop here**, surface to user with the finding ticket id and the trigger reason. Otherwise continue to Task 3.

---

## Task 3: Sub-pass 2 — Wrong answer

**Files:**
- Create: `experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh`
- Modify: `experiments/qa-redteam-2026-06-25/README.md`
- File on `issues` bookmark: 0–N tickets with `sev:wrong-answer`

**Interfaces:**
- Consumes: `lib.sh` helpers (same set as Task 2).
- Produces: per-attack verdicts in `README.md`; 0+ planner tickets.

### Attack recipes (function bodies)

- **B1. Sort stability on `created_at` ties.** Pin clock, create 5 issues at same second with mixed types. Run `jjf ready --json` twice; `cmp` outputs byte-for-byte.
- **B2. Status-machine reachability.** For each `(status, verb)` pair in the matrix: create an issue, drive it to the source status, run the verb, capture exit + stderr. Verify each combination either succeeds cleanly or rejects with a typed error (`invalid_status_transition` / `invalid_input` / similar — no generic exit 1 swallows).
- **B3. Dep-graph corner cases.** (i) `A blocks B` + `B parent-of A` mixed-kind cycle (should reject at preflight per `d32ec955`). (ii) `jjf dep add <slug-of-issue> -d blocks:<id-of-same-issue>` — self-dep through slug resolution. (iii) `A blocks B`; abandon A; does `jjf ready` consider B unblocked? (the spec is silent; the answer informs whether this is a real defect or a clarification ticket).
- **B4. `memories <search>` with regex metacharacters.** Insert one memory with key `regex-test`, value `the quick brown fox`. Search for `.`, `q.ick`, `\bfox\b`, `(fox)`, `f\*x`. Confirm: only literal-substring matches, no regex interpretation, no panic, no extra matches.

- [ ] **Step 1: Write `sub2-wronganswer.sh`**

Create `experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh`:

```bash
#!/usr/bin/env bash
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
build_jjf_release || exit 1

# -----------------------------------------------------------------------------
# B1. Sort stability on created_at ties
# -----------------------------------------------------------------------------
b1() {
  pin_clock 1735000000
  mk_scratch_repo b1 >/dev/null
  run_jjf new -t "b1 issue 1" --type bug --slug b1-bug-one
  run_jjf new -t "b1 issue 2" --type feature --slug b1-feat-two
  run_jjf new -t "b1 issue 3" --type bug --slug b1-bug-three
  run_jjf new -t "b1 issue 4" --type research --slug b1-res-four
  run_jjf new -t "b1 issue 5" --type epic --slug b1-epic-five

  run_jjf ready --json
  cp "$EVIDENCE/last-stdout" "$EVIDENCE/ready-run-1.json"
  run_jjf ready --json
  cp "$EVIDENCE/last-stdout" "$EVIDENCE/ready-run-2.json"

  if assert_byte_equal "$EVIDENCE/ready-run-1.json" "$EVIDENCE/ready-run-2.json" 2>/dev/null; then
    echo "[b1] NEGATIVE: ready --json output is stable across runs"
  else
    echo "[b1] FINDING: ready --json output not stable across runs (sort non-deterministic on created_at ties)"
  fi
  unset JJF_TEST_CLOCK_SECS
}

# -----------------------------------------------------------------------------
# B2. Status-machine reachability matrix
# -----------------------------------------------------------------------------
b2() {
  # Source statuses we'll drive an issue to.
  local statuses=("open" "closed" "abandoned" "blocked")
  # Verbs we'll exercise.
  local verbs=(
    "comment_one_line"
    "update_title"
    "update_status_open"
    "block_issue"
    "unblock_issue"
    "close_issue"
    "open_issue"
    "abandon_issue"
    "label_add_test"
    "dep_add_self"   # we'll create a sibling for this
  )

  # Helper: drive a fresh issue to the requested source status.
  drive_to_status() {
    local target_status="$1"
    local id_var="$2"
    local id
    run_jjf new -t "b2 source=$target_status target" --slug "b2-$target_status-$RANDOM"
    id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
    case "$target_status" in
      open) ;;
      closed)    run_jjf close "$id" ;;
      abandoned) run_jjf abandon "$id" ;;
      blocked)   run_jjf block "$id" --reason "b2 driver" ;;
    esac
    printf -v "$id_var" '%s' "$id"
  }

  # Helper: exercise one verb and record outcome.
  exercise_verb() {
    local verb="$1"
    local id="$2"
    local sib="$3"
    case "$verb" in
      comment_one_line) echo "x" | run_jjf comment "$id" -F - ;;
      update_title)     run_jjf update "$id" --title "b2-renamed" ;;
      update_status_open) run_jjf update "$id" --status open ;;
      block_issue)      run_jjf block "$id" --reason "b2 exercise" ;;
      unblock_issue)    run_jjf unblock "$id" ;;
      close_issue)      run_jjf close "$id" ;;
      open_issue)       run_jjf open "$id" ;;
      abandon_issue)    run_jjf abandon "$id" ;;
      label_add_test)   run_jjf label add "$id" b2-touched ;;
      dep_add_self)     run_jjf dep add "$id" -d "blocks:$sib" ;;
    esac
  }

  mk_scratch_repo b2 >/dev/null
  # Create a sibling for dep_add_self exercises.
  run_jjf new -t "b2 sibling for dep" --slug b2-sibling
  local sib
  sib="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  for s in "${statuses[@]}"; do
    for v in "${verbs[@]}"; do
      local id
      drive_to_status "$s" id
      exercise_verb "$v" "$id" "$sib"
      local rc; rc="$(cat "$EVIDENCE/last-exit")"
      record_evidence "matrix-$s-$v"
      # Generic exit 1 with no typed JSON envelope is a candidate finding.
      if [[ "$rc" == "1" ]]; then
        # Probe stderr for a typed error indicator (kind=... or invalid_input).
        if ! grep -qiE "invalid_status_transition|invalid_input|already_|not_" "$EVIDENCE/last-stderr"; then
          echo "[b2] FINDING-CANDIDATE: ($s, $v) exit 1 without typed error message"
        fi
      elif [[ "$rc" != "0" && "$rc" != "2" ]]; then
        echo "[b2] FINDING: ($s, $v) unexpected exit $rc"
      fi
    done
  done
  echo "[b2] done; matrix evidence in $EVIDENCE/matrix-*"
}

# -----------------------------------------------------------------------------
# B3. Dep-graph corner cases
# -----------------------------------------------------------------------------
b3() {
  mk_scratch_repo b3 >/dev/null
  run_jjf new -t "b3 issue A" --slug b3-issue-a
  local A
  A="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  run_jjf new -t "b3 issue B" --slug b3-issue-b
  local B
  B="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # (i) Mixed-kind cycle.
  run_jjf dep add "$A" -d "blocks:$B"
  assert_exit 0 || echo "[b3] FINDING: dep add blocks:B on A failed unexpectedly"
  run_jjf dep add "$B" -d "parent-of:$A"
  record_evidence "mixed-cycle-attempt"
  if [[ "$(cat "$EVIDENCE/mixed-cycle-attempt-exit")" == "0" ]]; then
    echo "[b3] FINDING: mixed-kind cycle (A blocks B + B parent-of A) accepted"
  else
    echo "[b3] NEGATIVE: mixed-kind cycle rejected at preflight"
  fi

  # (ii) Self-dep through slug resolution.
  run_jjf dep add b3-issue-a -d "blocks:$A"
  record_evidence "self-dep-slug"
  if [[ "$(cat "$EVIDENCE/self-dep-slug-exit")" == "0" ]]; then
    echo "[b3] FINDING: self-dep via slug resolution accepted"
  else
    echo "[b3] NEGATIVE: self-dep via slug rejected"
  fi

  # (iii) Abandoned blocker — does `ready` consider B unblocked?
  mk_scratch_repo b3-abandoned >/dev/null
  run_jjf new -t "b3-abandoned A" --slug b3-abandoned-a
  local AA
  AA="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  run_jjf new -t "b3-abandoned B" --slug b3-abandoned-b -d "blocks:$AA"
  local BB
  BB="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  run_jjf abandon "$AA"
  run_jjf ready --json
  record_evidence "ready-after-abandon"
  if jq -e ".[] | select(.id==\"$BB\")" "$EVIDENCE/ready-after-abandon-stdout" >/dev/null 2>&1; then
    echo "[b3] NEGATIVE: ready returns B after blocker A abandoned (treats abandoned as terminal)"
  else
    echo "[b3] FINDING-CANDIDATE: ready does NOT return B after blocker abandoned — spec ambiguous, decide & file"
  fi
}

# -----------------------------------------------------------------------------
# B4. memories <search> with regex metacharacters
# -----------------------------------------------------------------------------
b4() {
  mk_scratch_repo b4 >/dev/null
  run_jjf remember "the quick brown fox" --key regex-test
  for needle in "." "q.ick" "\\bfox\\b" "(fox)" "f*x"; do
    run_jjf memories "$needle"
    record_evidence "search-$(echo "$needle" | tr -c 'a-z' '_')"
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    if [[ "$rc" != "0" && "$rc" != "1" ]]; then
      echo "[b4] FINDING: search '$needle' exit $rc (panic or unexpected)"
    fi
    # Spurious-match check: literal "." should match (because "." is a substring
    # of nothing in our test data) — actually let's check carefully.
  done
  # Specific assertion: "q.ick" should NOT match "quick" under literal semantics.
  run_jjf memories "q.ick"
  if grep -q "regex-test" "$EVIDENCE/last-stdout"; then
    echo "[b4] FINDING: 'q.ick' matched 'quick' — regex semantics leaking into search"
  else
    echo "[b4] NEGATIVE: 'q.ick' did not match 'quick' — literal semantics confirmed"
  fi
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  b1; b2; b3; b4
fi
```

- [ ] **Step 2: Run sub-pass 2**

```bash
bash experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh 2>&1 | tee experiments/qa-redteam-2026-06-25/.scratch/sub2.log
```

- [ ] **Step 3: For each real finding, file a `sev:wrong-answer` ticket**

Use the same template as Task 2 step 3, but with `-l sev:wrong-answer` and **no `blocks:cc2fa96` edge** (wrong-answer findings don't gate migration).

- [ ] **Step 4: Update README.md verdict rows for B1–B4**

- [ ] **Step 5: Run workspace tests**

```bash
cargo nextest run --workspace 2>&1 | tail -5
```
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add experiments/qa-redteam-2026-06-25/sub2-wronganswer.sh experiments/qa-redteam-2026-06-25/README.md
git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: sub-pass 2 (wrong answer)

B1-B4 attacks landed; README finding index updated.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

---

## Task 4: Sub-pass 3 — Crash / panic

**Files:**
- Create: `experiments/qa-redteam-2026-06-25/sub3-panic.sh`
- Modify: `experiments/qa-redteam-2026-06-25/README.md`
- File on `issues` bookmark: 0–N tickets with `sev:panic`

**Interfaces:**
- Consumes: `lib.sh`.
- Produces: per-attack verdicts; 0+ planner tickets.

### Attack recipes (function bodies)

- **C1. Format-version sentinel → blob.** Replace `refs/jjf/meta/format-version` with an OID pointing at a blob (created via `git hash-object -w`). Run `jjf show`, `ls`, `ready`. Expected: typed error, exit 1, no Rust panic in stderr (no `thread 'main' panicked`), no raw `git2`/`gix` errors leaking.
- **C2. Adversarial Unicode through title.** Post-validation: create issues with titles containing U+200B (ZWSP), U+200D (ZWJ), U+202E (RTL override), combining diacritics, Cyrillic-а. Verify `show --json` round-trips byte-faithfully and `jjf ls` exits 0.
- **C3. Oversized inputs.** 10MB title (expect reject at validation, no allocation explosion); 10MB body (no documented cap — capture behavior); 1k labels via repeated `label add`; 1k comments via loop. Look for OOM, panics, latency cliffs >30s on `jjf ls`.
- **C4. Malformed `issue.json`.** Hand-write trees with: extra unknown field; missing `status`; `"status": 42`. Run `jjf show --json`. Expect typed error envelope, not `serde_json::Error` leaking.

- [ ] **Step 1: Write `sub3-panic.sh`**

```bash
#!/usr/bin/env bash
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
build_jjf_release || exit 1

# -----------------------------------------------------------------------------
# C1. format-version sentinel → blob
# -----------------------------------------------------------------------------
c1() {
  mk_scratch_repo c1 >/dev/null
  run_jjf new -t "c1 sentinel target"  # ensure repo is initialized

  local blob
  blob="$(cd "$SCRATCH" && echo "not a commit" | git hash-object -w --stdin)"
  (cd "$SCRATCH" && git update-ref refs/jjf/meta/format-version "$blob")

  for verb in "ls" "ready" "show roadmap"; do
    run_jjf $verb
    record_evidence "c1-$(echo $verb | tr ' ' '-')"
    if grep -qi "panicked\|thread 'main'" "$EVIDENCE/last-stderr"; then
      echo "[c1] FINDING: panic on '$verb' with blob sentinel"
    fi
    if grep -qiE "git2::|gix::" "$EVIDENCE/last-stderr"; then
      echo "[c1] FINDING-CANDIDATE: raw git library error leaking on '$verb'"
    fi
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    if [[ "$rc" == "0" ]]; then
      echo "[c1] FINDING: '$verb' exit 0 despite corrupt sentinel"
    fi
  done
}

# -----------------------------------------------------------------------------
# C2. Adversarial Unicode through title
# -----------------------------------------------------------------------------
c2() {
  mk_scratch_repo c2 >/dev/null
  local titles=(
    $'zwsp​in-middle'
    $'zwj‍joiner'
    $'rtl‮override'
    $'á-combined'         # a with combining acute
    $'аdmin'               # cyrillic a then 'dmin'
  )
  for t in "${titles[@]}"; do
    run_jjf new -t "$t"
    record_evidence "c2-create"
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    if [[ "$rc" != "0" && "$rc" != "2" ]]; then
      echo "[c2] FINDING: title $(printf '%q' "$t") exit $rc"
      continue
    fi
    if [[ "$rc" == "0" ]]; then
      local id; id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
      run_jjf show --json "$id"
      record_evidence "c2-show-$id"
      local got
      got="$(jq -r '.title' "$EVIDENCE/c2-show-$id-stdout")"
      if [[ "$got" != "$t" ]]; then
        echo "[c2] FINDING: title round-trip lossy for $(printf '%q' "$t") → $(printf '%q' "$got")"
      fi
    fi
  done
  run_jjf ls
  record_evidence "c2-ls"
  assert_exit 0 || echo "[c2] FINDING: ls non-zero after unicode titles"
}

# -----------------------------------------------------------------------------
# C3. Oversized inputs — title, body, labels, comments
# -----------------------------------------------------------------------------
c3() {
  mk_scratch_repo c3 >/dev/null

  # 10MB title — expect rejection at validation. The harness should not hang.
  local big_title; big_title="$(head -c 10000000 /dev/urandom | base64 | tr -d '\n' | head -c 10000000)"
  timeout 30 bash -c "
    SCRATCH='$SCRATCH' EVIDENCE='$EVIDENCE'
    cd '$SCRATCH' && '$JJF_BIN' new -t '$big_title' \
      >'$EVIDENCE/c3-bigtitle-stdout' 2>'$EVIDENCE/c3-bigtitle-stderr'
    echo \$? > '$EVIDENCE/c3-bigtitle-exit'
  " || echo "[c3] FINDING: 10MB title hung jjf for >30s"

  local rc; rc="$(cat "$EVIDENCE/c3-bigtitle-exit" 2>/dev/null || echo timeout)"
  case "$rc" in
    2) echo "[c3] NEGATIVE: 10MB title rejected at preflight (exit 2)" ;;
    0) echo "[c3] FINDING: 10MB title accepted (no length cap)" ;;
    *) echo "[c3] FINDING-CANDIDATE: 10MB title exit $rc — check stderr" ;;
  esac

  # 10MB body — observe behavior. No documented cap; just look for panic.
  local body_file="$EVIDENCE/c3-bigbody.txt"
  head -c 10000000 /dev/urandom | base64 > "$body_file"
  timeout 30 bash -c "
    SCRATCH='$SCRATCH' EVIDENCE='$EVIDENCE'
    cd '$SCRATCH' && '$JJF_BIN' new -t 'c3 bigbody' -F '$body_file' \
      >'$EVIDENCE/c3-bigbody-stdout' 2>'$EVIDENCE/c3-bigbody-stderr'
    echo \$? > '$EVIDENCE/c3-bigbody-exit'
  " || echo "[c3] FINDING: 10MB body hung jjf for >30s"

  # 1k labels on one issue.
  run_jjf new -t "c3 label-explosion"
  local lid; lid="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  for i in $(seq 1 1000); do
    run_jjf label add "$lid" "label-$i" >/dev/null 2>&1
    if [[ "$(cat "$EVIDENCE/last-exit")" != "0" ]]; then
      echo "[c3] info: label add failed at i=$i (exit $(cat "$EVIDENCE/last-exit"))"
      break
    fi
  done
  timeout 30 bash -c "
    SCRATCH='$SCRATCH' EVIDENCE='$EVIDENCE'
    cd '$SCRATCH' && '$JJF_BIN' ls \
      >'$EVIDENCE/c3-ls-bigchain-stdout' 2>'$EVIDENCE/c3-ls-bigchain-stderr'
    echo \$? > '$EVIDENCE/c3-ls-bigchain-exit'
  " || echo "[c3] FINDING: ls hung >30s with 1k-label issue"

  # 1k comments on one issue.
  run_jjf new -t "c3 comment-explosion"
  local cid; cid="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  for i in $(seq 1 1000); do
    echo "comment $i" | run_jjf comment "$cid" -F - >/dev/null 2>&1
    if [[ "$(cat "$EVIDENCE/last-exit")" != "0" ]]; then
      echo "[c3] info: comment add failed at i=$i"
      break
    fi
  done
  timeout 30 bash -c "
    SCRATCH='$SCRATCH' EVIDENCE='$EVIDENCE'
    cd '$SCRATCH' && '$JJF_BIN' show '$cid' --json \
      >'$EVIDENCE/c3-show-1k-comments-stdout' 2>'$EVIDENCE/c3-show-1k-comments-stderr'
    echo \$? > '$EVIDENCE/c3-show-1k-comments-exit'
  " || echo "[c3] FINDING: show hung >30s on 1k-comment issue"
}

# -----------------------------------------------------------------------------
# C4. Malformed issue.json — extra field, missing required, wrong type
# -----------------------------------------------------------------------------
c4() {
  mk_scratch_repo c4 >/dev/null
  run_jjf new -t "c4 malformed target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  local ref="refs/jjf/issues/$id"

  # Variant 1: extra unknown field (forward-compat probe).
  malform() {
    local payload="$1"
    local label="$2"
    local blob
    blob="$(cd "$SCRATCH" && echo "$payload" | git hash-object -w --stdin)"
    local tree
    tree="$(cd "$SCRATCH" && printf '100644 blob %s\tissue.json\n' "$blob" | git mktree)"
    local parent
    parent="$(cd "$SCRATCH" && git rev-parse "$ref")"
    local bad
    bad="$(cd "$SCRATCH" && git commit-tree "$tree" -p "$parent" -m "c4 $label")"
    (cd "$SCRATCH" && git update-ref "$ref" "$bad" "$parent")
    run_jjf show --json "$id"
    record_evidence "c4-$label"
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    if grep -qi "panicked" "$EVIDENCE/last-stderr"; then
      echo "[c4] FINDING: panic on $label malformed issue.json"
    fi
    if grep -qE "serde_json::|Error\(\"" "$EVIDENCE/last-stderr"; then
      echo "[c4] FINDING-CANDIDATE: raw serde_json error leaking on $label"
    fi
    if [[ "$rc" == "0" ]]; then
      echo "[c4] FINDING: $label malformed issue.json accepted (rc=0)"
    fi
  }

  # We need a baseline good payload to mutate.
  run_jjf show --json "$id" >/dev/null
  cp "$EVIDENCE/last-stdout" "$EVIDENCE/c4-baseline.json"

  # extra-field
  jq '. + {"unknown_field_xyz": true}' "$EVIDENCE/c4-baseline.json" > "$EVIDENCE/c4-extra.json"
  malform "$(cat "$EVIDENCE/c4-extra.json")" "extra-field"

  # missing-status
  jq 'del(.status)' "$EVIDENCE/c4-baseline.json" > "$EVIDENCE/c4-missing-status.json"
  malform "$(cat "$EVIDENCE/c4-missing-status.json")" "missing-status"

  # status-wrong-type
  jq '.status = 42' "$EVIDENCE/c4-baseline.json" > "$EVIDENCE/c4-status-int.json"
  malform "$(cat "$EVIDENCE/c4-status-int.json")" "status-int"
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  c1; c2; c3; c4
fi
```

- [ ] **Step 2: Run sub-pass 3**

```bash
bash experiments/qa-redteam-2026-06-25/sub3-panic.sh 2>&1 | tee experiments/qa-redteam-2026-06-25/.scratch/sub3.log
```

- [ ] **Step 3: For each `sev:panic` finding, file a ticket**

Use the Task 2 template with `-l sev:panic`. **Add the `blocks:cc2fa96` edge** — panic findings are migration-killers.

- [ ] **Step 4: Update README.md for C1–C4**

- [ ] **Step 5: Run workspace tests**

```bash
cargo nextest run --workspace 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add experiments/qa-redteam-2026-06-25/sub3-panic.sh experiments/qa-redteam-2026-06-25/README.md
git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: sub-pass 3 (crash/panic)

C1-C4 attacks landed; README finding index updated.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

- [ ] **Step 7: Re-check stop-early trigger** (same conditions as Task 2 step 7).

---

## Task 5: Sub-pass 4 — Contract drift

**Files:**
- Create: `experiments/qa-redteam-2026-06-25/sub4-contractdrift.sh`
- Modify: `experiments/qa-redteam-2026-06-25/README.md`
- File on `issues` bookmark: 0–N tickets with `sev:contract-drift`

**Interfaces:**
- Consumes: `lib.sh`.
- Produces: per-attack verdicts; 0+ planner tickets.

### Attack recipes

- **D1. Envelope/exit-code sweep.** For each combination of verb × error class, run with `--json`, capture exit + stdout + stderr, parse the JSON envelope (when present), and check: (a) every error has `ok: false`, (b) every error has a `kind` field, (c) the `kind` is from the documented vocabulary in `docs/cli-json.md`, (d) exit code matches the doc. Flag any mismatch.
- **D2. `abandon --json` envelope.** Sub-case of D1, called out because the spec mentioned doc-text-only treatment. Confirm the envelope exists and matches the mutating-verb family `{"ok": true, "id": "..."}`.
- **D3. `ConcurrentWrite.hint` text snapshot.** Trigger a concurrent-write scenario (start a mutate, race a second one in a sibling clone, push both). Capture the hint text. If it looks substring-matchable (e.g., contains a phrase a script might key on like "another writer landed first"), file a finding so a future round can either stabilize the message format or split it into structured fields.

- [ ] **Step 1: Write `sub4-contractdrift.sh`**

```bash
#!/usr/bin/env bash
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
build_jjf_release || exit 1

# -----------------------------------------------------------------------------
# D1. Envelope/exit-code sweep
# -----------------------------------------------------------------------------
d1() {
  mk_scratch_repo d1 >/dev/null

  # Each row: <description> <verb-args>
  # We exercise specific error classes by setting up failing preconditions.
  local -a cases=(
    "issue_not_found:show:fakefake"
    "slug_not_found:show:no-such-slug"
    "issue_not_found:close:fakefake"
    "issue_not_found:abandon:fakefake"
    "invalid_input:dep:add:fakefake:-d:blocks:alsofake"
    "invalid_input:label:add:fakefake:legit-label"
  )

  for c in "${cases[@]}"; do
    IFS=':' read -ra parts <<< "$c"
    local expected_kind="${parts[0]}"
    local verb="${parts[@]:1}"
    run_jjf --json $verb
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    record_evidence "d1-$expected_kind"

    # Envelope must exist and have ok:false.
    if ! jq -e '.ok == false' "$EVIDENCE/last-stdout" >/dev/null 2>&1; then
      echo "[d1] FINDING: '$verb' (--json) missing ok:false envelope (rc=$rc)"
      continue
    fi
    # kind field must match.
    local got_kind
    got_kind="$(jq -r '.kind' "$EVIDENCE/last-stdout")"
    if [[ "$got_kind" != "$expected_kind" ]]; then
      echo "[d1] FINDING: '$verb' (--json) kind=$got_kind, expected $expected_kind"
    fi
  done
}

# -----------------------------------------------------------------------------
# D2. abandon --json envelope on success
# -----------------------------------------------------------------------------
d2() {
  mk_scratch_repo d2 >/dev/null
  run_jjf new -t "d2 abandon target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  run_jjf --json abandon "$id"
  record_evidence "d2-abandon"
  assert_exit 0 || echo "[d2] FINDING: abandon --json exit non-zero on happy path"
  if ! jq -e '.ok == true' "$EVIDENCE/d2-abandon-stdout" >/dev/null 2>&1; then
    echo "[d2] FINDING: abandon --json missing {ok:true} envelope"
  fi
  local got_id
  got_id="$(jq -r '.id' "$EVIDENCE/d2-abandon-stdout" 2>/dev/null || echo NONE)"
  if [[ "$got_id" != "$id" ]]; then
    echo "[d2] FINDING: abandon --json id=$got_id (expected $id)"
  fi
}

# -----------------------------------------------------------------------------
# D3. ConcurrentWrite.hint stability snapshot
# -----------------------------------------------------------------------------
d3() {
  mk_scratch_repo d3 >/dev/null
  run_jjf new -t "d3 concurrent target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # Set up a sibling and race two updates with a shared bare remote.
  local bare="$QA_ROOT/.scratch/d3-bare.git"
  rm -rf "$bare"; git init --bare "$bare" >/dev/null
  (cd "$SCRATCH" && "$JJF_BIN" remote add origin "$bare" >/dev/null)
  (cd "$SCRATCH" && "$JJF_BIN" push origin >/dev/null)

  local sib="$QA_ROOT/.scratch/d3-sib"
  rm -rf "$sib"; cp -R "$SCRATCH" "$sib"
  # cp -R of a colocated jj+git repo can leave jj op-log in a weird
  # state; if d3 misbehaves, swap the cp -R for `git clone $bare $sib`
  # followed by `jj git init --git-repo=. --colocate` inside $sib.

  SCRATCH="$SCRATCH" EVIDENCE="$EVIDENCE" run_jjf update "$id" --title "first"
  SCRATCH="$SCRATCH" EVIDENCE="$EVIDENCE" run_jjf push origin >/dev/null

  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json update "$id" --title "second"
  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json push origin
  record_evidence "d3-push-conflict"
  # If push surfaces concurrent_write, snapshot the hint.
  local kind
  kind="$(jq -r '.kind' "$sib/evidence/last-stdout" 2>/dev/null || echo unknown)"
  local hint
  hint="$(jq -r '.hint // .message' "$sib/evidence/last-stdout" 2>/dev/null || cat "$sib/evidence/last-stderr")"
  echo "[d3] kind=$kind hint=$hint"
  # Substring-match-prone phrases — flag if any of these appear verbatim.
  for phrase in "another writer landed first" "concurrent write" "conflict" "stale"; do
    if [[ "$hint" == *"$phrase"* ]]; then
      echo "[d3] FINDING-CANDIDATE: hint contains substring '$phrase' (scripts may key on it)"
    fi
  done
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  d1; d2; d3
fi
```

- [ ] **Step 2: Run sub-pass 4**

```bash
bash experiments/qa-redteam-2026-06-25/sub4-contractdrift.sh 2>&1 | tee experiments/qa-redteam-2026-06-25/.scratch/sub4.log
```

- [ ] **Step 3: For each real finding, file a `sev:contract-drift` ticket**

Use the Task 2 template with `-l sev:contract-drift`. No `blocks:cc2fa96` edge.

- [ ] **Step 4: Update README.md for D1–D3**

- [ ] **Step 5: Run workspace tests**

```bash
cargo nextest run --workspace 2>&1 | tail -5
```

- [ ] **Step 6: Commit**

```bash
git add experiments/qa-redteam-2026-06-25/sub4-contractdrift.sh experiments/qa-redteam-2026-06-25/README.md
git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: sub-pass 4 (contract drift)

D1-D3 attacks landed; README finding index updated.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

---

## Task 6: File the proptest follow-up + round-closing comment + push

**Files:**
- Modify: `experiments/qa-redteam-2026-06-25/README.md` (final index pass)
- File on `issues` bookmark: 1 feature ticket (proptest harness) + 1 closing summary comment on `epic:agent-ergonomics` (`5a755ec`)

**Interfaces:**
- Consumes: the finding list accumulated across tasks 2–5.
- Produces: end-of-round state.

- [ ] **Step 1: File the proptest follow-up ticket**

```bash
cat <<'BODY' | ./bin/jjf new \
  --type feature \
  --slug qa-proptest-harness \
  -l qa-redteam-2026-06-25 \
  -l epic:agent-ergonomics \
  -t "qa-proptest-harness: stand up proptest against storage mutate/read surface" \
  -F - --json
# Goal

Stand up a `proptest` harness against `jjf-storage`'s public mutate/read
surface so future QA rounds can find counter-examples the harm-class
attack tree wouldn't.

# Approach (sketch)

- Generate `(Issue, [Op])` tuples — Issue varying status / labels /
  deps; Op varying verbs and arguments.
- Assert invariants:
  - **Round-trip**: write → read → write → read produces equal records.
  - **Idempotence** of read-only verbs.
  - **Status machine**: every transition either succeeds and lands a
    valid status or rejects with a typed error; no panics.
- Estimated setup cost: 4–6 hours. Why this round skipped it: the
  harm-class attack tree caught the highest-leverage interaction
  bugs first.

# References

- Round spec: `docs/superpowers/specs/2026-06-25-qa-redteam-sweep-design.md`
- Round plan: `docs/superpowers/plans/2026-06-25-qa-redteam-sweep.md`
- Surface map (in the spec) names the touchpoints worth covering first.
BODY
```

Capture the new ticket id from the JSON envelope.

- [ ] **Step 2: Tally findings and post the closing summary comment**

Build the summary. Replace `<N_*>` with the actual counts from your finding-index walk, and `<proptest-id>` with the id from step 1.

```bash
cat <<BODY | ./bin/jjf comment 5a755ec -F -
QA red-team round 2026-06-25 closed.

Summary:
- sev:data-loss: <N_dataloss> findings
- sev:wrong-answer: <N_wronganswer> findings
- sev:panic: <N_panic> findings
- sev:contract-drift: <N_contractdrift> findings
- total: <N_total> tickets filed under label \`qa-redteam-2026-06-25\`

Follow-up:
- \`<proptest-id> qa-proptest-harness\` — proptest harness against
  jjf-storage mutate/read surface. Filed as a feature, not blocking.

Repro harness: \`experiments/qa-redteam-2026-06-25/\` (scripts +
README finding index; \`.scratch/\` is gitignored).
BODY
```

- [ ] **Step 3: Final README pass — confirm every row has a verdict**

Open `experiments/qa-redteam-2026-06-25/README.md`. Every row in A1–A4, B1–B4, C1–C4, D1–D3 must have either `finding <id>`, `negative`, or `deferred + reason` in the verdict column. No `_pending_` left.

- [ ] **Step 4: Commit any final README edits**

```bash
git add experiments/qa-redteam-2026-06-25/README.md
git diff --cached --quiet || git commit -m "$(cat <<'EOF'
qa-redteam-2026-06-25: round-closing README pass

Final verdict column populated for all 15 attacks; round wraps.

Claude-Session: https://claude.ai/code/session_01XRGyWcRjfztdpwprrC7A6W
EOF
)"
```

- [ ] **Step 5: Push code + planner**

```bash
git push origin main
./bin/jjf push origin
```

- [ ] **Step 6: Final workspace test pass**

```bash
cargo nextest run --workspace 2>&1 | tail -5
```
Expected: green. If red, **surface to user** — the harness was supposed to be inert against the source tree.

- [ ] **Step 7: End-of-round report to orchestrator**

Print to stdout (or hand to the orchestrating session) a one-paragraph wrap:
- Finding count by severity.
- Proptest follow-up ticket id.
- Pointer to `experiments/qa-redteam-2026-06-25/`.
- Any deferred attacks worth re-running in the next round.

That's the deliverable.

---

## Self-review notes

(For the plan author, not the executor — these are the checks I ran after writing.)

- **Spec coverage**: every harm class A/B/C/D from the spec → its own task. Per-finding ticket shape → restated in Global Constraints and Task 2 Step 3. Proptest C-ticket → Task 6 Step 1. Stop-early triggers → Task 2 Step 7 and Task 4 Step 7. Negative-result discipline → README verdict column accepts `negative`.
- **Placeholders**: every recipe carries the actual Bash, not a description. The four-section ticket body template in Task 2 Step 3 has literal placeholders (`<short title>`, `<paste from evidence>`) — those are intentional substitution points, not plan-failure placeholders, and the surrounding prose tells the executor exactly what to paste.
- **Type consistency**: helper names (`build_jjf_release`, `mk_scratch_repo`, `run_jjf`, `assert_exit`, `assert_stderr_contains`, `assert_json_field`, `assert_byte_equal`, `record_evidence`, `pin_clock`) are consistent between the Interfaces block in Task 1 and every call site in Tasks 2–5.
