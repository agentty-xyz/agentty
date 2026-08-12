# Domain Layer

## Overview

Pure business logic and domain entities, decoupled from UI and infrastructure.

## Key Files

- `agent.rs` defines provider kinds, models, and model metadata.
- `session.rs` defines Agentty render/runtime session entities and sizing logic while
  re-exporting shared identity, status, and review-link models from `ag-session`.
- `orchestration.rs`, `personality.rs`, `project.rs`, `review.rs`, `setting.rs`, and
  `transcript_notice.rs` preserve Agentty's compatibility paths for models owned by
  `ag-session`.
- `question.rs` owns app-local input progress while re-exporting shared clarification
  models and option-selection policy from `ag-session`.
- `session_message.rs` re-exports durable transcript models from `ag-session`.
- `permission.rs` and `input.rs` define shared application concepts.

## Docs

Changes to agent kinds, models, or session status/sizes require updating:

- `docs/site/content/docs/agents/backends.md` — agent backends and models.
- `docs/site/content/docs/usage/workflow.md` — session lifecycle and sizes.
