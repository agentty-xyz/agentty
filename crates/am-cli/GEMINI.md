# Agent Manager CLI

Modular TUI application for managing agents.

## Project Structure

- `src/main.rs`: Application entry point, terminal lifecycle management, and main event loop.
- `src/app.rs`: `App` struct holding the application state (`agents`, `table_state`, `mode`) and business logic.
- `src/model.rs`: Core domain models (`Agent`, `Status`, `AppMode`).
- `src/ui.rs`: Rendering logic using `ratatui`.
- `Cargo.toml`: Crate dependencies and metadata.
