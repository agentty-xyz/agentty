# Documentation Site

Applies to site configuration, templates, content, and static assets.

## Search

- Keep `build_search_index`, the `[search]` configuration, generated-index loading,
  sidebar behavior, and the `/docs/` result filter consistent when changing search.
- For search changes or broad documentation changes, verify rendered search by title and
  body terms in addition to the root docs-site checks.

## Search Engine Metadata

- `docs/site/templates/partials/seo.html` owns titles, descriptions, canonical URLs,
  social tags, and structured data; templates must not emit their own.
- List each canonical URL once in the generated sitemap; keep redirecting sections out.
- Give every page and section a specific `description`.

## Feature Pages and Assets

- Each E2E test's `FeatureTest::zola()` metadata owns its generated feature title,
  description, and weight.
- Keep feature GIFs and same-named PNG posters paired under
  `docs/site/static/features/`; use `docs/contributing/feature-test/recording.md` for
  generation and verification.
