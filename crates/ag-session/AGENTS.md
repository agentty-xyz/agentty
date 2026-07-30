# Session API Crate

Owns the frontend-neutral programmatic API and shared models for Agentty sessions.

## Boundaries

- `service.rs` defines the public orchestration facade and the backend port implemented
  by host applications.
- `model.rs` owns stable session identity, lifecycle state, settings, and aggregate
  snapshots.
- `message.rs` owns durable transcript message models and formatting.
- Keep TUI state, SQLite rows, Git worktree mechanics, agent workers, and forge clients
  out of this crate. Host adapters translate those implementation details through
  `SessionBackend`.
- `SessionService` is an owned, cloneable capability over `Arc<dyn SessionBackend>`;
  backends are `Send + Sync` and accept shared `&self` access so background coordinators
  do not borrow a frontend.
- Extend the API with explicit request or result types instead of exposing host-specific
  managers.

## Tests

- Exercise each public facade operation through a fake backend.
- Keep model and transcript behavior covered with local unit tests using explicit
  `Arrange`, `Act`, and `Assert` sections.
