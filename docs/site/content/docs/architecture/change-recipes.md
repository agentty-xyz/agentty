+++
title = "Change Recipes"
description = "Concrete change paths for common contribution scenarios, plus a contributor checklist."
weight = 4
+++

<a id="architecture-change-recipes-introduction"></a> Use these recipes to route changes
through the correct modules without crossing layer boundaries. Module routers and doc
comments identify the exact files.

<!-- more -->

## Add or Modify a Session Workflow

1. Keep frontend-neutral request/result models and programmatic operations in
   `ag-session`.
1. Update Agentty orchestration in `crates/agentty/src/app/session/` and adapt it
   through `crates/agentty/src/app/session_api.rs`.
1. Route background-callable operations through the bounded actor in
   `crates/agentty/src/app/session_runtime.rs`; do not share `App` behind an async
   mutex.
1. Keep persistence in `ag-store` domain modules and Git operations behind `GitClient`.
1. Preserve the session-branch invariant: one evolving commit per session branch,
   created by the first file-changing turn and amended by later ones.
1. Update `docs/site/content/docs/usage/workflow.md` when lifecycle or status behavior
   changes.

## Add an Agent Definition

Add a repository definition under `.agents/agents/<id>/agent.md`, or a bundled
definition under `crates/ag-session/src/agents/`. Both use the same parser. Keep host
workflow contracts separate from definition identity and preserve task snapshots when
changing admission or continuation.

## Define a Generic Execution Graph

Follow the [execution graph contracts](@/docs/architecture/execution-graphs.md). Keep
model-authored proposals in `ag-protocol`, pure graph shapes and resolution in
`ag-session`, durable snapshots, results, and atomic admission in `ag-store`, and
dispatch in the `ag-worker` `GraphExecutor`. Inject worker adapters through
`GraphNodeRunner` and share `GraphResources` Env leases with other host work. Test
structural rules without storage or workers, storage invariants with SQLite, and
dispatch through the public API.

## Add a New Agent Backend or Model

1. Declare model IDs, descriptions, provider order, speed support, and known context
   budgets together in the `ag-session` model catalog. Keep provider defaults and
   retired-ID mappings explicit; removing a public enum variant is a Rust API change.
1. For a new backend, add behavior in `crates/ag-agent/src/agent/` and register it in
   the provider registry. Backends consume catalog capabilities instead of keeping their
   own model lists.
1. Keep transport selection, parsing, streaming, and provider setup in `ag-agent`;
   application workflows continue through worker clients.
1. Update `docs/site/content/docs/agents/backends.md`.

## Add a Built-in `ag-router` Model

1. Add the model's wire capabilities and reasoning mapping in `ag-router`, with local
   HTTP tests for schema format, capabilities, reasoning fields, and error mapping.
1. Expose it through `ag-harness` configuration only if the harness supports its
   capabilities.
1. To offer it in Agentty Harness sessions, add it to the `Harness` entries of the
   `ag-session` model catalog and declare its `ModelProfile` in `ag-agent`.

## Add or Change a Utility Agent Prompt

1. Submit an owned `OneShotRequest` through the worker `RunClient`; do not select a CLI,
   app-server, backend, or protocol-repair helper from application orchestration.
1. Inject `&dyn RunClient` into the smallest workflow helper that needs deterministic
   coverage and test it with `MockRunClient`.
1. Background work captures purpose and ownership with `ag-worker::scoped_client`;
   nested utilities keep parent cancellation and never queue behind the session command
   awaiting them.
1. Keep provider routing, protocol repair, usage aggregation, and runtime cleanup in
   `ag-agent`.

## Add a Keybinding or Mode Interaction

1. For text editing, add the semantic command in `crates/agentty/src/domain/input.rs`
   and map terminal keys once in `crates/agentty/src/runtime/mode/input_key.rs`.
1. Let context-specific input modes intercept only their own actions before falling back
   to the shared mapping.
1. Change prompt composer state in the pure reducer of
   `crates/agentty/src/runtime/mode/prompt.rs` and request app work as an effect, so
   unit tests need no `App`.
1. Update other handlers in `crates/agentty/src/runtime/mode/`, or the cross-mode
   overlay dispatch in `crates/agentty/src/runtime/key_handler.rs`.
1. Extend `crates/agentty/src/presentation/app_mode.rs` for a new mode and
   `crates/agentty/src/presentation/help_action.rs` for help content.
1. Update `docs/site/content/docs/usage/keybindings.md`.

## Add or Change Database Schema

1. Add a new migration in `crates/ag-store/migrations/` (`NNN_description.sql`); never
   modify existing migrations.
1. Keep query changes in the matching `ag-store` domain module.
1. Update user-facing docs affected by status or model behavior changes.

## Add a New UI Page or Component

1. Add the page in `crates/agentty/src/ui/page/` or component in
   `crates/agentty/src/ui/component/`, and wire pages into
   `crates/agentty/src/ui/router.rs`.
1. For a new `AppMode`, extend `crates/agentty/src/presentation/app_mode.rs` and add a
   key handler in `crates/agentty/src/runtime/mode/`.

## Change run execution

1. Update shared lifecycle contracts in `ag-contracts` and provider behavior in
   `ag-agent`.
1. Keep mailboxes, session runtime ownership, scheduling, cancellation, and recovery in
   `ag-worker`; implement product-specific question, Git, forge, and UI policy in the
   host adapter.
1. Construct `ag-agent` adapters only inside `ag-runtime`; applications configure
   workers.
1. Extend headless contract tests and the affected Agentty workflow tests. Changes to
   operation persistence also need `ag-store` adapter tests.

## Adding model-assisted work

Use the
[utility prompt recipe](@/docs/architecture/change-recipes.md#add-or-change-a-utility-agent-prompt)
and the [Execution](@/docs/core-components/execution-components/execution-boundary.md)
contract.

## Contributor Checklist for Architecture-Safe Changes

1. Follow [Module Map](@/docs/architecture/module-map.md#layer-rules) for layer
   ownership and [Testability Boundaries](@/docs/architecture/testability-boundaries.md)
   for external-system injection.
1. Update the relevant user guide for visible changes and the canonical architecture
   page when its contracts change, including the runtime key-types table and the
   boundary table.
1. Reuse cached derived data on render hot paths; measurement and painting must agree.
1. Run the required gates in root `AGENTS.md`.
