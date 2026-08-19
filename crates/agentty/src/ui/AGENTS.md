# UI Layer

Ratatui pages, components, layout, and formatting.

## Boundaries

- Put page-specific rendering in `page/`. Put reusable widgets in `component/`, and
  implement `Component` for shared widgets.
- Keep render functions focused. Move reusable or nontrivial layout and formatting into
  the narrowest pure helper module; put host-independent terminal text in `ag-tui-text`.
- Use semantic palette tokens from `style.rs`; direct `Color` values are reserved for
  approved data-visualization scales.
- Keep workflow decisions and external I/O out of UI code.

## Render Performance

- Do not clone large strings or collections in per-frame paths merely to satisfy borrow
  scopes.
- Keep derived-data caches bounded and document their key and invalidation trigger.
- When measurement and painting need the same expensive representation, share one cached
  snapshot.

Test pure layout and formatting behavior in its owning module. For visible behavior,
follow the root feature-test gate and keep workflow/keybinding docs synchronized.
