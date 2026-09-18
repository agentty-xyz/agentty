---
name: review
description: Guide for reviewing code changes (uncommitted or on a branch), existing code, and the project in general, providing a structured review report.
---

# Code Review Skill

Use this skill when asked to review changes (uncommitted, staged, or committed on a
feature branch), existing code files, or the overall project.

## Workflow

1. **Gather Context**

   - For uncommitted/staged changes: Run `git diff HEAD` or `git diff --staged`.
   - For a feature branch: Identify the base branch and run
     `git diff <base_branch>...HEAD`.
   - For existing code: Use file reading and searching tools to inspect the files and
     project structure.
   - Always verify the project's specific conventions and architectural guidelines
     (e.g., from `AGENTS.md`) to inform your review.
   - Keep review mode inspection-only by default. Do not run build, test, formatter,
     linter, package-manager, dev-server, static analyzer, or long-running commands
     unless the user explicitly requests verification. If verification would be useful,
     recommend the exact command instead of running it. Read-only documentation lookup
     through Context7 or official sources is permitted research, not execution of
     repository checks; reuse relevant documentation already fetched for this version.

1. **Establish Actionable Findings**

   - Inspect relevant unchanged source, call sites, tests, and accepted decisions before
     reporting an omission or regression. Absence from a diff is not absence from the
     repository.
   - Require a concrete triggering scenario, cited evidence, practical impact, and an
     actionable correction for every finding. Label uncertainty; do not present a
     hypothetical issue as an observed failure.
   - Prioritize correctness, security, data loss, build failures, reliability,
     performance, and maintenance risks with concrete impact. Formatting preferences,
     missing comments, or architectural taste alone are not medium-severity defects.
   - Honor accepted trade-offs. Reopen resolved suggestions only with new evidence and
     explain what changed.

1. **Report the Review**

   - Lead with actionable findings, ordered by severity, using repository-relative
     locations. Include the trigger, evidence, impact, and correction for each.
   - A clean review is valid: say no actionable findings when the evidence supports it.
     Do not invent findings to fill severity categories or meet a quota.
   - Keep optional improvements separate from defects and include them only when useful
     to the requested scope. Avoid repetitive file inventories and empty categories.
   - State what was inspected and what was not verified. Suggested commands are not
     checks that ran.

## Decision Examples

- An import is absent from the diff but present in unchanged source: no finding.
- A documented credential-dependent live test is ignored in ordinary CI: no finding
  unless a concrete required behavior lacks deterministic coverage.
- A retry drops the caller's cancellation token on a reproducible path: report the
  trigger, affected operation, source evidence, and correction.
