+++
title = "Rule Enforcement"
description = "How Agentty enforces project rules with Clippy, ast-grep, ag-xtask checks, contract tests, and AGENTS.md guidance, and how to add exceptions."
weight = 2
+++

<a id="rule-enforcement-introduction"></a> Agentty enforces each project rule with the
cheapest automated check that can decide it. Instructions in `AGENTS.md` cover only the
judgement that no tool can make.

<!-- more -->

## Enforcement Levels

<a id="rule-enforcement-levels"></a> Each rule lives at the lowest level that can decide
it. Lower levels run earlier, cost less to maintain, and explain themselves through
their own failure messages.

| Level | Home                                                                | Decides                                                     |
| ----- | ------------------------------------------------------------------- | ----------------------------------------------------------- |
| 1     | `[workspace.lints]` in `Cargo.toml`, `clippy.toml`, `.rustfmt.toml` | Anything a rustc or Clippy lint, or a formatter, covers     |
| 2     | ast-grep rules in `lint/rules/`                                     | One file's syntax: banned calls, imports, attributes        |
| 2     | `ag-xtask` `check-*` tasks                                          | Cross-file facts: manifests, the dependency graph, Git      |
| 2     | Contract tests in the owning crate                                  | Facts only running code can see, such as SQLite query plans |
| 3     | The nearest `AGENTS.md`                                             | Judgement no tool can decide                                |

Level 1 is preferred over level 2, and an ast-grep rule over a new `ag-xtask` task. A
rule moves to a lower level when a tool gains the ability to check it, and back to
`AGENTS.md` when its exceptions stop fitting the check's configuration.

## Where Failures Appear

<a id="rule-enforcement-failures"></a> Every level reports through a `prek` hook, so
local runs and CI fail the same way:

- **Clippy and rustfmt:** the `clippy` and `rustfmt` hooks.
- **ast-grep:** the `ast-grep` hook checks the rules, and `ast-grep-test` checks their
  examples under `lint/rule-tests/`.
- **`ag-xtask` checks:** a dedicated hook per check, such as `check-manifests`,
  `check-execution-boundary`, or `check-instructions`.
- **Contract tests:** the owning crate's ordinary test suite.

Each failure message names the fix. Read it before looking anywhere else.

## Exceptions

<a id="rule-enforcement-exceptions"></a> An exception lives in the check's
configuration, with a reason, and never at the call site. Clippy rejects `#[allow]`, and
ast-grep rules reject `#[expect(clippy::...)]` and `ast-grep-ignore` comments.

- **Clippy:** a lint threshold in `clippy.toml`, or the lint stays off.
- **ast-grep:** a narrower `files` or `ignores` scope, explained in the rule's `note`.
- **`ag-xtask` and contract tests:** a named allowlist constant with one reason per
  entry.
- **SonarQube:** a rule exception in `sonar-project.properties`, with a comment
  explaining why the rule does not apply.

`#[expect(<rustc lint>, reason = "...")]` stays available for rustc lints such as
`unsafe_code`, where the exception is a reviewed property of one site.

## Proposing a Rule

<a id="rule-enforcement-proposing"></a> Propose a rule when a review keeps repeating the
same comment, a fixed bug class could come back, or an `AGENTS.md` line could be checked
mechanically. State it as a predicate: the paths it covers, what it forbids or requires,
the allowed exceptions, and the fix. A rule that needs words such as "focused" or "when
clearer" stays in `AGENTS.md`.

The `enforce-rules` skill in `skills/enforce-rules/SKILL.md` walks through choosing the
level, measuring existing violations, writing the check, and removing the instruction it
replaces.
