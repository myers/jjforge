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

  # Build titles array with explicit Unicode escapes to avoid shell encoding issues
  local zwsp; zwsp=$'\xe2\x80\x8b'       # U+200B ZERO WIDTH SPACE
  local zwj; zwj=$'\xe2\x80\x8d'         # U+200D ZERO WIDTH JOINER
  local rtl; rtl=$'\xe2\x80\xae'         # U+202E RIGHT-TO-LEFT OVERRIDE
  local comb_acute; comb_acute=$'\xcc\x81'  # U+0301 COMBINING ACUTE ACCENT
  local cyr_a; cyr_a=$'\xd0\xb0'         # U+0430 CYRILLIC SMALL LETTER A

  local -a titles=(
    "zwsp${zwsp}in-middle"
    "zwj${zwj}joiner"
    "rtl${rtl}override"
    "a${comb_acute}-combined"
    "${cyr_a}dmin"
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
  if [[ "$(cat "$EVIDENCE/last-exit")" != "0" ]]; then
    echo "[c2] FINDING: ls non-zero after unicode titles"
  fi
}

# -----------------------------------------------------------------------------
# C3. Oversized inputs — title, body, labels, comments
# -----------------------------------------------------------------------------
c3() {
  mk_scratch_repo c3 >/dev/null

  # 10MB title — expect rejection at validation. Write to a file to avoid ARG_MAX issues.
  local big_title_file="$EVIDENCE/c3-bigtitle.txt"
  head -c 10000000 /dev/urandom | base64 | tr -d '\n' | head -c 10000000 > "$big_title_file"
  local big_title; big_title="$(cat "$big_title_file")"
  timeout 30 bash -c "
    cd '$SCRATCH' && '$JJF_BIN' new -t \"\$(cat '$big_title_file')\" \
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
    cd '$SCRATCH' && '$JJF_BIN' new -t 'c3 bigbody' -F '$body_file' \
      >'$EVIDENCE/c3-bigbody-stdout' 2>'$EVIDENCE/c3-bigbody-stderr'
    echo \$? > '$EVIDENCE/c3-bigbody-exit'
  " || echo "[c3] FINDING: 10MB body hung jjf for >30s"

  if grep -qi "panicked" "$EVIDENCE/c3-bigbody-stderr" 2>/dev/null; then
    echo "[c3] FINDING: panic on 10MB body"
  fi
  local body_rc; body_rc="$(cat "$EVIDENCE/c3-bigbody-exit" 2>/dev/null || echo timeout)"
  # Post-fix (issue 679444a, 2026-06-26): jjforge declared a
  # 65,536-byte cap matching GitHub's documented limit. A 10MB body
  # should reject at preflight (exit 2) with a `body_too_large`
  # envelope. Anything else is a regression: exit 0 means the cap
  # got removed; any other exit means the rejection path broke.
  case "$body_rc" in
    2) echo "[c3] NEGATIVE: 10MB body rejected at preflight (exit 2)" ;;
    0) echo "[c3] FINDING: 10MB body accepted (no length cap)" ;;
    *) echo "[c3] FINDING-CANDIDATE: 10MB body exit $body_rc — check stderr" ;;
  esac

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
    cd '$SCRATCH' && '$JJF_BIN' ls \
      >'$EVIDENCE/c3-ls-bigchain-stdout' 2>'$EVIDENCE/c3-ls-bigchain-stderr'
    echo \$? > '$EVIDENCE/c3-ls-bigchain-exit'
  " || echo "[c3] FINDING: ls hung >30s with 1k-label issue"

  if grep -qi "panicked" "$EVIDENCE/c3-ls-bigchain-stderr" 2>/dev/null; then
    echo "[c3] FINDING: panic on ls with 1k-label issue"
  fi

  # 1k comments on one issue.
  run_jjf new -t "c3 comment-explosion"
  local cid; cid="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
  for i in $(seq 1 1000); do
    echo "comment $i" | run_jjf comment "$cid" -F -
    if [[ "$(cat "$EVIDENCE/last-exit")" != "0" ]]; then
      echo "[c3] info: comment add failed at i=$i"
      break
    fi
  done
  timeout 30 bash -c "
    cd '$SCRATCH' && '$JJF_BIN' show '$cid' --json \
      >'$EVIDENCE/c3-show-1k-comments-stdout' 2>'$EVIDENCE/c3-show-1k-comments-stderr'
    echo \$? > '$EVIDENCE/c3-show-1k-comments-exit'
  " || echo "[c3] FINDING: show hung >30s on 1k-comment issue"

  if grep -qi "panicked" "$EVIDENCE/c3-show-1k-comments-stderr" 2>/dev/null; then
    echo "[c3] FINDING: panic on show with 1k-comment issue"
  fi
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

  # Variant helper: inject a malformed issue.json and probe jjf show --json
  malform() {
    local payload="$1"
    local label="$2"
    local blob
    blob="$(cd "$SCRATCH" && printf '%s' "$payload" | git hash-object -w --stdin)"
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
    # Reset ref back to parent so each variant starts clean
    (cd "$SCRATCH" && git update-ref "$ref" "$parent")
  }

  # Get a baseline good payload to mutate (use show --json output directly)
  run_jjf show --json "$id"
  cp "$EVIDENCE/last-stdout" "$EVIDENCE/c4-baseline.json"

  # extra-field (forward-compat probe)
  local extra_payload
  extra_payload="$(jq '. + {"unknown_field_xyz": true}' "$EVIDENCE/c4-baseline.json")"
  malform "$extra_payload" "extra-field"

  # missing-status
  local missing_status_payload
  missing_status_payload="$(jq 'del(.status)' "$EVIDENCE/c4-baseline.json")"
  malform "$missing_status_payload" "missing-status"

  # status-wrong-type
  local status_int_payload
  status_int_payload="$(jq '.status = 42' "$EVIDENCE/c4-baseline.json")"
  malform "$status_int_payload" "status-int"
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  c1; c2; c3; c4
fi
