# Domain Layer

## Overview

Pure business logic and domain entities, decoupled from UI and infrastructure.

## Key Files

- `agent.rs` defines provider kinds, models, and model metadata.
- `operation.rs` defines durable operation kinds, lifecycle states, and transition
  rules.
- `session.rs` defines Agentty render/runtime session entities and sizing logic while
  re-exporting shared identity, status, and review-link models from `ag-session`.
- `session_message.rs` re-exports durable transcript models from `ag-session`.
- `project.rs`, `setting.rs`, `permission.rs`, and `input.rs` define shared application
  concepts.

## Docs

Changes to agent kinds, models, or session status/sizes require updating:

- `docs/site/content/docs/agents/backends.md` — agent backends and models.
- `docs/site/content/docs/usage/workflow.md` — session lifecycle and sizes.
