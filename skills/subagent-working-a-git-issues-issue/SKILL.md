---
name: subagent-working-a-git-issues-issue
description: Use when dispatching a subagent (or working as one) to do focused work — research, implementation, design, triage — on a single git-issues issue and report the outcome back on the issue itself. Triggers on "git-issues", "iss", "issue", or "ticket" in the dispatch.
---

# Subagent working a git-issues issue

The issue is the deliverable. Your closing comment and status change are the
artifact a human or the next subagent will see. Everything else you did is
invisible. Match the recipe below or the work doesn't count.

## What "done" looks like

You produced **two things on the issue**, in this order:

1. **A closing comment on the issue** matching the recipe below.
2. **A status change** — closed if the work is complete, or `Blocked` /
   left open with the comment explaining why if not.

`iss show <id>` after you finish must show both, or you are not done.

## Closing-comment recipe

The comment is exactly these four sections, in this order, with these
headings. Fill each one. None are optional. Order is the contract.

```
## Findings

<What you learned. Quote sources. Link file paths with line numbers
(file_path:line). If you ran code, say what and what it told you.
This is the substance.>

## Recommendation

<One paragraph. The thing the next person should do based on Findings.>

## Confidence

<low | medium | high>. Then one sentence of why that level.

## Open follow-ups

<Bulleted list. New issues you filed (with their ids), questions you
hit that need their own ticket, or "none.">
```

Send via stdin so multi-line content works:

```bash
cat <<'EOF' | iss comment <id> -F -
## Findings
...
## Recommendation
...
## Confidence
medium. <why>
## Open follow-ups
- none.
EOF
```

## Actor attribution

If the orchestrator set `ISS_ACTOR=<your-name>` in your environment (or
told you to pass `--actor <name>` on mutating `iss` calls), use that
attribution on every `iss update --claim` / `iss comment` / `iss close`
you make. Parallel subagents share the same `git config user.name`; the actor
override is the only way the eventual reader can tell who did what.
Chain precedence: `--actor <name>` > `ISS_ACTOR` env > `git config user.name`.

If neither was set, don't invent one — the orchestrator decides whether
attribution matters for this round.

## REQUIRED steps

- [ ] `iss show <id>` — read the body end-to-end before starting. The body
      AND every comment are context; the latest comment may carry crucial
      direction the body doesn't.
- [ ] Do the work. If you write throwaway code, put it under
      `experiments/<topic>/` in the repo, not at the root. Strip nested
      `.git/` and `.jj/` from experiment subdirs before committing
      (`find experiments/<topic> -name ".git" -exec rm -rf {} +`; same for
      `.jj`).
- [ ] Post the closing comment using the recipe above.
- [ ] If work is complete: `iss close <id>`.
      If blocked on something concrete: `iss block <id> --reason "<why>"`.
      If genuinely incomplete and not blocked: leave open. The Findings
      section MUST explain why.
- [ ] `iss show <id>` again. Verify the new comment is at the bottom and
      (if applicable) status changed. If not, fix it.

## Boundary

- Do not edit the issue title or body. Comments only.
- Do not edit the body's checklist. If acceptance criteria are met,
  the closing status change is the signal — not editing `[ ]` to `[x]`
  in the original body.
- Do not modify other issues unless your findings genuinely change them.
  When you must, leave a cross-link comment; do not edit their bodies.
- Do not `git push` or `iss push` to a remote. The orchestrator decides
  when to push.
- Do not close issues that aren't yours.

## Return value to the orchestrator

After the recipe steps land on the issue, summarize back to the
dispatcher under 200 words: the verdict, the issue id you closed (or
left open and why), and any follow-ups you filed.

## Common mistakes

| Mistake | Fix |
|---|---|
| Comment uses your own headings instead of the four-section recipe | Rewrite using the exact recipe headings; the recipe is the contract |
| Restated the body's checklist as `[x]` inside the comment | Delete that section; closure status change is the only signal needed |
| "Comment landed" in the return value but issue still open | Run `iss close <id>` or explain in Findings why not |
| Confidence implied by tone but not stated | Add the `## Confidence` line with one of `low`/`medium`/`high` |
| Did the work, wrote a great writeup, never posted it | The comment IS the artifact; nothing else counts |
| Orchestrator set `ISS_ACTOR` but the comment landed under the shared `git config user.name` | The env var was in your shell, not in `iss`'s — check `iss show <id>` after; if the actor is wrong, pass `--actor <name>` explicitly on a corrective comment |
| Pre-cutover muscle memory typed `git-bug bug X` | git-issues replaced git-bug on 2026-06-22. Use `iss X`. Pre-cutover history lives in `refs/bugs/*` and is read via `git-bug bug show <id>` — but you write via `iss`. |
