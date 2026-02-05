# UI Module

This directory contains the modular user interface components for the Agentty CLI.

## Code Design & Architecture

The UI is split into separate modules to improve readability and maintainability. The core philosophy is **separation of concerns** by page and component type.

### Module Structure

- **`mod.rs`**: The main entry point. It contains the top-level `render` function which dispatches rendering to the appropriate sub-module based on the current `AppMode`. It also defines the `Page` trait.
- **`components.rs`**: Contains reusable UI widgets shared across different pages, such as the `render_status_bar` and `render_chat_input`.
- **`pages/`**: Directory containing the rendering logic for each distinct UI page.
    - **`sessions_list.rs`**: `SessionsListPage` - renders the session list (`AppMode::List`).
    - **`session_chat.rs`**: `SessionChatPage` - renders the session detail view (`AppMode::View`) and reply interface (`AppMode::Reply`).
    - **`new_session.rs`**: `NewSessionPage` - renders the initial prompt input (`AppMode::Prompt`).
- **`util.rs`**: Contains pure helper functions for layout calculations, text wrapping, and input handling. **All logic that can be unit-tested without a full TUI environment should go here.**

## Maintenance Guidelines

1.  **Adding a New Page**:
    - Create a new module in `pages/` (e.g., `pages/my_page.rs`).
    - Define a struct (e.g., `MyPage`) that holds necessary references.
    - Implement the `Page` trait for your struct.
    - Expose the module in `pages/mod.rs`.
    - Update the match expression in `mod.rs` to instantiate and render your page.

2.  **Modifying Layouts**:
    - Use `util.rs` for complex layout logic (like splitting areas or calculating heights).
    - Ensure extensive unit tests in `util.rs` for any layout calculations.

3.  **Components**:
    - If a UI element is used in more than one place, extract it into `components.rs`.
    - Keep components stateless where possible; pass all necessary data as arguments.

4.  **Testing**:
    - **Unit Tests**: Focus on `util.rs`. Test layout logic, string manipulation, and input height calculations heavily here.
    - **Integration**: Verifying the actual `render` output is harder. Rely on visual verification for broad changes, but ensure the underlying logic in `util` is rock-solid.
