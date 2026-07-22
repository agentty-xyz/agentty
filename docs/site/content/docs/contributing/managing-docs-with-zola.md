+++
title = "Managing Docs with Zola"
description = "Recommended structure and front-matter conventions for maintaining docs in Zola."
weight = 1
+++

<a id="managing-docs-introduction"></a> Use these conventions to keep Agentty
documentation maintainable as it grows.

<!-- more -->

## Keep URLs Stable

- Keep documentation under the `content/docs/` section.
- Keep the section directory named `docs` so its canonical route remains `/docs/`.
- When moving or renaming pages, add `aliases` in page front matter to preserve old
  links.
- For paragraph-level deep links, add explicit HTML anchors in content (for example,
  `<a id="some-paragraph-id"></a>` before the paragraph).
- Paragraph anchors automatically render a `#` affordance next to the paragraph so users
  can copy deep links directly.

## Use Section Metadata Deliberately

- Set `sort_by = "weight"` and define page `weight` values for intentional ordering.
- Keep `page_template` on the docs section so all guides share a consistent layout.
- Keep `build_search_index = true`, use the `fuse_json` format in the `[search]`
  configuration, and keep documentation pages in the index.
- The shared navigation searches documentation, feature pages, and blog posts. The
  documentation sidebar uses the same generated index and filters its results to
  `/docs/` routes.

## Preserve the Site Design System

- Treat `sass/site.scss` as the source of truth for semantic color, typography, radius,
  layout, and motion tokens. Extend an existing token before adding a raw visual value.
- Keep the terminal identity in the ASCII mark, monospace labels, code, prompts, and
  demo frames. Use the sans-serif stack for prose and larger interface headings.
- Use `--color-text` for primary content, `--color-muted` for supporting prose, and
  `--color-dim` only for short metadata. Interactive boundaries should use
  `--color-border-strong`, and keyboard focus should use `--color-focus`.
- Preserve the shared `:focus-visible` treatment and test both the `green` and `dark`
  themes when adding controls.
- Keep animated feature images behind the `data-motion-demo` loader so off-screen media
  remains deferred. Pair each GIF with a same-named PNG poster so
  `prefers-reduced-motion` users receive a meaningful static preview. Regenerate and
  visually inspect the poster whenever its GIF changes.

## Scale with Nested Sections

- Group larger topics into nested sections (`content/docs/<topic>/_index.md`).
- Render navigation from `get_section(...).subsections` so new sections appear
  automatically.
- Use `transparent = true` only when subsection pages should be merged into the parent
  listing.

## Prefer Mermaid for Diagrams

- Use fenced `mermaid` code blocks for flow, lifecycle, and architecture diagrams
  instead of ASCII trees.
- Keep node labels concise and let the docs-page template handle theme-aware Mermaid
  rendering.
- Mermaid diagrams in docs pages now ship with built-in `fit`, zoom, and drag-to-pan
  controls automatically, so authors do not need to add extra wrapper markup.

## Add a Feature Entry

The `/features/` page auto-discovers entries from individual `.md` files in
`content/features/`. To add a new feature:

1. Add an E2E feature test with the `FeatureTest` builder in
   `crates/agentty/tests/e2e/`.
1. Place the generated GIF in `static/features/`. `FeatureTest` writes this when VHS is
   installed; if GIF generation is skipped, do not add or keep the feature page until
   the matching asset exists. Successful GIF regeneration removes the previous
   same-named PNG so it cannot remain as a stale poster; recreate and inspect the poster
   before finalizing the feature.
1. Create `content/features/<name>.md` with the following front matter:
   ```toml
   +++
   title = "Feature title"
   description = "One-line description shown on the card."
   weight = <ordering number>

   [extra]
   gif = "<name>.gif"
   +++
   ```
1. Choose a `weight` that slots the entry into the desired display position (lower
   weights appear first).
1. Run `zola check` to verify the features page renders the new entry.

The `features.html` template uses `get_section(path="features/_index.md")` and iterates
`section.pages` ordered by `weight`. The homepage feature card in `index.html` is
hardcoded and curated separately.

## Authoring Workflow

1. Create a new Markdown page under `content/docs/`.
1. Add `title`, `description`, and `weight` front matter.
1. Add a `<!-- more -->` break so docs listings show concise summaries.
1. Use Zola `@/...` links for internal Markdown pages so renamed or missing targets fail
   validation instead of producing deployed `.md` links.
1. Keep every pipe-table header, delimiter, and body row on its own source line. Prefer
   short titled blocks when cells need full sentences.
1. Run `zola check` before publishing, then test sidebar search with a title and a term
   that appears only in page content.
