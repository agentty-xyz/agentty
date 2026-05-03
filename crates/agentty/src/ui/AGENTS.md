# UI Agent Instructions

When working within `crates/agentty/src/ui/`:

## Core Rules

- **Modularization**: Always respect the module boundaries. Do not put page-specific
  rendering logic in the UI module root file. Use the dedicated files in `page/`
  (`session_list.rs`, `session_chat.rs`, etc.).
- **Helper Functions**: Keep render functions focused. Move reusable or complex layout
  and text-processing helpers to the narrowest shared module that fits the behavior (for
  example, `layout.rs`, `input_layout.rs`, `prompt_format.rs`, `question_format.rs`,
  `session_format.rs`, or `text_util.rs`) and cover that helper with a unit test when
  practical.
- **Component Reuse**: Check the `component/` directory before building a new common
  widget. All components must implement the `Component` trait.
- **Palette Usage**: Use semantic color tokens from `style.rs` (`palette::*`) for UI
  colors. Avoid direct `Color::*` usage in UI components/pages, except approved
  data-visualization scales (for example heatmap intensity colors).
- **Symlinks**: Ensure `CLAUDE.md` and `GEMINI.md` are symlinked to this file.

## Procedures

### Adding a New Page

1. Create a new module in `page/` (e.g., `page/my_page.rs`)
1. Define a struct (e.g., `MyPage`) that holds necessary references
1. Implement the `Page` trait for your struct with a
   `render(&mut self, frame: &mut Frame, area: Rect)` method
1. Expose the module in `page.rs`
1. Update the match expression in the UI module root file to instantiate and render your
   page

### Adding a New Component

1. Create a new module in `component/` (e.g., `component/my_widget.rs`)
1. Define a struct that holds the rendering data needed
1. Implement the `Component` trait with a `render(&self, frame: &mut Frame, area: Rect)`
   method
1. Add a `new()` constructor only when initialization logic or meaningful defaults make
   direct struct construction unclear
1. Expose the module in `component.rs`
1. Usage pattern: construct the widget and call `render(frame, area)`

### Modifying Layouts

- Use the narrowest shared helper module for complex layout logic (like splitting areas
  or calculating heights)
- Ensure unit tests cover any nontrivial layout calculations
- Do not leave layout calculations inline within render functions

### Testing Requirements

- **Unit Tests**: Test layout logic, string manipulation, and input height calculations
  in the module that owns the helper.
- **Integration**: Verifying actual `render` output is difficult. Rely on visual
  verification for broad changes, but ensure the underlying shared logic has focused
  tests.

## Entry Points

- `render.rs` and `router.rs` own frame composition and page dispatch.
- `page.rs` and `page/` own full-screen pages.
- `component.rs` and `component/` own reusable widgets and overlays.
- `state.rs` and `state/` own UI mode and prompt state.
- `layout.rs` owns pure area and panel geometry.
- `input_layout.rs` owns chat-input wrapping, cursor geometry, dropdown sizing, and
  table-column geometry.
- `prompt_format.rs`, `question_format.rs`, and `session_format.rs` own prompt,
  clarification-question, and session display formatting.
- `style.rs`, `markdown.rs`, `text_util.rs`, `diff_util.rs`, and `activity_heatmap.rs`
  own focused shared presentation helpers.
