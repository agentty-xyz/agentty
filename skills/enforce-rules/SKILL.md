---
name: enforce-rules
description: Turn a proposed or existing project rule into the cheapest automated check that enforces it, measure and fix current violations, and promote or demote rules between Clippy lints, ast-grep rules, ag-xtask checks, contract tests, and AGENTS.md instructions.
---

# Enforce Rules

Use this skill when someone proposes a convention, a review keeps repeating the same
comment, a fixed bug class could come back, or an `AGENTS.md` line could be checked
mechanically. It decides where a rule lives and how it gets there.

## Levels

Place each rule at the lowest level that can decide it. Lower levels run earlier, cost
less to maintain, and explain themselves through their own failure messages.

| Level | Home                                                                | Decides                                                               |
| ----- | ------------------------------------------------------------------- | --------------------------------------------------------------------- |
| 1     | `[workspace.lints]` in `Cargo.toml`, `clippy.toml`, `.rustfmt.toml` | Anything an existing rustc or Clippy lint, or a formatter, covers     |
| 2     | `lint/rules/` (ast-grep)                                            | One file's syntax, scoped by path: banned calls, imports, attributes  |
| 2     | `ag-xtask` `check-*` tasks                                          | Cross-file facts: manifests, the dependency graph, Git history        |
| 2     | Contract tests in the owning crate                                  | Facts only running code can see, such as SQLite schema or query plans |
| 3     | The nearest `AGENTS.md`                                             | Judgement no tool can decide                                          |

Prefer level 1 over level 2, and ast-grep over a new `ag-xtask` task. Write an
`ag-xtask` task only when the rule needs more than one file's syntax.

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
   - ast-grep: add the draft rule under `lint/rules/` and run
     `prek run ast-grep --all-files`; it lists every match.
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
1. Validate through `docs/contributing/validation.md`, adding the check's own hooks.

## Exceptions

Exceptions live in the check's configuration with a reason, never at the call site.
Clippy rejects `#[allow]`, and ast-grep rules reject `#[expect(clippy::...)]` and
`ast-grep-ignore` comments.

- Clippy: set a lint threshold in `clippy.toml`, or keep the lint off.
- ast-grep: narrow `files` or `ignores` in the rule and explain the exception in `note`.
- `ag-xtask` and contract tests: a named allowlist constant with one reason per entry.

`#[expect(<rustc lint>, reason = "...")]` stays available for rustc lints such as
`unsafe_code`, where the exception is a reviewed property of one site.

## Writing ast-grep Rules

- Name the file after the rule `id`, in kebab-case, and prefix rules scoped to one crate
  with that crate, such as `agentty-layer-host-access`.
- Use `severity: error`. Write `message` as one sentence saying what to do, and `note`
  as the fix plus the owning document.
- Scope with `files` and `ignores`, relative to the repository root. Production-only
  rules ignore `"**/*_test.rs"` and `"**/*_test/**"`.
- Match tree-sitter node kinds (`kind`, `has`, `inside`, `follows`) and use `regex` only
  on the matched node's text. Inspect node kinds with ast-grep's
  `run --debug-query=cst -l rust -p '<snippet>'` mode, using the `ast-grep-cli` version
  that `.pre-commit-config.yaml` pins.
- Add `lint/rule-tests/<id>-test.yml` with at least one `valid` and one `invalid` case
  covering each branch of the rule; `prek run ast-grep-test --all-files` runs them. Rule
  tests ignore `files` and `ignores`, so keep path scoping simple.

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
instructions removed, and the hooks run.
