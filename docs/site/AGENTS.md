# Documentation Site

Applies to site configuration, templates, content, and static assets.

## Search

- Keep `build_search_index`, the `[search]` configuration, generated-index loading,
  sidebar behavior, and the `/docs/` result filter consistent when changing search.
- For search changes or broad documentation changes, verify rendered search by title and
  body terms in addition to the root docs-site checks.

## Styles

- Design values live in `design/tokens.json`. `docs/site/sass/_tokens.scss` is generated
  from it, and `docs/site/sass/site.scss` loads it with `@use`; keep only layout sizes
  and tokens derived from other tokens in `site.scss`.
- Shared foundations and components live in `design/base.css`, which
  `docs/site/sass/_base.scss` copies for `site.scss` to `@use` after the tokens. Change
  a rule there when the remote-control client or another workspace shares it, and keep
  site-only rules in `site.scss`.

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
