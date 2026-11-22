---
name: enforce-rules
description: Turn a proposed or existing project rule into the cheapest automated check that enforces it, measure and fix current violations, and promote or demote rules between Clippy lints, ast-grep rules, ag-xtask checks, contract tests, and AGENTS.md instructions.
---

# Enforce Rules

Use this skill when someone proposes a convention, a review keeps repeating the same
comment, a fixed bug class could come back, or an `AGENTS.md` line could be checked
mechanically. It decides where a rule lives and how it gets there.

## Levels and Exceptions

`docs/site/content/docs/contributing/rule-enforcement.md` is the contributor-facing
explanation of the levels, the hooks that report each one, and the exception policy.
Read it first; the steps below use its level numbers:

- **Level 1:** rustc and Clippy lints and formatter configuration.
- **Level 2:** ast-grep rules, `ag-xtask` checks, and contract tests.
- **Level 3:** the nearest `AGENTS.md`.

Write an `ag-xtask` task only when the rule needs more than one file's syntax. Put every
exception in the check's configuration as that page describes, never at the call site.

## Placing a Rule

1. Write the rule as a predicate: the paths it covers, the construct it forbids or
   requires, the allowed exceptions, and the fix. If you cannot state the predicate
   without judgement words such as "focused" or "when clearer", keep it at level 3.
1. Pick the lowest level that can evaluate the predicate.
1. Measure current violations before changing anything:
   - Clippy: run
     `cargo clippy --workspace --all-targets --all-features --message-format=short -- -W clippy::<lint>`
     and count the distinct sites. Restriction lints that report thousands of existing
     sites usually belong at level 3.
   - ast-grep: add the draft rule to its directory under `lint/rules/` (see below) and
     run `prek run ast-grep --all-files`; it lists every match.
   - `ag-xtask` or contract test: implement the check first and let it list violations.
1. Decide what to do with the existing violations:
   - A handful: fix them in the same change.
   - Many but mechanical: fix them in the same change, or in a preceding one when the
     fix touches unrelated code.
   - Many that each need judgement: narrow the scope until the remaining sites are
     fixable, or keep the rule at level 3.
1. Implement the check (see below) and make its failure message name the fix.
1. Delete the `AGENTS.md` text the check now enforces, or shrink it to the judgement
   that remains. The root guide forbids restating enforced rules.
1. Update `docs/site/content/docs/contributing/rule-enforcement.md` when the change adds
   a level, a reporting hook, or an exception mechanism, or changes the exception
   policy. A new rule at an existing level needs no page change.
1. Validate through `skills/validate-changes/SKILL.md`, adding the check's own hooks.

## Writing ast-grep Rules

- Name the file after the rule `id`, in kebab-case, and prefix rules scoped to one crate
  with that crate, such as `agentty-layer-host-access`.
- Put a rule in `lint/rules/shared/` when it names no crate or path of this workspace
  and its `files` and `ignores` globs start with a pattern such as `**/src/**/*.rs`;
  `standards.toml` exports these rules to downstream workspaces, and `check-standards`
  rejects shared rules that break either condition. Put every other rule in
  `lint/rules/workspace/`.
- Use `severity: error`. Write `message` as one sentence saying what to do, and `note`
  as the fix plus the owning document.
- Scope with `files` and `ignores`, relative to the repository root. Production-only
  rules ignore `"**/*_test.rs"` and `"**/*_test/**"`.
- Match tree-sitter node kinds (`kind`, `has`, `inside`, `follows`) and use `regex` only
  on the matched node's text. Inspect node kinds with ast-grep's
  `run --debug-query=cst -l rust -p '<snippet>'` mode, using the `ast-grep-cli` version
  that `.pre-commit-config.yaml` pins.
- Add `<id>-test.yml` to the matching `lint/rule-tests/shared/` or
  `lint/rule-tests/workspace/` directory with at least one `valid` and one `invalid`
  case covering each branch of the rule; `prek run ast-grep-test --all-files` runs them.
  Rule tests ignore `files` and `ignores`, so keep path scoping simple.

## Writing ag-xtask Checks

Follow `crates/ag-xtask/AGENTS.md`: one module per check, a mockable boundary for
filesystem, process, and Git access, 100% coverage, and registration in
`.pre-commit-config.yaml` with a `files` filter that fires when any input changes.
Extend an existing check when the rule shares its inputs, as dependency rules do with
`check-execution-boundary`.

## Moving a Rule Later

- **Up to level 1:** when Clippy or rustc gains a lint that covers an ast-grep rule or
  `ag-xtask` check, enable the lint and delete the custom rule in the same change.
- **Within level 2:** move an ast-grep rule to `ag-xtask` when it starts needing facts
  outside one file, such as whether a sibling directory exists.
- **Down to level 3:** when a rule keeps needing exceptions that its configuration
  cannot express, delete it and restore a short instruction stating the judgement.
  Record the reason in the change description.

## Handoff

Report the rule, its level and home, the violation count before and after, the
instructions removed, any update to the rule-enforcement page, and the hooks run.
