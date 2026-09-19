---
name: development
description: Prepare an Agentty development environment, select validation suites, or regenerate SQLx offline metadata. Use for these workflows; development policy and required gates remain in AGENTS.md.
---

# Development Workflows

Read only the reference needed for the current task:

- [Setup](references/setup.md): first checkout, missing tools, and local site preview.
- [Validation](references/validation.md): focused iteration, affected-package tests,
  coverage troubleshooting, and instruction checks.
- [SQLx metadata](references/sqlx.md): checked-query changes in a persistence crate.

The root `AGENTS.md` defines required gates and when prior results remain valid.
`.pre-commit-config.yaml` defines executable checks. These recipes explain their use;
they do not replace either source of truth. Use `skills/feature-test/SKILL.md` for
Agentty PTY scenarios and recordings.

For model prompts, protocol schemas, or contributor instructions, follow
`references/prompts.md` for prompt-specific validation and evaluation.
