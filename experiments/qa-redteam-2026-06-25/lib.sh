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
  (cd "$dir" && jj git init >/dev/null) || {
    echo "[lib] FATAL: jj git init failed in $dir" >&2
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
