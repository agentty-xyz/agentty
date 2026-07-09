# Docs Site Content

Scope: `docs/site/content/docs/` and its child documentation pages.

## Purpose

- Keep user-facing documentation readable in the static docs site on both desktop and
  narrow viewports.
- Preserve stable page structure, headings, and anchors for cross-page links.
- Keep this subtree focused on current Agentty behavior. Avoid using user docs as a
  changelog, implementation dump, or agent-environment setup guide.

## Organization Rules

- Keep overview pages concise and task-oriented. Link to the deeper workflow,
  keybinding, agent, or architecture page instead of repeating the same lifecycle detail
  in multiple places.
- Keep architecture pages at the layer, boundary, ownership, and runtime-flow level. Do
  not maintain path-by-path inventories, file lists, or runtime mode lists in narrative
  docs; point readers to module docstrings for file-level detail.
- Keep user-facing pages focused on the currently supported behavior. Remove obsolete
  setup pages, retired feature notes, and historical compatibility details once they are
  no longer needed to use the current product.
- Use markdown tables only for compact comparison data where most cells stay to a short
  phrase.
- When a table cell needs a sentence, replace the table with stacked sections or short
  titled blocks instead of relying on horizontal scrolling.
- Prefer one subsection per concept with consistent labels such as `Comes from`,
  `Prints`, and `Hidden or removed` when documenting lifecycle-style behavior.
- For backend and model docs, document installed CLI requirements, visible model picker
  behavior, prompt attachment support, and user-facing fallback behavior. Keep provider
  protocol or transport internals out of user-facing pages unless users must understand
  them to operate Agentty.
- For workflow and keybinding docs, split dense lifecycle behavior into short titled
  sections and keep shortcut tables scan-friendly.

## Change Routing

- Update `getting-started/overview.md` only for high-level concepts and first-run
  workflow. Route detailed draft, stacked, publish, question, update, and settings
  behavior to the usage pages.
- Update `agents/backends.md` when CLI availability, model choices, model fallback,
  prompt attachment support, or backend prerequisites change.
- Update `architecture/runtime-flow.md` when render order, session-output sources, or
  status-driven visibility rules change.
- Update `architecture/module-map.md` when workspace crate ownership, application-layer
  boundaries, or canonical module responsibilities change; keep the page layer-level.
- Update `architecture/testability-boundaries.md` when external command, filesystem,
  clock, terminal, provider, forge, or persistence boundaries change.
- Update `usage/workflow.md` and `usage/keybindings.md` when UI behavior or controls
  change.
- When shortcut behavior changes, compare `usage/keybindings.md` and `usage/workflow.md`
  against the runtime handlers and rendered help actions before handoff.
- When forge review-request support changes, keep usage and architecture docs aligned
  with the supported forge families and CLI names.
- When a product surface is removed, remove its docs page and navigation entry instead
  of keeping an obsolete stub.

## Docs Sync Notes

- Keep this directory guide aligned with the docs site's presentation patterns when a
  new formatting convention becomes the default for multiple pages.
- Before handoff for broad docs refreshes, scan edited pages for duplicated lifecycle
  descriptions, long table cells, stale setup instructions, and implementation details
  that should live in architecture docs or source docstrings instead.
