# UI Module

This directory contains the modular user interface components for the Agentty CLI.

## Code Design & Architecture

The UI is split into separate modules to improve readability and maintainability. The core philosophy is **separation of concerns** by page and component type.

### Module Structure

- **`mod.rs`**: The main entry point. It contains the top-level `render` function which dispatches rendering to the appropriate sub-module based on the current `AppMode`. It also defines the `Page` and `Component` traits.
- **`components/`**: Directory containing reusable UI widgets shared across different pages. All components implement the `Component` trait.
    - **`status_bar.rs`**: `StatusBar` - renders the top status bar showing app version and agent type.
    - **`chat_input.rs`**: `ChatInput` - renders the chat input box with cursor positioning.
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
    - If a UI element is used in more than one place, create a new module in `components/`.
    - Define a struct that holds necessary rendering data.
    - Implement the `Component` trait with a `render(&self, f: &mut Frame, area: Rect)` method.
    - Add a `new()` constructor to initialize the struct.
    - Usage pattern: `ComponentName::new(...).render(f, area)`

4.  **Testing**:
    - **Unit Tests**: Focus on `util.rs`. Test layout logic, string manipulation, and input height calculations heavily here.
    - **Integration**: Verifying the actual `render` output is harder. Rely on visual verification for broad changes, but ensure the underlying logic in `util` is rock-solid.
