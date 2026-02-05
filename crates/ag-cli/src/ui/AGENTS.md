# UI Agent Instructions

When working within `crates/ag-cli/src/ui/`:

- **Modularization**: Always respect the module boundaries. Do not put screen-specific rendering logic in `mod.rs`. Use the dedicated files (`list.rs`, `view.rs`, etc.).
- **Helper Functions**: If you write a helper function that calculates layout or processes text, **IMMEDIATELY** move it to `util.rs` and write a unit test for it. Do not leave complex logic inline within render functions.
- **Component Reuse**: Check `components.rs` before building a new common widget.
- **Symlinks**: Ensure `CLAUDE.md` and `GEMINI.md` are symlinked to this file.
