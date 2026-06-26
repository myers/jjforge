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
#
# 2026-06-26 followup (c67f162): the original matrix only asserted on exit
# code and stderr-regex. A verb that succeeds (exit 0) but performs no
# state change on a terminal issue (e.g. `unblock` on `open`, `open` on
# `open`) would not have been flagged — but the wrong-answer class
# explicitly includes silent no-ops. We now read `jjf show --json` after
# every verb and compare the post-status to the spec's status-machine
# expectation. Any mismatch is reported as a finding.
#
# Expected post-status per (source_status, verb) pair, derived from
# `crates/jjf-storage/src/lib.rs` (the unconditional `set_status` path
# for close/open/abandon; `block` / `unblock` rejecting on
# closed/abandoned; `comment` / `update --title` / `label add` /
# `dep add` being status-preserving):
#
#                        verb that succeeds → post-status
#   source │ comment │ updT │ updSO │ block │ unblock │ close │ open │ abandon │ label │ dep
#   ──────┼─────────┼──────┼───────┼───────┼─────────┼───────┼──────┼─────────┼───────┼────
#   open   │ open    │ open │ open  │block. │ open    │closed │ open │ aband.  │ open  │ open
#   closed │ closed  │ closed│ open │  REJ  │   REJ   │closed │ open │ aband.  │ closed│ closed
#   aband. │ aband.  │ aband.│ open │  REJ  │   REJ   │closed │ open │ aband.  │ aband.│ aband.
#   blocked│ blocked │ blocked│ open│block. │ open    │closed │ open │ aband.  │ blocked│ blocked
#
# REJ = expected typed rejection (storage.block/unblock return
# Error::Invalid on closed/abandoned). For REJ cells we assert post-
# status is UNCHANGED from source.
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

  # expected_post_status <source_status> <verb> → echoes expected post-
  # status. The post-status reflects: success-cases follow the
  # status-machine; rejection-cases (REJ) leave the status unchanged.
  expected_post_status() {
    local s="$1"
    local v="$2"
    case "$v" in
      comment_one_line|update_title|label_add_test|dep_add_self)
        # Status-preserving verbs.
        echo "$s"
        ;;
      update_status_open|open_issue)
        # Unconditional re-open via set_status (or update --status open).
        echo "open"
        ;;
      close_issue)
        # Unconditional close via set_status.
        echo "closed"
        ;;
      abandon_issue)
        # Unconditional abandon via set_status.
        echo "abandoned"
        ;;
      block_issue)
        # Rejects on closed/abandoned; otherwise → blocked.
        case "$s" in
          closed|abandoned) echo "$s" ;;
          *) echo "blocked" ;;
        esac
        ;;
      unblock_issue)
        # Rejects on closed/abandoned; otherwise → open.
        case "$s" in
          closed|abandoned) echo "$s" ;;
          *) echo "open" ;;
        esac
        ;;
    esac
  }

  # Cells where the verb is EXPECTED to reject (typed Error::Invalid).
  # For those cells: exit 1 is correct; exit 0 is a finding (silent
  # success that didn't enforce the transition rule).
  is_expected_rejection() {
    local s="$1"
    local v="$2"
    case "$v" in
      block_issue|unblock_issue)
        case "$s" in
          closed|abandoned) return 0 ;;
          *) return 1 ;;
        esac
        ;;
      *) return 1 ;;
    esac
  }

  mk_scratch_repo b2 >/dev/null
  # Create a sibling for dep_add_self exercises.
  run_jjf new -t "b2 sibling for dep" --slug b2-sibling
  local sib
  sib="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"

  # Helper: drive a fresh issue to the requested source status.
  # Echoes the issue id to stdout so the caller can capture it.
  drive_to_status() {
    local target_status="$1"
    local iid
    run_jjf new -t "b2 source=$target_status target" --slug "b2-$target_status-$RANDOM"
    iid="$(grep -oE '[0-9a-f]{7}' "$EVIDENCE/last-stdout" | head -1)"
    case "$target_status" in
      open) ;;
      closed)    run_jjf close "$iid" ;;
      abandoned) run_jjf abandon "$iid" ;;
      blocked)   run_jjf block "$iid" --reason "b2 driver" ;;
    esac
    echo "$iid"
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
      dep_add_self)     run_jjf dep add "$id" "$sib" --kind blocks ;;
    esac
  }

  for s in "${statuses[@]}"; do
    for v in "${verbs[@]}"; do
      local id
      id="$(drive_to_status "$s")"
      exercise_verb "$v" "$id" "$sib"
      local rc; rc="$(cat "$EVIDENCE/last-exit")"
      record_evidence "matrix-$s-$v"

      # Generic exit 1 with no typed JSON envelope is a candidate finding.
      if [[ "$rc" == "1" ]]; then
        # Probe stderr for a typed error indicator (kind=... or invalid_input).
        if ! grep -qiE "invalid_status_transition|invalid[_ ]input|already[_ ]|not[_ ]" "$EVIDENCE/last-stderr"; then
          echo "[b2] FINDING-CANDIDATE: ($s, $v) exit 1 without typed error message"
        fi
      elif [[ "$rc" != "0" && "$rc" != "2" ]]; then
        echo "[b2] FINDING: ($s, $v) unexpected exit $rc"
      fi

      # Post-status assertion (2026-06-26 followup). Read the issue's
      # post-state and compare to the spec's expectation. Silent no-ops
      # surface here: e.g. a verb returning exit 0 on a terminal issue
      # but failing to apply the transition.
      run_jjf show --json "$id"
      local show_rc; show_rc="$(cat "$EVIDENCE/last-exit")"
      if [[ "$show_rc" != "0" ]]; then
        echo "[b2] FINDING: ($s, $v) show --json failed after verb (exit $show_rc)"
        cp "$EVIDENCE/last-stdout" "$EVIDENCE/matrix-$s-$v-show-stdout"
        cp "$EVIDENCE/last-stderr" "$EVIDENCE/matrix-$s-$v-show-stderr"
        continue
      fi
      local got_status
      got_status="$(jq -r '.status' "$EVIDENCE/last-stdout" 2>/dev/null || echo NONE)"
      local want_status
      want_status="$(expected_post_status "$s" "$v")"
      cp "$EVIDENCE/last-stdout" "$EVIDENCE/matrix-$s-$v-show-stdout"

      if is_expected_rejection "$s" "$v"; then
        # Cell where verb is expected to reject.
        if [[ "$rc" == "0" && "$got_status" != "$s" ]]; then
          # Silent acceptance that DID flip the status — clear finding.
          echo "[b2] FINDING: ($s, $v) wrong-answer: verb exit 0 AND status flipped $s → $got_status; spec mandates rejection on terminal/non-applicable source"
        elif [[ "$rc" == "0" && "$got_status" == "$s" ]]; then
          # Silent no-op: exit 0 but post-state unchanged. The
          # verb claimed success without doing anything.
          echo "[b2] FINDING: ($s, $v) silent-no-op: verb exit 0 but post-status unchanged ($got_status); spec mandates a typed rejection"
        fi
      else
        # Cell where verb is expected to apply (or be status-preserving).
        if [[ "$rc" == "0" && "$got_status" != "$want_status" ]]; then
          echo "[b2] FINDING: ($s, $v) wrong-answer: post-status=$got_status, expected $want_status (verb exited 0 but didn't apply the transition)"
        elif [[ "$rc" != "0" && "$got_status" != "$s" ]]; then
          # Verb errored but state changed — corruption.
          echo "[b2] FINDING: ($s, $v) wrong-answer: verb exit $rc but status mutated $s → $got_status (failed mutation should be atomic)"
        fi
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
  run_jjf dep add "$A" "$B" --kind blocks
  assert_exit 0 || echo "[b3] FINDING: dep add blocks:B on A failed unexpectedly"
  run_jjf dep add "$B" "$A" --kind parent-child
  record_evidence "mixed-cycle-attempt"
  if [[ "$(cat "$EVIDENCE/mixed-cycle-attempt-exit")" == "0" ]]; then
    echo "[b3] FINDING: mixed-kind cycle (A blocks B + B parent-of A) accepted"
  else
    echo "[b3] NEGATIVE: mixed-kind cycle rejected at preflight"
  fi

  # (ii) Self-dep through slug resolution.
  run_jjf dep add b3-issue-a "$A" --kind blocks
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
  run_jjf new -t "b3-abandoned B" --slug b3-abandoned-b -d "$AA"
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
