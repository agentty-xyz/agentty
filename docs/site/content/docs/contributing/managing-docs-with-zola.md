+++
title = "Managing Docs with Zola"
description = "Recommended structure and front-matter conventions for maintaining docs in Zola."
weight = 1
+++

<a id="managing-docs-introduction"></a> Use these conventions to keep Agentty
documentation maintainable as it grows.

<!-- more -->

## Keep URLs Stable

- Keep documentation under the `content/docs/` section, whose directory name keeps the
  canonical route at `/docs/`.
- When moving or renaming pages, add `aliases` in page front matter to preserve old
  links.
- For paragraph-level deep links, add an explicit HTML anchor before the paragraph, such
  as `<a id="some-paragraph-id"></a>`. The page renders a `#` affordance next to it so
  readers can copy the link.

## Use Section Metadata Deliberately

- Set `sort_by = "weight"` and define page `weight` values for intentional ordering.
- Keep `page_template` on the docs section so all guides share a consistent layout.
- Keep `build_search_index = true`, use the `fuse_json` format in the `[search]`
  configuration, and keep documentation pages in the index.
- The shared navigation searches documentation, feature pages, and blog posts. The
  documentation sidebar uses the same generated index and filters its results to
  `/docs/` routes.

## Keep Search Metadata Complete

- `templates/partials/seo.html` derives the title, meta description, canonical URL, Open
  Graph and Twitter tags, and JSON-LD for every page. Do not add per-template `<title>`
  or canonical tags.
- Give every page and section a specific `description` of about 150 characters; it
  becomes the search snippet and social preview text.
- Set `extra.seo_title` only when the generated `<page> | Agentty <Section>` title does
  not fit, and keep it under 60 characters.
- Set `extra.sitemap_exclude = true` on sections that only redirect.
- `static/social-card.png` is the 1200x630 link preview for every page except feature
  pages, which use their own poster. It shows the home page glyph art and hero text.
  Regenerate it from the repository root with `node docs/site/scripts/social-card.mjs`
  when the hero text, `static/glyph-art.js` scenes, or the theme colors change. The
  script needs network access for `npx` and the glyph font.

## Preserve the Site Design System

- Treat `sass/site.scss` as the source of truth for semantic color, typography, radius,
  layout, and motion tokens. Extend an existing token before adding a raw visual value.
- Keep the terminal identity in the ASCII mark, monospace labels, code, prompts, and
  demo frames. Use the sans-serif stack for prose and larger interface headings.
- Use `--color-text` for primary content, `--color-muted` for supporting prose, and
  `--color-dim` only for short metadata. Use `--color-border-strong` for interactive
  boundaries and `--color-focus` for keyboard focus.
- Preserve the shared `:focus-visible` treatment and test both the `green` and `dark`
  themes when adding controls.
- Keep animated feature images behind the `data-motion-demo` loader so off-screen media
  remains deferred. Pair each GIF with a same-named PNG poster so
  `prefers-reduced-motion` users receive a meaningful static preview.

## Scale with Nested Sections

- Group larger topics into nested sections (`content/docs/<topic>/_index.md`).
- Render navigation from `get_section(...).subsections` so new sections appear
  automatically.
- Use `transparent = true` only when subsection pages should be merged into the parent
  listing.

## Prefer Mermaid for Diagrams

- Use fenced `mermaid` code blocks for flow, lifecycle, and architecture diagrams
  instead of ASCII trees.
- Keep node labels concise. The docs-page template handles theme-aware rendering and
  adds `fit`, zoom, and drag-to-pan controls, so no wrapper markup is needed.

## Add a Feature Entry

The `/features/` page lists every `.md` file in `content/features/`: the `features.html`
template iterates `get_section(path="features/_index.md")` pages by `weight`, lower
weights first. The homepage feature card in `index.html` is hardcoded and curated
separately. To add a feature:

1. Add an E2E test with the `FeatureTest` builder and `.zola(...)` in
   `crates/agentty/tests/e2e/`, following `docs/contributing/feature-test/authoring.md`.
   Its first run writes `content/features/<name>.md` with this front matter:
   ```toml
   +++
   title = "Feature title"
   description = "One-line description shown on the card."
   weight = <ordering number>

   [extra]
   gif = "<name>.gif"
   +++
   ```
1. Record the GIF into `static/features/` with the `e2e-container` hook described in
   `docs/contributing/feature-test/recording.md`. Do not add or keep the feature page
   until the matching GIF exists.
1. Create the same-named PNG poster and inspect it. Successful GIF regeneration removes
   the previous poster so it cannot go stale; recreate and inspect it whenever the GIF
   changes.
1. Run `prek run zola-check --all-files --hook-stage manual` to verify the features page
   renders the new entry.

## Authoring Workflow

1. Create a Markdown page under `content/docs/` with `title`, `description`, and
   `weight` front matter.
1. Add a `<!-- more -->` break so docs listings show concise summaries.
1. Use Zola `@/...` links for internal Markdown pages so renamed or missing targets fail
   validation instead of producing deployed `.md` links.
1. Keep every pipe-table header, delimiter, and body row on its own source line. Prefer
   short titled blocks when cells need full sentences.
1. Run `prek run zola-check --all-files --hook-stage manual` before publishing, then
   test sidebar search with a title and a term that appears only in page content.
