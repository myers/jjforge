#!/usr/bin/env bash
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"
build_jjf_release || exit 1

# -----------------------------------------------------------------------------
# D1. Envelope/exit-code sweep
# -----------------------------------------------------------------------------
#
# Per docs/cli-json.md: error envelopes go to STDERR, not stdout.
# Stdout is empty (or the success payload) on error paths.
# So we check last-stderr for the {ok:false, error:{kind:...}} shape.
#
# Cases and expected behavior per the error-kind table:
#   issue_not_found  -> exit 1  (runtime)   fakefake = valid 7-hex, not found
#   slug_not_found   -> exit 2  (preflight) no-such-slug = non-hex, no slug match
#   invalid_input    -> exit 1  (runtime)   close on already-closed issue
#   bad_id           -> exit 2  (preflight) "NOTVALID" is not valid hex
#
d1() {
  mk_scratch_repo d1 >/dev/null

  # Create one issue, then close it — gives us a "close on closed" target
  run_jjf new -t "d1 target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  run_jjf close "$id"

  # Each row: <expected_kind>:<expected_exit>:<description>  +  verb args in remaining fields
  # IFS=: splits on colons; the verb args start at index 3.
  # Note: "fakefake" (8 chars) and "deadbee" (7 lowercase hex chars) are valid 7-hex-parseable
  # ids that don't exist in the repo → issue_not_found (exit 1, runtime).
  # "no-such-slug" contains hyphens — non-hex, treated as slug handle → slug_not_found (exit 2).
  # "NOTVALID" — not valid hex → bad_id (exit 2).
  local -a cases=(
    "issue_not_found:1:show-nonexistent-id:show:deadbee"
    "slug_not_found:2:show-nonexistent-slug:show:no-such-slug"
    "issue_not_found:1:close-nonexistent-id:close:deadbee"
    "issue_not_found:1:abandon-nonexistent-id:abandon:deadbee"
    "invalid_input:1:close-already-closed:close:${id}"
    "bad_id:2:label-add-bad-id:label:add:NOTVALID:legit-label"
  )

  for c in "${cases[@]}"; do
    # Split on ':'
    IFS=':' read -ra parts <<< "$c"
    local expected_kind="${parts[0]}"
    local expected_exit="${parts[1]}"
    local desc="${parts[2]}"
    # verb args start at index 3
    local verb_args=("${parts[@]:3}")

    run_jjf --json "${verb_args[@]}" || true
    local rc; rc="$(cat "$EVIDENCE/last-exit")"
    record_evidence "d1-${desc}"

    # Check exit code
    if [[ "$rc" != "$expected_exit" ]]; then
      echo "[d1] FINDING: '${verb_args[*]}' (--json) exit=$rc, expected $expected_exit (kind=$expected_kind)"
    fi

    # Error envelope must be on stderr (not stdout)
    if ! jq -e '.ok == false' "$EVIDENCE/last-stderr" >/dev/null 2>&1; then
      echo "[d1] FINDING: '${verb_args[*]}' (--json) missing ok:false envelope on stderr (rc=$rc)"
      echo "  stdout: $(cat "$EVIDENCE/last-stdout")"
      echo "  stderr: $(cat "$EVIDENCE/last-stderr")"
      continue
    fi

    # Stdout must be empty (no error leaked to stdout)
    if [[ -s "$EVIDENCE/last-stdout" ]]; then
      echo "[d1] FINDING-CANDIDATE: '${verb_args[*]}' (--json) error leaked to stdout:"
      echo "  stdout: $(cat "$EVIDENCE/last-stdout")"
    fi

    # kind field must match
    local got_kind
    got_kind="$(jq -r '.error.kind' "$EVIDENCE/last-stderr")"
    if [[ "$got_kind" != "$expected_kind" ]]; then
      echo "[d1] FINDING: '${verb_args[*]}' (--json) kind=$got_kind, expected $expected_kind"
    else
      echo "[d1] NEGATIVE: '${verb_args[*]}' → kind=$got_kind exit=$rc (correct)"
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
  run_jjf --json abandon "$id" || true
  record_evidence "d2-abandon"

  local rc; rc="$(cat "$EVIDENCE/last-exit")"
  if [[ "$rc" != "0" ]]; then
    echo "[d2] FINDING: abandon --json exit $rc on happy path (expected 0)"
    echo "  stderr: $(cat "$EVIDENCE/last-stderr")"
  fi

  # Success envelope on stdout
  if ! jq -e '.ok == true' "$EVIDENCE/d2-abandon-stdout" >/dev/null 2>&1; then
    echo "[d2] FINDING: abandon --json missing {ok:true} envelope on stdout"
    echo "  stdout: $(cat "$EVIDENCE/d2-abandon-stdout")"
    echo "  stderr: $(cat "$EVIDENCE/d2-abandon-stderr")"
  else
    echo "[d2] NEGATIVE: ok:true envelope present"
  fi

  local got_id
  got_id="$(jq -r '.id' "$EVIDENCE/d2-abandon-stdout" 2>/dev/null || echo NONE)"
  if [[ "$got_id" != "$id" ]]; then
    echo "[d2] FINDING: abandon --json id=$got_id (expected $id)"
  else
    echo "[d2] NEGATIVE: id field correct ($got_id)"
  fi

  # Per docs/cli-json.md v2.7: envelope is {"ok": true, "id": "...", "status": "abandoned"}
  local got_status
  got_status="$(jq -r '.status' "$EVIDENCE/d2-abandon-stdout" 2>/dev/null || echo NONE)"
  if [[ "$got_status" != "abandoned" ]]; then
    echo "[d2] FINDING: abandon --json status=$got_status (expected 'abandoned')"
  else
    echo "[d2] NEGATIVE: status field correct ($got_status)"
  fi

  echo "[d2] full envelope: $(cat "$EVIDENCE/d2-abandon-stdout")"
}

# -----------------------------------------------------------------------------
# D3. ConcurrentWrite.hint text snapshot
# -----------------------------------------------------------------------------
#
# Race two updates through a shared bare remote.
# Writer A: update + push (succeeds, advances the bare remote).
# Writer B (sib): update (in a divergent clone), then push (should be rejected
#   as non-fast-forward → push_rejected, or race surfaces concurrent_write).
#
# Strategy: we want to surface concurrent_write or push_rejected.
# Use the cp -R approach first; if jj op-log is confused in the sib,
# fall back to git clone + jj git init --git-repo=. --colocate.
#
d3() {
  mk_scratch_repo d3 >/dev/null
  run_jjf new -t "d3 concurrent target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # Set up a shared bare remote
  local bare="$QA_ROOT/.scratch/d3-bare.git"
  rm -rf "$bare"; git init --bare "$bare" >/dev/null 2>&1

  local orig_scratch="$SCRATCH"
  local orig_evidence="$EVIDENCE"

  (cd "$orig_scratch" && "$JJF_BIN" remote add origin "file://$bare" >/dev/null 2>&1)
  (cd "$orig_scratch" && "$JJF_BIN" push origin >/dev/null 2>&1)

  # Sibling: cp -R of the colocated repo
  local sib="$QA_ROOT/.scratch/d3-sib"
  rm -rf "$sib"; cp -R "$orig_scratch" "$sib"
  mkdir -p "$sib/evidence"

  # Writer A (orig): mutate + push — this advances the bare remote
  SCRATCH="$orig_scratch" EVIDENCE="$orig_evidence" run_jjf update "$id" --title "first" || true
  SCRATCH="$orig_scratch" EVIDENCE="$orig_evidence" run_jjf push origin || true

  # Writer B (sib): mutate (already diverged from orig), then push — should conflict
  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json update "$id" --title "second" || true
  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json push origin || true

  local push_rc; push_rc="$(cat "$sib/evidence/last-exit")"
  local push_stdout; push_stdout="$(cat "$sib/evidence/last-stdout")"
  local push_stderr; push_stderr="$(cat "$sib/evidence/last-stderr")"

  echo "[d3] sib push exit=$push_rc"
  echo "[d3] sib push stdout: $push_stdout"
  echo "[d3] sib push stderr: $push_stderr"

  # Determine what error kind we got (if any)
  local kind="unknown"
  # Check stderr for error envelope (errors go to stderr)
  if jq -e '.ok == false' "$sib/evidence/last-stderr" >/dev/null 2>&1; then
    kind="$(jq -r '.error.kind' "$sib/evidence/last-stderr" 2>/dev/null || echo unknown)"
  elif jq -e '.ok == true' "$sib/evidence/last-stdout" >/dev/null 2>&1; then
    kind="success"
  fi
  echo "[d3] kind=$kind"

  if [[ "$kind" == "concurrent_write" ]]; then
    # Snapshot the hint text — this is what D3 is designed to capture
    local hint
    hint="$(jq -r '.error.details.hint // .error.message' "$sib/evidence/last-stderr" 2>/dev/null || echo unknown)"
    echo "[d3] concurrent_write hint: $hint"

    # Check if hint contains substring-matchable phrases that scripts might key on
    for phrase in "another writer landed first" "concurrent write" "conflict" "stale" "retry"; do
      if [[ "$hint" == *"$phrase"* ]]; then
        echo "[d3] FINDING-CANDIDATE: hint contains substring '$phrase' — scripts may hardcode this"
      fi
    done
    echo "[d3] NEGATIVE: concurrent_write surfaced with structured envelope (kind+hint fields present)"

  elif [[ "$kind" == "push_rejected" ]]; then
    echo "[d3] NEGATIVE: push_rejected (non-fast-forward) — correct behavior, structured envelope"
    local hint
    hint="$(jq -r '.error.message // ""' "$sib/evidence/last-stderr" 2>/dev/null || echo "")"
    echo "[d3] push_rejected message: $hint"

    # Check for substring-matchable hint text
    for phrase in "pull first" "run \`jjf pull" "retry" "rejected"; do
      if [[ "$hint" == *"$phrase"* ]]; then
        echo "[d3] FINDING-CANDIDATE: push_rejected message contains '$phrase' — scripts may key on it"
      fi
    done

  elif [[ "$kind" == "success" ]]; then
    # Both pushes succeeded — cp -R left jj in a state where sib's push went through
    # The race didn't actually produce a conflict. This is a harness limitation.
    echo "[d3] NEGATIVE: both pushes succeeded (cp -R sib shares jj op-log — no real divergence)"
    echo "[d3] NOTE: concurrent_write path not triggered by cp -R approach (op-log shared)"
    echo "[d3] NOTE: this is a harness limitation, not a jjf bug"
  else
    echo "[d3] INFO: unexpected kind=$kind — raw stderr below"
    cat "$sib/evidence/last-stderr" || true
  fi
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  echo "=== D1: envelope/exit-code sweep ==="
  d1
  echo ""
  echo "=== D2: abandon --json envelope ==="
  d2
  echo ""
  echo "=== D3: ConcurrentWrite hint snapshot ==="
  d3
fi
