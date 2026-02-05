# UI Agent Instructions

When working within `crates/ag-cli/src/ui/`:

- **Modularization**: Always respect the module boundaries. Do not put page-specific rendering logic in `mod.rs`. Use the dedicated files in `pages/` (`sessions_list.rs`, `session_chat.rs`, etc.).
- **Helper Functions**: If you write a helper function that calculates layout or processes text, **IMMEDIATELY** move it to `util.rs` and write a unit test for it. Do not leave complex logic inline within render functions.
- **Component Reuse**: Check the `components/` directory before building a new common widget. All components must implement the `Component` trait.
- **Component Pattern**: When creating a component, define a struct that holds rendering data, implement `Component` trait with `render(&self, f: &mut Frame, area: Rect)` method, and provide a `new()` constructor. Usage pattern: `ComponentName::new(...).render(f, area)`.
- **Symlinks**: Ensure `CLAUDE.md` and `GEMINI.md` are symlinked to this file.
