# Documentation Site

Applies to site configuration, templates, content, and static assets.

## Search

- Keep `build_search_index`, the `[search]` configuration, generated-index loading,
  sidebar behavior, and the `/docs/` result filter consistent when changing search.
- For search changes or broad documentation changes, verify rendered search by title and
  body terms in addition to the root docs-site checks.

## Styles

- Design values and the self-hosted monospace face live in `design/tokens.json`.
  `docs/site/sass/_tokens.scss` and `docs/site/sass/_fonts.scss` are generated from it,
  and `docs/site/sass/site.scss` loads them with `@use`; keep only site-specific layout
  sizes and tokens derived from other tokens in `site.scss`, and take type, spacing,
  widths, and control heights from the token scales. Load no font from another origin
  except the home page's display face.
- Shared foundations and components live in `design/base.css`, which
  `docs/site/sass/_base.scss` copies for `site.scss` to `@use` after the tokens and
  fonts. Change a rule there when the remote-control client or another workspace shares
  it, and keep site-only rules in `site.scss`.
- `docs/site/static/mark.svg`, `docs/site/static/favicon.ico`, and the `theme-color`
  value in `docs/site/templates/partials/seo.html` are generated from `design/`; change
  them there. Scripts and templates read colors and fonts from the custom properties at
  runtime. The theme switch leaves the `theme-color` meta at the default theme's value.

## Search Engine Metadata

- `docs/site/templates/partials/seo.html` owns titles, descriptions, canonical URLs,
  social tags, and structured data; templates must not emit their own.
- List each canonical URL once in the generated sitemap; keep redirecting sections out.
- Give every page and section a specific `description`.

## Feature Pages and Assets

- Each E2E test's `FeatureTest::zola()` metadata owns its generated feature title,
  description, and weight.
- Keep feature GIFs and same-named PNG posters paired under
  `docs/site/static/features/`; use `skills/refresh-feature-recordings/SKILL.md` for
  generation and verification.
