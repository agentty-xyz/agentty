# UI Module

This directory contains the modular user interface components for the Agentty CLI.

## Code Design & Architecture

The UI is split into separate modules to improve readability and maintainability. The core philosophy is **separation of concerns** by screen and component type.

### Module Structure

- **`mod.rs`**: The main entry point. It contains the top-level `render` function which dispatches rendering to the appropriate sub-module based on the current `AppMode`.
- **`components.rs`**: Contains reusable UI widgets shared across different screens, such as the `render_status_bar` and `render_chat_input`.
- **`list.rs`**: Responsible for rendering the session list screen (`AppMode::List`).
- **`view.rs`**: Responsible for rendering the session detail view (`AppMode::View`) and the reply interface (`AppMode::Reply`).
- **`prompt.rs`**: Responsible for rendering the initial prompt input screen (`AppMode::Prompt`).
- **`util.rs`**: Contains pure helper functions for layout calculations, text wrapping, and input handling. **All logic that can be unit-tested without a full TUI environment should go here.**

## Maintenance Guidelines

1.  **Adding a New Screen**:
    - Create a new module (e.g., `my_screen.rs`).
    - Implement a public `render` function.
    - Expose the module in `mod.rs`.
    - Update the match expression in `mod.rs` to call your new render function for the corresponding `AppMode`.

2.  **Modifying Layouts**:
    - Use `util.rs` for complex layout logic (like splitting areas or calculating heights).
    - Ensure extensive unit tests in `util.rs` for any layout calculations.

3.  **Components**:
    - If a UI element is used in more than one place, extract it into `components.rs`.
    - Keep components stateless where possible; pass all necessary data as arguments.

4.  **Testing**:
    - **Unit Tests**: Focus on `util.rs`. Test layout logic, string manipulation, and input height calculations heavily here.
    - **Integration**: Verifying the actual `render` output is harder. Rely on visual verification for broad changes, but ensure the underlying logic in `util` is rock-solid.
