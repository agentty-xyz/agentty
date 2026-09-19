---
name: tech-debt
description: Sweep the codebase for tech debt and return a prioritized markdown task list of findings.
---

# Tech Debt Skill

Use this skill when asked to find tech debt, stale patterns, or maintenance issues in a
codebase.

The user may scope the sweep to specific directories or modules (e.g., "sweep
`crates/agentty/src/app`"). When a scope is given, restrict traversal to those paths.
When no scope is given, sweep the full codebase.

## Workflow

1. **Read Project Context**

   - Read the root `AGENTS.md` for project conventions, architecture references, and
     style rules.
   - Identify which directories and modules are relevant to the sweep (respecting any
     user-provided scope).

1. **Traverse Target Directories**

   - Navigate into each target directory and read the nearest available `AGENTS.md` for
     local conventions, entry points, and change guidance.
   - Use the architecture docs and module routers to prioritize which files and modules
     to inspect when no deeper local guide exists.

1. **Analyze for Tech Debt**

   - **TODOs and FIXMEs:** Find `TODO`, `FIXME`, `HACK`, and `XXX` comments that
     indicate deferred work.
   - **Outdated Patterns:** Identify deprecated API usage, legacy code paths retained
     without justification, and patterns that conflict with current project conventions.
   - **Inconsistent Error Handling:** Flag mixed error handling strategies (e.g.,
     `unwrap()` alongside proper `Result` propagation), swallowed errors, and missing
     error context.
   - **Missing Documentation:** Note public types, traits, and functions lacking doc
     comments, especially in areas with complex logic.
   - **Dependencies:** Require a concrete security, compatibility, support, or
     maintenance cost before recommending an upgrade. Reproducibility pins and older
     versions are not debt by themselves. Check release policy and actual usage;
     identify unused dependencies or obsolete feature flags with evidence.
   - **Dead Code:** Identify unused functions, modules, imports, or feature gates that
     can be removed.
   - **Test Gaps:** Identify critical behavior without suitable coverage. Inspect
     ignored tests' reasons and deterministic boundary tests first: credential-dependent
     or expensive live tests may be intentionally excluded from ordinary CI.
   - **Convention Violations:** Check code against the project conventions discovered in
     Step 1 (e.g., naming rules, module layout, import style, constructor patterns) and
     flag deviations.

1. **Return Findings as a Task List**

   - Structure your answer as a prioritized markdown task list.
   - Require a triggering scenario or recurring maintenance cost, source evidence,
     practical impact, and a specific action. Do not infer missing behavior from a diff
     without inspecting unchanged code. Respect documented trade-offs.
   - Return no findings when nothing actionable is supported; omit empty categories.
     Keep optional polish separate from defects.
   - Use the format below for each finding.

### Task List Format

```markdown
# Tech Debt Report

## Summary
[Brief overview: total finding count, highest-risk area, and overall codebase health impression.]

## Critical
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and why it is critical.]

## High
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]

## Medium
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]

## Low
- [ ] **[Title]** — `[file/module scope]`
  [Description of the issue and recommended action.]
```

### Priority Guidelines

| Priority     | Criteria                                                                                                                   |
| ------------ | -------------------------------------------------------------------------------------------------------------------------- |
| **Critical** | Causes runtime failures, data loss, or blocks other work.                                                                  |
| **High**     | Significant maintenance burden, outdated patterns actively causing confusion, or missing error handling in critical paths. |
| **Medium**   | Concrete recurring maintenance cost or test gaps with a demonstrated affected path.                                        |
| **Low**      | Cosmetic issues, minor naming improvements, or optional cleanup with no immediate impact.                                  |
