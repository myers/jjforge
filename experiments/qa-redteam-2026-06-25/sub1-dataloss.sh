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
    echo "[a1] FINDING-CANDIDATE: assignee with tab+BOM rejected (exit $(cat "$EVIDENCE/last-exit")) — may be intentional validation"
    cat "$EVIDENCE/last-stderr" >&2
  else
    record_evidence "assignee-accepted"
    local id
    id="$(cat "$EVIDENCE/assignee-accepted-stdout" | tr -d '\n')"
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
    else
      echo "[a1] NEGATIVE: assignee round-trip preserved $(printf '%q' "$got")"
    fi
  fi

  # 1.b — comment --author with embedded JSON metachar and tab.
  run_jjf new -t "a1 author target"
  local cid
  cid="$(cat "$EVIDENCE/last-stdout" | tr -d '\n')"
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
  else
    echo "[a1] NEGATIVE: comment author round-trip preserved $(printf '%q' "$author_got")"
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
  else
    echo "[a1] NEGATIVE: memory value round-trip preserved $(printf '%q' "$mem_got")"
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
  id="$(cat "$EVIDENCE/last-stdout" | tr -d '\n')"
  record_evidence "create"

  # Set up a bare remote and two siblings cloned from SCRATCH.
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
  mkdir -p "$side_a/evidence" "$side_b/evidence"

  # Land a divergent title update on each side (same clock = LWW tiebreaker).
  local saved_scratch="$SCRATCH"
  local saved_evidence="$EVIDENCE"

  SCRATCH="$side_a"; EVIDENCE="$side_a/evidence"
  run_jjf update "$id" --title "A-title"
  record_evidence "side-a-update"

  SCRATCH="$side_b"; EVIDENCE="$side_b/evidence"
  run_jjf update "$id" --title "B-title"
  record_evidence "side-b-update"

  # Push both sides to the bare remote (one will lose the race).
  (cd "$side_a" && "$JJF_BIN" push origin >/dev/null) || true
  (cd "$side_b" && "$JJF_BIN" push origin >/dev/null) || true

  # Pull each side so they converge.
  SCRATCH="$side_a"; EVIDENCE="$side_a/evidence"
  run_jjf pull origin
  record_evidence "side-a-pull"

  SCRATCH="$side_b"; EVIDENCE="$side_b/evidence"
  run_jjf pull origin
  record_evidence "side-b-pull"

  # Read what each side thinks the title is.
  SCRATCH="$side_a"; EVIDENCE="$side_a/evidence"
  run_jjf show --json "$id"
  local title_a
  title_a="$(jq -r '.title' "$side_a/evidence/last-stdout")"

  SCRATCH="$side_b"; EVIDENCE="$side_b/evidence"
  run_jjf show --json "$id"
  local title_b
  title_b="$(jq -r '.title' "$side_b/evidence/last-stdout")"

  # Restore globals.
  SCRATCH="$saved_scratch"
  EVIDENCE="$saved_evidence"

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
  id="$(cat "$EVIDENCE/last-stdout" | tr -d '\n')"

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
    "a3 trailer target")
      echo "[a3] NEGATIVE: known set-title op did not hijack (title='$title_after')"
      ;;
    "<no-json>")
      echo "[a3] FINDING: show --json returned non-JSON output (exit $rc)"
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

  echo "[a3] done; evidence at $EVIDENCE"
}

# -----------------------------------------------------------------------------
# A4. Corrupt-ref regression check
# -----------------------------------------------------------------------------
a4() {
  mk_scratch_repo a4 >/dev/null
  run_jjf new -t "a4 corruption target"
  local id
  id="$(cat "$EVIDENCE/last-stdout" | tr -d '\n')"
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
  if grep -qiE "unreadable|warning|skipped" "$EVIDENCE/ready-after-corrupt-stderr"; then
    echo "[a4] OK: ready surfaced unreadable-ref warning"
  else
    echo "[a4] FINDING: ready silently dropped corrupt ref (no stderr warning)"
  fi

  if grep -qF "$id" "$EVIDENCE/ls-after-corrupt-stdout"; then
    echo "[a4] FINDING: corrupt ref still appeared in ls output"
  else
    echo "[a4] NEGATIVE: corrupt ref correctly excluded from ls output"
  fi

  echo "[a4] done; evidence at $EVIDENCE"
}

# When invoked as a script, run all four serially.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  a1; a2; a3; a4
fi
