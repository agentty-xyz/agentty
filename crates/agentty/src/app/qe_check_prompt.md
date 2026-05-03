# /qe:check

Audit the current repository for Agentty quality-enforcement readiness. Do not edit
files, run mutating git commands, create commits, or change repository state. Return a
concise markdown report that lists each rule as `Pass`, `Warn`, or `Unknown`, explains
the evidence you found with repository-root-relative paths, and gives concrete
remediation as a prioritized list of actionable High and Medium severity items.

Apply these rules:

- Existing agent instruction files, such as `AGENTS.md`, `CLAUDE.md`, and `GEMINI.md`,
  give agents clear, current, and non-duplicative guidance for the repository. Review
  whichever of these files exist without requiring a predefined file set or layout.
- Agent instruction files describe the project purpose, important entry points,
  mandatory workflow rules, test expectations, documentation sync expectations, and
  validation workflow at the right level of detail.
- Multiple agent instruction files, when present, stay consistent with one another and
  avoid near-identical copies that can drift unless the duplication is clearly
  intentional and maintained.
- `.pre-commit-config.yaml` exists when the repository has automated quality gates, and
  the documented validation workflow points agents to repository-defined hooks instead
  of ad hoc commands.
- Agent instructions document per-turn validation requirements, test coverage
  expectations, feature-test requirements for user-visible behavior, and the rule to
  preserve unrelated user changes when those expectations apply to the repository.
- Architecture documentation exists for repositories with layered or boundary-heavy
  code. Check whether the repository documents module ownership, runtime flow,
  testability boundaries, and change-path guidance in an appropriate location for that
  project.
- Areas that need specialized local guidance have appropriately scoped local agent
  instructions, while instruction files avoid exhaustive inventories and focus on
  purpose, invariants, entry points, change routing, and docs-sync notes.

Use this report shape:

```markdown
## /qe:check

**Verdict:** <healthy | has recommendations | blocked by unknowns>

### Findings

- `Pass` — <rule name>: <repository-root-relative evidence>
- `Warn` — <rule name>: <repository-root-relative evidence>
- `Unknown` — <rule name>: <what could not be verified>

### Action Items

#### High

- <imperative action item that fixes the highest-impact gap>

#### Medium

- <imperative action item for a useful but less urgent improvement>

### Suggested Next Prompt

<one focused prompt the user can send to address the highest-severity action item>
```
