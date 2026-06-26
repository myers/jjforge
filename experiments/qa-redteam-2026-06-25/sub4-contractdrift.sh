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

# -----------------------------------------------------------------------------
# D3b. ConcurrentWrite.hint text snapshot — fallback recipe
# -----------------------------------------------------------------------------
#
# 2026-06-26 followup (c67f162): the original d3 used `cp -R` to clone
# the scratch repo to a sibling. That left both clones sharing jj's
# op-log so neither push diverged, and the race never surfaced —
# leaving `ConcurrentWrite.hint` text un-snapshotted (the whole point
# of D3).
#
# This variant uses the recipe the plan documented as a fallback:
# `git clone "$bare" "$sib"; cd "$sib"; jj git init --git-repo=. --colocate`.
# That gives sib a fresh jj op-log over the same git history, so the
# two writers really do diverge against the bare remote.
#
# We capture whichever surfaces first (concurrent_write or
# push_rejected) and probe the message text for substring-matchable
# phrases.
d3b() {
  mk_scratch_repo d3b >/dev/null
  run_jjf new -t "d3b concurrent target"
  local id
  id="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # Set up a shared bare remote
  local bare="$QA_ROOT/.scratch/d3b-bare.git"
  rm -rf "$bare"; git init --bare "$bare" >/dev/null 2>&1

  local orig_scratch="$SCRATCH"
  local orig_evidence="$EVIDENCE"

  (cd "$orig_scratch" && "$JJF_BIN" remote add origin "file://$bare" >/dev/null 2>&1)
  (cd "$orig_scratch" && "$JJF_BIN" push origin >/dev/null 2>&1)

  # Sibling via fallback recipe: `git clone` then `jj git init
  # --colocate` then `jjf pull` to materialize the `refs/jjf/*`
  # state locally. The plan documented `--git-repo=. --colocate`
  # but those flags are mutually exclusive in jj 0.40
  # (`--colocate` alone is the right form when the working dir
  # already has a `.git/`). And `git clone` only fetches
  # `refs/heads/*` and `refs/tags/*` by default — `refs/jjf/*`
  # requires an explicit fetch, which is what `jjf pull` does.
  # The fresh jj op-log + fetched refs/jjf/* is what makes the
  # divergence real (vs `cp -R` which cloned the op-log too and
  # left neither side diverged).
  local sib="$QA_ROOT/.scratch/d3b-sib"
  rm -rf "$sib"
  git clone "file://$bare" "$sib" >/dev/null 2>&1
  (cd "$sib" && jj git init --colocate >/dev/null 2>&1)
  (cd "$sib" && "$JJF_BIN" remote add origin "file://$bare" >/dev/null 2>&1)
  (cd "$sib" && "$JJF_BIN" pull origin >/dev/null 2>&1)
  mkdir -p "$sib/evidence"

  # Writer A (orig): mutate + push — this advances the bare remote
  SCRATCH="$orig_scratch" EVIDENCE="$orig_evidence" run_jjf update "$id" --title "first" || true
  SCRATCH="$orig_scratch" EVIDENCE="$orig_evidence" run_jjf push origin || true

  # Writer B (sib): mutate (diverged from orig — sib pulled before
  # orig's "first" push), then push — should conflict against the
  # bare's updated refs/jjf/issues/* heads
  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json update "$id" --title "second" || true
  SCRATCH="$sib" EVIDENCE="$sib/evidence" run_jjf --json push origin || true

  local push_rc; push_rc="$(cat "$sib/evidence/last-exit")"
  local push_stdout; push_stdout="$(cat "$sib/evidence/last-stdout")"
  local push_stderr; push_stderr="$(cat "$sib/evidence/last-stderr")"

  echo "[d3b] sib push exit=$push_rc"
  echo "[d3b] sib push stdout: $push_stdout"
  echo "[d3b] sib push stderr: $push_stderr"

  # Snapshot the full stderr for archival — this is the canonical
  # record of "what does jjf say when a push diverges" for round
  # 2026-06-25.
  cp "$sib/evidence/last-stderr" "$orig_evidence/d3b-hint-snapshot.txt"
  cp "$sib/evidence/last-stdout" "$orig_evidence/d3b-stdout-snapshot.txt"

  # Determine error kind
  local kind="unknown"
  if jq -e '.ok == false' "$sib/evidence/last-stderr" >/dev/null 2>&1; then
    kind="$(jq -r '.error.kind' "$sib/evidence/last-stderr" 2>/dev/null || echo unknown)"
  elif jq -e '.ok == true' "$sib/evidence/last-stdout" >/dev/null 2>&1; then
    kind="success"
  fi
  echo "[d3b] kind=$kind"

  # Extract the hint/message text (whichever the envelope carries).
  # Reading `details.hint` preferentially is correct — that's the
  # contract surface for the operator advisory.
  local hint=""
  local message=""
  if [[ "$kind" != "unknown" && "$kind" != "success" ]]; then
    hint="$(jq -r '.error.details.hint // .error.message // ""' "$sib/evidence/last-stderr" 2>/dev/null || echo "")"
    message="$(jq -r '.error.message // ""' "$sib/evidence/last-stderr" 2>/dev/null || echo "")"
    echo "[d3b] hint/message: $hint"
  fi

  # Substring-match probe across both phrase sets (concurrent_write
  # and push_rejected). Any hit is a contract-drift candidate: the
  # message text isn't stable contract, so scripts depending on it
  # will break when the message gets edited.
  #
  # Note (2026-06-26, 88e4d6b): after the push_rejected reshape,
  # "retry" and "run `jjf pull" appearing here is EXPECTED — they
  # are the intentional `details.hint` text (the contract surface
  # for the operator advisory), not stderr leakage. The hits
  # this probe is really looking for are "fetch first" (raw
  # libgit2 token, git-version-dependent) and "non-fast-forward"
  # leaking out of `stderr_raw` into the contract surface.
  local matched=0
  for phrase in \
    "another writer landed first" \
    "concurrent write" \
    "concurrent" \
    "conflict" \
    "stale" \
    "retry" \
    "pull first" \
    "run \`jjf pull" \
    "rejected" \
    "non-fast-forward" \
    "fetch first"
  do
    if [[ "$hint" == *"$phrase"* ]]; then
      echo "[d3b] FINDING-CANDIDATE: $kind hint contains substring '$phrase' — scripts may hardcode this; file as sev:contract-drift if the message isn't stabilized or split into structured fields"
      matched=1
    fi
  done

  # Stronger check: the `message` field itself must NOT carry raw
  # git stderr tokens. These are the version-dependent phrases that
  # were leaking pre-88e4d6b. If any of these appear in `message`,
  # the contract drift is re-emerging.
  if [[ "$kind" == "push_rejected" ]]; then
    local message_clean=1
    for raw_token in \
      "fetch first" \
      "hint: Updates were rejected" \
      "refs/jjf/issues/" \
      "git push --help"
    do
      if [[ "$message" == *"$raw_token"* ]]; then
        echo "[d3b] FINDING: push_rejected MESSAGE field carries raw git stderr token '$raw_token' — contract drift, scripts pushed to parse message"
        message_clean=0
      fi
    done
    if [[ "$message_clean" == "1" ]]; then
      echo "[d3b] OK: push_rejected message field is free of raw git stderr tokens"
    fi
  fi

  if [[ "$kind" == "concurrent_write" || "$kind" == "push_rejected" ]]; then
    if [[ "$matched" == "0" ]]; then
      echo "[d3b] NEGATIVE: $kind surfaced with structured envelope; hint text contains none of the watched substrings"
    fi
  elif [[ "$kind" == "success" ]]; then
    echo "[d3b] FINDING: both pushes succeeded under the fallback recipe — divergence didn't trigger an error envelope (expected concurrent_write or push_rejected)"
  else
    echo "[d3b] INFO: unexpected kind=$kind — raw stderr archived at $orig_evidence/d3b-hint-snapshot.txt"
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
  echo ""
  echo "=== D3b: ConcurrentWrite hint snapshot (fallback recipe) ==="
  d3b
fi
