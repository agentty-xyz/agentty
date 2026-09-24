+++
title = "Change Recipes"
description = "Concrete change paths for common contribution scenarios, plus a contributor checklist."
weight = 4
+++

<a id="architecture-change-recipes-introduction"></a> Use these recipes to route changes
through the correct modules without crossing layer boundaries.

<!-- more -->

## Add or Modify a Session Workflow

1. Keep frontend-neutral request/result models and programmatic operations in
   `crates/ag-session/`.
1. Update Agentty orchestration in `crates/agentty/src/app/session/` and adapt it
   through `crates/agentty/src/app/session_api.rs`.
1. Route background-callable operations through the bounded actor in
   `crates/agentty/src/app/session_runtime.rs`; do not share `App` behind an async
   mutex.
1. Keep persistence in `crates/ag-store/src/` domain modules. Use `repository.rs` for
   repository bundle composition and `connection.rs` for pool wiring; keep Agentty's
   `crates/agentty/src/infra/db.rs` limited to database-location and clock composition.
1. Keep git operations behind `GitClient` in `crates/ag-git/src/client.rs`.
1. Preserve the session-branch invariant: one evolving commit per session branch, with
   the first file-changing turn creating it and later file-changing turns updating it by
   amending `HEAD`.
1. Update docs when lifecycle/status behavior changes:
   `docs/site/content/docs/usage/workflow.md`.

## Changing Campaign Orchestration

1. Keep shared task and lifecycle models in `ag-session`, campaign policy and prompts in
   `ag-orchestration`, and SQL in `ag-store`.
1. Route session mutations through `SessionService`. Emit campaign notifications through
   `OrchestrationEventSink`; Agentty owns translation to reducer events and injects the
   reconciliation schedule.
1. Run `test-ag-orchestration-src` and affected session/store/application checks through
   `prek`. Preserve the campaign E2E coverage in `crates/agentty/tests/e2e/`.

## Add a New Agent Backend or Model

1. Update provider model declarations in `crates/ag-session/src/agent.rs`.
1. Add backend behavior in `crates/ag-agent/src/agent/` and register it in
   `crates/ag-agent/src/agent/provider.rs`.
1. Keep transport selection, parsing, streaming, and provider setup in the provider
   registry; application workflows continue through worker clients.
1. Update `docs/site/content/docs/agents/backends.md` with backend/model documentation.

## Add or Change a Utility Agent Prompt

1. Submit an owned `OneShotRequest` through the worker `RunClient`; do not select a CLI,
   app-server, backend, or protocol-repair helper from application orchestration.
1. Inject `&dyn RunClient` into the smallest workflow helper that needs deterministic
   coverage and test it with `MockRunClient`.
1. Keep provider routing, protocol repair, usage aggregation, and runtime cleanup in
   `crates/ag-agent/src/agent/submission.rs`.

## Add a Keybinding or Mode Interaction

1. For basic text editing, add or update the semantic command in
   `crates/agentty/src/domain/input.rs`, then map terminal keys once in
   `crates/agentty/src/runtime/mode/input_key.rs`.
1. Let prompt, question, branch-publish, and settings input modes intercept only their
   context-specific actions before falling back to the shared input command mapping.
1. For other interactions, update the handler in `crates/agentty/src/runtime/mode/`, or
   in `crates/agentty/src/runtime/key_handler.rs` when the interaction is a cross-mode
   overlay dispatch.
1. If a new mode/state is needed, extend `crates/agentty/src/presentation/app_mode.rs`.
1. If help content changes, update `crates/agentty/src/presentation/help_action.rs` as
   needed.
1. Update `docs/site/content/docs/usage/keybindings.md`.

## Add or Change Database Schema

1. Add a new migration file in `crates/ag-store/migrations/` (`NNN_description.sql`).
1. Never modify existing migration files.
1. Keep query changes in the matching `crates/ag-store/src/*.rs` domain module instead
   of expanding Agentty's composition facade.
1. Ensure any status/model behavior changes are reflected in docs pages affected by
   user-facing behavior.

## Add a New UI Page or Component

1. Add the page in `crates/agentty/src/ui/page/` or component in
   `crates/agentty/src/ui/component/`.
1. Wire the page into `crates/agentty/src/ui/router.rs`.
1. If a new `AppMode` is needed, extend the shared presentation contract implemented in
   `crates/agentty/src/presentation/app_mode.rs` and exported through
   `crates/agentty/src/presentation.rs`, then add a key handler in
   `crates/agentty/src/runtime/mode/`.

## Contributor Checklist for Architecture-Safe Changes

1. Follow [Module Map](@/docs/architecture/module-map.md#layer-rules) for layer
   ownership and [Testability Boundaries](@/docs/architecture/testability-boundaries.md)
   for external-system injection.
1. Update the relevant user guide for visible changes and the canonical architecture
   page when its contracts change. Keep durable scope instructions aligned.
1. Reuse cached derived data on render hot paths; measurement and painting must agree.
1. Keep the runtime key-types table current when execution contracts change, and the
   boundary reference current when external traits change.
1. Run the required gates in root `AGENTS.md`.

## Change run execution

1. Update shared lifecycle contracts in `ag-contracts` and provider behavior in
   `ag-agent`.
1. Keep mailboxes, session runtime ownership, scheduling, cancellation, and recovery in
   `ag-worker`; implement product-specific question, Git, forge, and UI policy in the
   host adapter. Submit session turns through `SessionRunClient` and utilities through
   `RunClient`.
1. Import execution contracts from `ag-contracts` and selections from `ag-session`.
   Construct `ag-agent` adapters only inside `ag-runtime`; applications configure
   workers.
1. Extend headless contract tests and the affected Agentty workflow tests. Changes to
   operation persistence also need `ag-store` adapter tests.

## Adding model-assisted work

Use the
[utility prompt recipe](@/docs/architecture/change-recipes.md#add-or-change-a-utility-agent-prompt).
Background work must capture purpose and session/project ownership with
`ag-worker::scoped_client`. Nested utilities retain parent cancellation and run directly
under worker supervision, without enqueueing behind the session command awaiting them.

See [Execution](@/docs/core-components/execution.md) for the execution contract.
