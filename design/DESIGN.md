# Agentty Design

This guide describes how Agentty's web pages look and why. The values it names reach
pages as CSS custom properties through `tokens.css`, which is generated from a token
source in the W3C Design Tokens format. `base.css` holds the foundations and components
that the pages share. Load `tokens.css` first, then `base.css`, then a page's own rules,
and build every color, font, radius, shadow, and transition from the custom properties
instead of writing values by hand.

## Overview

Agentty is a terminal application, and its web pages look like a well-kept terminal: a
near-black field, one bright accent, monospace labels, and quiet panels framed like
terminal windows. The tone is calm and precise. Pages are dark in every theme, keep
decoration to the brand mark and the accent, and let content and status carry the color.

- **Terminal identity:** the brand mark, the brand name, navigation, labels, code,
  prompts, and window chrome use the monospace stack. Prose and large headings use the
  sans-serif stack.
- **One accent:** the accent marks what to act on or follow. Everything else is text,
  muted text, borders, and surfaces.
- **Restraint:** borders separate, surfaces lift, and a single shadow marks floating
  panels. Glows belong only to the logo and to live status dots.

### Brand Mark

The mark is a prompt chevron followed by a cursor line, drawn in the accent color beside
the name "Agentty" in the monospace stack. `mark.svg`, exported beside this guide, is
the one copy: it strokes with `currentColor` and is hidden from assistive technology,
because the name beside it says the same. Inline its markup in the page, inside an
element whose `color` is `--color-accent`, so the mark follows the theme; an `img`
cannot inherit the color. `favicon.ico`, exported beside it too, is the browser tab
icon. Copy both from the export rather than redrawing them.

## Colors

Each color token is a role, not a hue. Pick the role that matches the element's job, and
the theme supplies the value.

| Token                         | Role                                                      |
| ----------------------------- | --------------------------------------------------------- |
| `--color-bg`                  | Page background                                           |
| `--color-surface`             | Inset surfaces: inputs, inline code, badges, switches     |
| `--color-surface-raised`      | Panels, cards, dialogs, menus, secondary buttons          |
| `--color-surface-interactive` | Hovered, pressed, and selected controls                   |
| `--color-text`                | Body text and headings                                    |
| `--color-muted`               | Supporting prose, navigation, labels                      |
| `--color-dim`                 | Short metadata, placeholders, idle states                 |
| `--color-accent`              | Links, the brand mark, primary actions                    |
| `--color-accent-dim`          | Quieter accent: hover outlines, secondary links           |
| `--color-on-accent`           | Text and icons on an accent fill                          |
| `--color-border`              | Dividers and the outlines of panels                       |
| `--color-border-strong`       | Outlines that must stand out: buttons, dialogs, fields    |
| `--color-focus`               | Keyboard focus ring; also the hovered primary button fill |
| `--color-error`               | Errors, destructive actions                               |
| `--color-roadmap-future`      | Warm highlight: upcoming work, things in progress         |
| `--color-roadmap-horizon`     | Cool highlight: later work, things waiting for an answer  |
| `--color-scrim`               | Translucent backdrop behind dialogs, sheets, and menus    |

### Themes

There are two themes, and both are dark, so pages declare `color-scheme: dark`.

- **Green** is the default and the brand: green-tinted surfaces, mint text, and a bright
  green accent. `:root` holds its values.
- **Dark** is neutral: gray surfaces, near-white text, and a white accent. Adding the
  `theme-dark` class to the root element selects it, and `tokens.css` overrides the
  tokens that differ under `:root.theme-dark`. Error, on-accent, the roadmap highlights,
  the scrim, fonts, radii, shadows, and motion are the same in both themes.

Apply a saved theme before the first paint, with a small inline script in the document
head, so the page never flashes the other theme. Offer both themes wherever a page lets
people choose, and test every new component in both.

### Status Tones

Session and task states use a dot and a short label in one tone. Name the tone after the
state, then map it to a color token:

| State                         | Tone token                | Dot              |
| ----------------------------- | ------------------------- | ---------------- |
| Working, in progress          | `--color-roadmap-future`  | Glows and pulses |
| Asking, waiting for an answer | `--color-roadmap-horizon` | Glows            |
| Ready for review              | `--color-accent`          | Solid            |
| Queued, syncing, merging      | `--color-accent-dim`      | Solid            |
| Draft, idle, unknown          | `--color-dim`             | Hollow ring      |
| Done, merged                  | `--color-dim`             | Solid            |
| Failed, canceled              | `--color-error`           | Solid            |

Never let color alone carry a state: always pair the dot with its label.

### Accessibility

Text meets WCAG 2 AA contrast, 4.5:1, and focus rings, status dots, and outlines that
identify a control meet 3:1, for every foreground and background pair the pages use, in
both themes. These pairs fall short today:

- In the green theme, `--color-dim` text on `--color-surface-raised` reaches only
  4.41:1, and on `--color-surface-interactive` 4.01:1. Inside raised panels and on
  selected rows, use `--color-muted` for anything people must read.
- `--color-border` outlines fields at 1.4:1 to 2.0:1, and `--color-border-strong`
  outlines them below 3:1 on `--color-surface` and `--color-surface-raised` in the dark
  theme. Give a new field `--color-border-strong` on `--color-bg`, which passes in both
  themes.

Keep the shared focus ring on every focusable element, follow the system's reduced
motion setting, and give every page a skip link to its main content.

### Terminal Palette

Agentty's terminal interface has its own palette with its own role names, because a
terminal renders colors differently from a browser and some users keep their terminal's
colors. It offers three themes:

- **Agentty Default** uses the terminal's own ANSI colors, with a cyan accent, so it
  follows whatever color scheme the terminal has.
- **Agentty Green** is a green phosphor palette. It shares the web green theme's
  identity, green on near-black, but its values are tuned for terminals and are not the
  web values.
- **Dark Horizon** is a warm dark palette with a cyan accent, inspired by the Horizon
  editor theme.

The web themes and the terminal palette agree on roles, not values. Never copy a value
from one into the other; when a role changes meaning, change both. The roles correspond
as follows:

| Web token                     | Terminal role       |
| ----------------------------- | ------------------- |
| `--color-text`                | `text`              |
| `--color-muted`               | `text_muted`        |
| `--color-dim`                 | `text_subtle`       |
| `--color-accent`              | `accent`            |
| `--color-accent-dim`          | `accent_soft`       |
| `--color-border`              | `border`            |
| `--color-error`               | `danger`            |
| `--color-roadmap-future`      | `warning`           |
| `--color-roadmap-horizon`     | `question`          |
| `--color-surface-raised`      | `surface_elevated`  |
| `--color-surface-interactive` | `surface_selection` |

The terminal also has success and info roles, and tinted surfaces for diffs and prompts,
which the web pages do not use. Some of its status colors differ from the web tones:
draft states use muted text where the web uses the dim tone, done states use success
green, review states info blue, and syncing and merging the full accent.

## Typography

| Token         | Stack                                                                               |
| ------------- | ----------------------------------------------------------------------------------- |
| `--font-sans` | Inter, `ui-sans-serif`, `-apple-system`, BlinkMacSystemFont, Segoe UI, `sans-serif` |
| `--font-mono` | Share Tech Mono, Courier New, `monospace`                                           |

- Share Tech Mono is a web font. Load it from Google Fonts
  (`https://fonts.googleapis.com/css2?family=Share+Tech+Mono&display=swap`) with
  `display=swap`, and preconnect to `fonts.googleapis.com` and `fonts.gstatic.com`.
  Inter is not loaded: pages use it where it is installed and fall back to the system
  sans-serif face.
- A page that must load nothing from the network, such as one with a strict content
  security policy, keeps the same token name but puts system monospace faces
  (`ui-monospace`, SF Mono, Menlo, Consolas) before Courier New.
- Body text is 1rem at line height 1.6 with `text-rendering: optimizeLegibility`. Lead
  paragraphs use `--color-muted` at about 1.1rem and line height 1.7, kept to 60ch.
- Headings use the sans-serif stack at weight 600 to 700 with negative letter spacing,
  from about -0.025em on small headings to -0.055em on the largest, and line heights
  from 1.25 down to about 1 as the size grows.
- Eyebrows, the small labels above headings, use the monospace stack in `--color-accent`
  at 0.72rem to 0.78rem, uppercase, with 0.08em letter spacing.
- The brand name uses the monospace stack at 1.1rem, weight 600, with 0.02em letter
  spacing.

## Layout

- Pages are a sticky header over one centered column. Prose columns are about 800px
  wide, wide sections 960px, and documentation with a sidebar 1240px; a chat column is
  about 760px.
- Horizontal padding is fluid, such as `clamp(1rem, 3vw, 1.5rem)`, so pages hold up from
  phones to desktops without separate layouts. On narrow screens, header groups wrap
  onto their own rows instead of shrinking.
- Spacing follows a loose rem scale: 0.35rem and 0.5rem inside controls, 0.75rem to 1rem
  between related items, and 1.25rem to 2rem between groups. Section padding grows with
  the viewport, from about 3rem to 5.5rem.
- Controls are at least 2.15rem tall, and primary actions 2.75rem, so they stay easy to
  hit.

## Elevation & Depth

Depth comes from surface color first and shadow second.

1. `--color-bg` is the page.
1. `--color-surface` sits inset into it: fields, code, badges.
1. `--color-surface-raised` lifts panels, cards, menus, and dialogs.
1. `--color-surface-interactive` marks the hovered or selected control on top.

`--shadow-panel` is the only shadow, for panels that float above content: dialogs, the
search panel, and window-framed panels. Behind a modal, cover the page with
`--color-scrim` and an 8px backdrop blur. The sticky header lets content scroll beneath
it through a translucent `--color-bg` and an 18px blur.

## Shapes

| Token         | Use                                           |
| ------------- | --------------------------------------------- |
| `--radius-sm` | Buttons, fields, badges, menu items (0.35rem) |
| `--radius-md` | Cards and large fields (0.65rem)              |
| `--radius-lg` | Panels, dialogs, and composers (0.9rem)       |

Small badges and keycaps use 0.25rem, inline code 0.3rem, and status dots and window
dots are circles. Borders are 1px; focus rings are 2px. A layout that wants a stricter
terminal look, such as a landing page, may set the three radius tokens to 0 on its
container so every panel inside turns square.

## Components

`base.css` provides these. Use its class names, and add page-specific rules after it.

- **Focus ring:** links, buttons, and inputs show a 2px `--color-focus` outline, 3px
  out, on `:focus-visible`. Extend the same rule to any other focusable element.
- **Visually hidden text:** `.sr-only` keeps text for screen readers only.
- **Skip link:** `.skip-link` sits above the page and slides into view on focus. Make it
  the first element in the body and point it at the main content.
- **Site header:** `.site-nav` is the sticky, blurred header bar; `.site-nav-inner`
  spaces its contents; `.site-brand` is the linked brand name, with the mark inside
  `.site-brand-icon`.
- **Buttons:** `.button` with `.button-primary` for the one main action, an accent fill
  that turns to the focus color on hover, or `.button-secondary` for the rest, a raised
  surface with a strong border. Buttons lift by 1px on hover.
- **Terminal window:** `.terminal-window` is a raised panel with a strong border, the
  large radius, and the panel shadow. Its `.terminal-window-bar` holds three dots in
  `.terminal-window-dots` (empty `i` elements), a title, and an optional trailing item,
  in the monospace stack and `--color-dim`.

Pages build these from the same tokens when they need them:

- **Eyebrow and lead paragraph:** see Typography.
- **Fields:** a `--color-bg` fill, a 1px `--color-border-strong` outline, and
  `--radius-md`, with the outline turning `--color-focus` while the field has focus.
- **Theme switch:** a segmented control on `--color-surface` whose current option sits
  on `--color-surface-interactive` in `--color-accent`.
- **Status:** a 0.45rem dot and a monospace label, both in the state's tone.
- **Inline code:** `--color-surface` with a `--color-border` outline, a 0.3rem radius,
  and `--color-accent` monospace text slightly smaller than the surrounding text.

## Do's and Don'ts

- Do build every color, font, radius, shadow, and transition from the custom properties;
  don't write a hex value, `rgb()`, or a named color in a stylesheet.
- Do change a value only at its token source and regenerate `tokens.css`; don't edit
  `tokens.css`.
- Do keep one primary action per view; don't fill several buttons with the accent.
- Do pair every status color with a label; don't signal state by color alone.
- Do use `--color-muted` for text people must read in raised panels; don't use
  `--color-dim` there, which falls short of AA contrast in the green theme.
- Do test both themes and keyboard focus; don't add a light theme without a design for
  every token.
- Do keep motion short, `--transition-fast` for color and small movements, and honor
  reduced motion; don't animate layout or loop decoration other than live status.
- Don't load images or scripts from other origins. Web fonts are the one exception, and
  pages whose content security policy forbids them use the system stacks instead.
