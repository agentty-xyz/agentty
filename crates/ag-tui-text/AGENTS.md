# ag-tui-text

Shared Ratatui rendering for Markdown, HTML, Mermaid diagrams, and terminal-width text.

## Boundaries

- Keep the crate independent of Agentty application layers.
- Require hosts to inject semantic palette and cache-version settings; do not read
  application theme globals.
- Keep parsing, caches, and layout limits bounded for untrusted transcript content.
- Put reusable terminal text behavior here rather than duplicating it in host UIs.
