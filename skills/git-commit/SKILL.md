---
name: git-commit
description: Guide for preparing git commits and pull-request descriptions in this repository, including context gathering and repository-specific commit message conventions.
---

# Git Commit Workflow

Use this skill when preparing commits or authoring pull-request descriptions.

## Workflow

1. **Gather commit context in one tool call**

   - Run:
     `git status --short && git diff && git diff --staged && git log -n 3 --format="---%n%B"`.
   - `git diff` alone hides staged and untracked work, so a message written from it can
     silently omit part of the commit. Read the staged diff too.
   - Neither diff shows the contents of untracked files, only their names in
     `git status --short`. Read the contents of every untracked file the commit will
     include; otherwise a whole new file is invisible to the message.

1. **Write for a reviewer who has not seen the implementation**

   - Assume the reader has the title, the body, and no appetite for the diff.

1. **Write the commit title**

   - Use one concise line in imperative mood (example: `Fix cursor offset`), with no
     trailing period.
   - State the outcome the change achieves, not the activity performed. Prefer
     `Prevent duplicate session commits` over `Update session commit handling`.

1. **Write the body only when it adds information**

   - Leave one blank line between the title and the body, and write bullets as concise
     declarative statements. Imperative mood belongs to the title; motivation and impact
     are facts, so forcing them into commands distorts them.
   - Admit a bullet only when it states why the change exists, a distinct behavior or
     design decision a reviewer must know, or migration, compatibility, configuration,
     or operational impact.
   - Omit file inventories, line counts, routine tests, formatting, documentation
     updates, and mechanical refactors unless they change how the result is used or
     reviewed.
   - Default to at most three `-` bullets, one point per line. Merge bullets that serve
     the same outcome, never restate the title, and drop near-duplicate wording.
   - Never infer rationale the change context does not support. Omit motivation rather
     than guess it.

1. **Avoid Conventional Commit prefixes**

   - Do not use prefixes such as `feat:` or `fix:` in commit titles.

1. **Apply repository-specific commit rules**

   - Never use `--no-verify` with git commands to bypass `prek`-managed hooks.
   - Do not add `Co-Authored-By` trailers or AI attribution to commit messages.

## Pull Request Descriptions

Apply this section only when you author a pull-request description yourself. Publishing
a session sends the commit title and body straight to the forge as the review-request
body, which bypasses the template, so never reshape a commit body into these sections to
compensate — hold the commit message to the rules above.

When you do author a description, fill `.github/pull_request_template.md` as:

- `Why?`: one short paragraph of problem or motivation.
- `What?`: two to four outcome-level bullets, not a file-by-file walkthrough.
- `Project impact`: material user-facing, operational, or maintenance consequences only.
  Delete the section when the change has none rather than writing `N/A`.
