# UI Module

This directory contains the modular user interface components for the Agentty CLI.

## Architecture

The UI is built using [Ratatui](https://ratatui.rs/) and follows a **separation of
concerns** design pattern, splitting rendering logic by page and component type.

### Design Principles

- **Pages** represent complete UI views (session list, chat view, new session prompt)
- **Components** are reusable widgets shared across pages (status bar, input box)
- **Focused helpers** keep pure geometry, input layout, text processing, and
  domain-aware display formatting in separate modules with extensive unit tests
- All pages implement the `Page` trait
- All components implement the `Component` trait

### Module Structure

- **`../ui.rs`**: Main entry point containing the top-level `render` function that
  dispatches to pages based on `AppMode`. Defines the `Page` and `Component` traits.
- **`page/`**: Each page is a separate module implementing the `Page` trait.
  - `session_list.rs` - Session list view (`AppMode::List`)
  - `session_chat.rs` - Session chat interface (`AppMode::View`, `AppMode::Prompt`)
- **`component/`**: Reusable widgets implementing the `Component` trait.
  - `status_bar.rs` - Top status bar with version and runtime scope hint
  - `chat_input.rs` - Chat input box with cursor positioning
- **`layout.rs`**: Pure area and panel geometry helpers.
- **`input_layout.rs`**: Chat-input wrapping, cursor geometry, dropdown sizing, and
  table-column geometry.
- **`prompt_format.rs`**, **`question_format.rs`**, and **`session_format.rs`**:
  Domain-aware display formatting for prompt, clarification, and session surfaces.
- **`text_util.rs`**, **`diff_util.rs`**, and **`activity_heatmap.rs`**: Focused shared
  presentation helpers.

For maintenance procedures and development guidelines, see `AGENTS.md`.
