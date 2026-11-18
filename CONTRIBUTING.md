# Contributing

Agentty encourages agent-assisted development. All contributions must follow the
applicable repository instructions. Contributors remain responsible for correctness,
review, and validation evidence, regardless of how a change was authored.

## Start Here

- Read [AGENTS.md](AGENTS.md) and each ancestor guide for the paths you change. These
  files define development policy and required quality gates.
- Use [skills/AGENTS.md](skills/AGENTS.md) to select the relevant task workflow. Read
  detailed references only when needed.
- Follow the [development setup](skills/setup-development/SKILL.md) to prepare a
  checkout. Use the [validation workflow](skills/validate-changes/SKILL.md) for test
  selection and the [SQLx metadata workflow](skills/regenerate-sqlx-metadata/SKILL.md)
  for checked queries.
- Read [rule enforcement](docs/site/content/docs/contributing/rule-enforcement.md) to
  learn where project rules are checked and how exceptions work.

## Preparing a Contribution

Keep changes focused, explain the resulting behavior, and report the checks run and any
remaining verification gaps. Follow the required gates in `AGENTS.md`; executable check
definitions live in [.pre-commit-config.yaml](.pre-commit-config.yaml).

For architecture changes, use the
[change recipes](docs/site/content/docs/architecture/change-recipes.md) and the
documentation routing in `AGENTS.md`. Use the
[Agentty E2E guide](crates/agentty/tests/e2e/AGENTS.md) for visible TUI scenarios.
