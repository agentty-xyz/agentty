---
name: review
description: Guide for reviewing code changes (uncommitted or on a branch), existing code, and the project in general, providing a structured review report.
---

# Code Review Skill

Use this skill when asked to review changes (uncommitted, staged, or committed on a feature branch), existing code files, or the overall project.

## Workflow

1. **Gather Context**
   - For uncommitted/staged changes: Run `git diff HEAD` or `git diff --staged`.
   - For a feature branch: Identify the base branch and run `git diff <base_branch>...HEAD`.
   - For existing code: Use file reading and searching tools to inspect the files and project structure.
   - Always verify the project's specific conventions and architectural guidelines (e.g., from `AGENTS.md`) to inform your review.

2. **Analyze the Code**
   - Check for adherence to project style guides (e.g., formatting, naming conventions, docstrings).
   - Evaluate logic correctness, test coverage, edge cases, and error handling.
   - Look for security vulnerabilities, performance bottlenecks, and architectural issues.
   - Ensure new dependencies or major changes align with project rules.

3. **Generate the Review Report**
   - Structure your findings into a clear, categorized report.
   - Classify issues by severity: **Critical**, **High**, **Medium**, or **Low**.
   - For each issue, provide a brief description and an exact, actionable recommendation or fix (including code snippets when applicable).

### Review Report Format

```markdown
# Review Report

## Summary
[Brief summary of the changes or code reviewed and overall impressions]

## Critical Issues
[Issues that cause immediate failures, security risks, or block progress. Must be fixed.]
- **[Issue Title]:** [Description]
  - **Recommendation/Fix:** [Actionable advice or exact code fix]

## High Issues
[Significant issues like major bugs, missing tests, or severe architectural deviations.]
- **[Issue Title]:** [Description]
  - **Recommendation/Fix:** [Actionable advice or exact code fix]

## Medium Issues
[Style violations, suboptimal performance, missing documentation, or minor edge cases.]
- **[Issue Title]:** [Description]
  - **Recommendation/Fix:** [Actionable advice or exact code fix]

## Low Issues (Nitpicks)
[Minor stylistic suggestions, small improvements, or general thoughts.]
- **[Issue Title]:** [Description]
  - **Recommendation/Fix:** [Actionable advice or exact code fix]

## Architectural & Maintainability Recommendations
[High-level recommendations for making the project more maintainable, modular, and extendable.]
- **[Area/Component]:** [Description of the current state]
  - **Recommendation:** [Actionable advice on improving modularity, separation of concerns, or testability]
```
