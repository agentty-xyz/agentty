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

## Reusable Standards

<a id="rule-enforcement-reusable-standards"></a> `standards.toml` at the repository root
names the part of these rules that holds in any Cargo workspace, so downstream
workspaces that follow the same standards can vendor it at a pinned commit instead of
copying it by hand. It exports the formatter and Clippy configuration, the pinned
toolchain, the `[workspace.lints]` tables, the ast-grep rules in `lint/rules/shared/`
with their tests, whole skills, and `AGENTS.md` sections named by heading. Rules that
name this workspace's crates or paths stay in `lint/rules/workspace/`.

Its `[design]` table lets another workspace's pages share the same look. It has exactly
four keys:

- `guide`: the design guide, `design/DESIGN.md`.
- `files`: the stylesheets a web page copies beside it, in load order:
  `design/tokens.css`, `design/fonts.css`, then `design/base.css`.
- `tokens`: the machine-readable token source, `design/tokens.json`, in the W3C Design
  Tokens format, for consumers that need a value outside CSS.
- `assets`: other files copied byte for byte, text or binary, such as the brand mark,
  the favicon, and the monospace font with its licence, which the token source names,
  and `design/gallery.html` and `design/gallery-check.mjs`, the component gallery and
  the script that compares a page's stylesheets with it.

Its `[protocol]` table lets another workspace's remote-control clients speak the same
frames. It has exactly two keys, both files generated from the protocol types, as the
[remote-control design](@/docs/architecture/remote-control-design.md#frame-schema)
describes:

- `schema`: the JSON Schema of every frame,
  `crates/ag-remote/schema/remote_protocol.json`.
- `types`: the TypeScript declarations rendered from it,
  `crates/ag-remote/schema/remote_protocol.ts`.

The manifest's `version` is 4 since `[protocol]` was added; a reader of an older version
rejects it, and every table rejects keys it does not know.

Exported means portable. An exported section, skill, rule, guide, stylesheet, token
file, or text asset names no crate, hook, or repository path that only this workspace
has, unless the manifest exports that path too; a crate name that follows another label
and a dot, as in a reverse-domain `$extensions` namespace such as `org.example.theme`,
is a different name. A shared rule selects files by pattern, such as `**/src/**/*.rs`,
rather than by one of this workspace's directories. A design export loads nothing from
the network or from outside the export:

- A stylesheet is plain CSS with no Sass or `@import`, and its `url()` targets are
  `data:` URIs, fragments, or exported files in its own directory.
- The token file is JSON whose root is a token group.
- An HTML asset loads, through `src`, `srcset`, `href`, `poster`, `data`, `ping`,
  `url()`, `@import`, and the `import` and `fetch` calls of its scripts and event
  handlers, only `data:` URIs, fragments, and exported files in its own directory. A
  link to a web, mail, or plain relative address is not a load; any other link
  destination, such as a `javascript:` URL, is.
- An SVG asset holds no script or event handler and references only its own fragments
  and `data:` URIs.
- Every asset is at most 256 KiB, and a stylesheet belongs in `files`, not `assets`.

The protocol files describe this workspace's own protocol, so they may name the crate
they are generated from, but each stands alone:

- Each is a regular file that Git lists, tracked or untracked but not ignored, so it is
  neither a link nor a file reached through a linked directory. It is UTF-8 text with no
  byte order mark and at most 512 KiB.
- The schema is a JSON object whose string `$ref`, `$dynamicRef`, and `$recursiveRef`
  values are JSON pointer fragments, such as `#/$defs/ClientFrame`, that resolve inside
  it. It holds no `$id`, `$anchor`, `$dynamicAnchor`, or `$recursiveAnchor` key
  anywhere, and its only `$schema` is `https://json-schema.org/draft/2020-12/schema`, at
  the root.
- The types import nothing: outside comments and string literals they hold no `import`
  or `export ... from` statement, `import()` type, `require` call, or triple-slash
  directive such as `/// <reference>`. They hold no template literal, and every string
  literal ends on its own line.
- The schema's integer `x-protocol-version` equals the one
  `export type ProtocolVersion = <integer>;` the types declare, written without a sign
  or leading zero, so the pair cannot be exported out of step.

The `check-standards` hook enforces this and checks that every path, skill, and heading
the manifest names exists and that no design export is listed twice.
