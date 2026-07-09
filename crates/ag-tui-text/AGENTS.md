# AG-TUI-Text

Shared Ratatui text rendering crate for markdown, mermaid diagrams, and terminal-width
text helpers.

## Entry Points

- `src/lib.rs` is the public crate root.
- `src/markdown.rs` owns markdown parsing, terminal styling, and rendered-line caching.
- `src/mermaid.rs` owns bounded mermaid source parsing and terminal diagram drawing.
- `src/text_util.rs` owns terminal-width wrapping, truncation, and compact formatting
  helpers.
- `src/style.rs` owns `TextPalette` and `TextRenderSettings` injection for host
  applications.

## Change Guidance

- Keep this crate independent from `agentty` app, domain, infra, and runtime modules.
- Host applications must inject semantic palette and cache-version settings at render
  boundaries instead of this crate reading application theme globals directly.
- Keep parser and layout limits bounded so untrusted transcript content cannot make
  rendering unbounded.
