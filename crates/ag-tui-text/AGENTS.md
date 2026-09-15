# ag-tui-text

Shared Ratatui rendering for Markdown, HTML, Mermaid diagrams, and terminal-width text.

## Boundaries

- Keep the crate independent of Agentty application layers.
- Require hosts to inject semantic palette and cache-version settings; do not read
  application theme globals.
- Keep parsing, caches, and layout limits bounded for untrusted transcript content.
- Put reusable terminal text behavior here rather than duplicating it in host UIs.

## Integration

- Pass `TextRenderSettings` at the host render boundary and change its cache version
  when non-content styling changes. Standalone default settings are available for
  consumers without an application theme.
- Consult `crates/ag-tui-text/src/markdown.rs` for cache/render contracts and
  `crates/ag-tui-text/src/style.rs` for semantic settings.

## Documentation

Keep `docs/site/content/docs/usage/workflow.md` aligned with supported transcript markup
and rendering limits; update `docs/site/content/docs/architecture/module-map.md` when
rendering ownership changes.
