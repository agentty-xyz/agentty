# Agentty Design

This guide describes how Agentty's web pages and its native iOS and Android apps look
and why. Both use the same token roles, and every token value has one home:
`tokens.json`, a token source in the W3C Design Tokens format.

- **Web pages** receive the values as CSS custom properties through `tokens.css`, which
  is generated from the token source. `fonts.css`, generated from the same source,
  embeds the self-hosted monospace face, and `base.css` holds the foundations and
  components that the pages share. Load `tokens.css` first, then `fonts.css`, then
  `base.css`, then a page's own rules, and build every color, font, type size, spacing,
  width, control height, radius, shadow, and transition from the custom properties
  instead of writing values by hand.
- **Native apps** take the values from `tokens.json` through code that the app's own
  build generates from it, with both themes and the high-contrast values, and never copy
  a hex value. [Native Apps](#native-apps) describes where the apps follow their
  platform instead of the web.

## Overview

Agentty is a terminal application, and its web pages and native apps look like a
well-kept terminal: a near-black field, one bright accent, monospace labels, and quiet
panels framed like terminal windows. The tone is calm and precise. Pages and apps are
dark in every theme, keep decoration to the brand mark and the accent, and let content
and status carry the color.

- **Terminal identity:** on the web, the brand mark, the brand name, navigation, labels,
  code, prompts, and window chrome use the monospace stack, and prose and large headings
  use the sans-serif stack. For the native apps, see [Native Apps: Type](#type).
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
the theme supplies the value. The table names each role's custom property; a native app
uses the same role under its token path, such as `color.bg` for `--color-bg`.

| Token                         | Role                                                      |
| ----------------------------- | --------------------------------------------------------- |
| `--color-bg`                  | Page and screen background                                |
| `--color-surface`             | Inset surfaces: inputs, inline code, badges, switches     |
| `--color-surface-raised`      | Panels, cards, dialogs, menus, secondary buttons          |
| `--color-surface-interactive` | Hovered, pressed, and selected controls                   |
| `--color-text`                | Body text and headings                                    |
| `--color-muted`               | Supporting prose, navigation, labels                      |
| `--color-dim`                 | Short metadata, placeholders, idle states                 |
| `--color-accent`              | Links, the brand mark, primary actions                    |
| `--color-accent-dim`          | Quieter accent: hover outlines, secondary links           |
| `--color-on-accent`           | Text and icons on an accent fill                          |
| `--color-border`              | Dividers and the outlines of panels, cards, and badges    |
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
head, so the page never flashes the other theme. It reads the choice the theme switch
saved, under the page's own storage key, and tolerates storage that is blocked:

```html
<script>
  try {
    if (localStorage.getItem("agentty-theme") === "dark") {
      document.documentElement.classList.add("theme-dark");
    }
  } catch (error) {
    // Without storage the default theme applies.
  }
</script>
```

A page whose content security policy forbids inline scripts serves the same lines as a
file and loads it with a plain `<script src>` in the head, neither deferred nor a
module, so it still runs before the first paint. Offer both themes wherever a page lets
people choose, and test every new component in both. For the native apps, see
[Native Apps: Appearance and Color](#appearance-and-color).

Code outside a stylesheet takes values from the same tokens instead of copying them. A
script running in the page reads the custom property, such as
`getComputedStyle(document.documentElement).getPropertyValue("--color-bg")`, after the
theme is applied. A value that must be written into static markup, such as the
`theme-color` meta that tints the browser's own interface, comes from `tokens.json`: the
default theme's page background is the `hex` of `color.bg`'s `$value`. A page that
switches themes may keep that value or set the meta from `--color-bg` each time it
applies one.

### Status Tones

Session and task states use a dot and a short label in one tone. Name the tone after the
state, then map it to a color token; the status component's modifier applies both:

| State                         | Modifier           | Tone token                | Dot              |
| ----------------------------- | ------------------ | ------------------------- | ---------------- |
| Working, in progress          | `.status--working` | `--color-roadmap-future`  | Glows and pulses |
| Asking, waiting for an answer | `.status--asking`  | `--color-roadmap-horizon` | Glows            |
| Ready for review              | `.status--ready`   | `--color-accent`          | Solid            |
| Queued, syncing, merging      | `.status--queue`   | `--color-accent-dim`      | Solid            |
| Draft, idle                   | `.status--idle`    | `--color-dim`             | Hollow ring      |
| Done, merged                  | `.status--done`    | `--color-dim`             | Solid            |
| Failed, canceled              | `.status--failed`  | `--color-error`           | Solid            |

A status with no modifier is an unknown state, in `--color-dim` with a solid dot. Add
`.status--live` to any tone whose work is still running, such as syncing, so its dot
glows and pulses too.

Never let color alone carry a state: always pair the dot with its label, on a page and
in an app alike.

### Accessibility

Every pair of color tokens that the pages and apps draw with, foreground on background,
meets WCAG 2 AA in both themes, with no exceptions. System components in the apps that
keep their platform colors are outside these pairs.

- Text, including `--color-dim`, reaches 4.5:1 on `--color-bg`, `--color-surface`,
  `--color-surface-raised`, and `--color-surface-interactive`. Labels on a primary
  button reach 4.5:1 on both its fills.
- Focus rings, status dots, and primary button fills reach 3:1 on every surface.
- A form field's boundary is what identifies it, so outline every field in
  `--color-border-strong`, which reaches 3:1 against `--color-bg`, `--color-surface`,
  `--color-surface-raised`, and `--color-surface-interactive`, whichever is its fill or
  surrounds it, as on a native app's unchecked switch track. `--color-border` is for
  dividers and the outlines of panels, cards, and badges, which WCAG does not require to
  stand out; never outline a field with it.

When the system asks for more contrast, `--color-muted`, `--color-dim`, and
`--color-accent-dim` take their high-contrast values, which reach 7:1 on the same four
surfaces in both themes. Every other token keeps its value, and every pair above keeps
its minimum. Each platform asks as follows:

- **Web:** `prefers-contrast: more`, under which `tokens.css` sets the values.
- **iOS:** Increase Contrast, which UIKit reports as `UIAccessibilityContrast.high` and
  SwiftUI as `colorSchemeContrast == .increased`.
- **Android:** `UiModeManager.getContrast()` above 0, the standard level, on API 34 and
  later, so the medium and high contrast settings both select the values. At 0, and on
  earlier versions, the standard values apply.

A native app follows a change to the setting while it runs: on iOS through the trait
change, and on Android through `UiModeManager.addContrastChangeListener`.

The token source lists these pairs, and its check fails when any of them falls below its
minimum in either theme, at standard or at high contrast, so a new color or a new
pairing must pass before it lands. Keep the shared focus ring on every focusable
element, follow the system's reduced motion setting, and give every page a skip link to
its main content.

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

| Token         | Stack                                                                                               |
| ------------- | --------------------------------------------------------------------------------------------------- |
| `--font-sans` | Inter, `ui-sans-serif`, `-apple-system`, BlinkMacSystemFont, Segoe UI, `sans-serif`                 |
| `--font-mono` | Share Tech Mono, `ui-monospace`, SF Mono, SFMono-Regular, Menlo, Consolas, Courier New, `monospace` |

- Every page uses the same two stacks. Share Tech Mono is self-hosted: `fonts.css`
  embeds its one face, weight 400, as a WOFF2 `data:` URI with `font-display: swap`,
  after a comment with its copyright and SIL Open Font License notice, so it loads from
  no other origin and works wherever the stylesheet is copied. System monospace faces
  stand in until it loads. A page with a content security policy allows it with
  `font-src data:`. The font file the token source names,
  `fonts/ShareTechMono-Regular.woff2`, and its full licence, `fonts/OFL.txt`, are
  exported beside this guide.
- Inter is not loaded: pages use it where it is installed and fall back to the system
  sans-serif face.
- The documentation home page alone also uses Martian Mono, a display face for its hero
  and glyph art, loaded from Google Fonts on that page only. No other page or component
  uses it.
- For the native apps' faces and sizes, see [Native Apps: Type](#type).

### Type Scale

Take every size, weight, line height, and letter spacing from these tokens. A display
heading may grow fluidly with `clamp()` between two of them, and the few sizes that
belong to one figure, such as a hero title, stay where that figure is styled.

| Size token         | Value   | Use                                           |
| ------------------ | ------- | --------------------------------------------- |
| `--font-size-3xs`  | 0.68rem | Keycaps, badges, the smallest metadata        |
| `--font-size-2xs`  | 0.72rem | Eyebrows, window bars, status labels          |
| `--font-size-xs`   | 0.78rem | Navigation, small labels, search hints        |
| `--font-size-sm`   | 0.82rem | Field labels, table text, compact controls    |
| `--font-size-md`   | 0.86rem | Buttons, secondary text in lists              |
| `--font-size-lg`   | 0.92rem | Descriptions and supporting prose in panels   |
| `--font-size-body` | 1rem    | Body text and form input                      |
| `--font-size-xl`   | 1.1rem  | Lead paragraphs and the brand name            |
| `--font-size-2xl`  | 1.35rem | Dialog and panel titles                       |
| `--font-size-3xl`  | 2rem    | Large display figures, such as a pairing code |

| Weight token             | Value | Use                                    |
| ------------------------ | ----- | -------------------------------------- |
| `--font-weight-regular`  | 400   | Body text                              |
| `--font-weight-medium`   | 500   | Emphasis inside tables                 |
| `--font-weight-semibold` | 600   | Buttons, the brand name, strong labels |
| `--font-weight-heading`  | 650   | Section and content headings           |
| `--font-weight-bold`     | 700   | Page titles and the strongest emphasis |

| Line height token       | Value | Use                                    |
| ----------------------- | ----- | -------------------------------------- |
| `--line-height-none`    | 1     | Single-line controls, display headings |
| `--line-height-tight`   | 1.1   | Large headings                         |
| `--line-height-snug`    | 1.25  | Small headings, labels that may wrap   |
| `--line-height-compact` | 1.4   | Metadata, cards, inline code           |
| `--line-height-normal`  | 1.5   | Tables, notices, buttons with text     |
| `--line-height-body`    | 1.6   | Body text and code blocks              |
| `--line-height-relaxed` | 1.7   | Lead paragraphs and long answers       |

| Tracking token        | Value    | Use                                   |
| --------------------- | -------- | ------------------------------------- |
| `--tracking-tightest` | -0.055em | The largest display heading           |
| `--tracking-tighter`  | -0.045em | Page titles                           |
| `--tracking-tight`    | -0.035em | Section titles                        |
| `--tracking-snug`     | -0.025em | Small headings and dialog titles      |
| `--tracking-wide`     | 0.02em   | The brand name and monospace headings |
| `--tracking-wider`    | 0.04em   | Uppercase group and table labels      |
| `--tracking-widest`   | 0.08em   | Eyebrows and other uppercase captions |

- Body text is `--font-size-body` at `--line-height-body` with
  `text-rendering: optimizeLegibility`. Lead paragraphs use `--color-muted` at
  `--line-height-relaxed`, kept to 60ch, growing from `--font-size-body` on a phone to
  `--font-size-xl`.
- Headings use the sans-serif stack at `--font-weight-heading` or `--font-weight-bold`,
  and tighten as they grow: `--tracking-snug` and `--line-height-snug` on small
  headings, down to `--tracking-tightest` and `--line-height-none` on the largest.
- Eyebrows, the small labels above headings, use the monospace stack in `--color-accent`
  at `--font-size-2xs`, uppercase, with `--tracking-widest`.
- The brand name uses the monospace stack at `--font-size-xl`, `--font-weight-semibold`,
  and `--tracking-wide`.

## Layout

| Token             | Value  | Use                                      |
| ----------------- | ------ | ---------------------------------------- |
| `--width-reading` | 760px  | A documentation article or a chat column |
| `--width-prose`   | 800px  | A prose page                             |
| `--width-wide`    | 960px  | Wide sections, such as feature grids     |
| `--width-docs`    | 1240px | Documentation with its sidebars          |

| Token         | Value   | Token         | Value   |
| ------------- | ------- | ------------- | ------- |
| `--space-3xs` | 0.15rem | `--space-xl`  | 1.25rem |
| `--space-2xs` | 0.25rem | `--space-2xl` | 1.5rem  |
| `--space-xs`  | 0.35rem | `--space-3xl` | 2rem    |
| `--space-sm`  | 0.5rem  | `--space-4xl` | 3rem    |
| `--space-md`  | 0.75rem | `--space-5xl` | 4rem    |
| `--space-lg`  | 1rem    |               |         |

| Token                 | Value   | Use                                             |
| --------------------- | ------- | ----------------------------------------------- |
| `--control-height-xs` | 1.75rem | Options inside a segmented control or a toolbar |
| `--control-height-sm` | 2rem    | Compact buttons beside content, such as copy    |
| `--control-height-md` | 2.15rem | The minimum for every other control             |
| `--control-height-lg` | 2.75rem | Primary actions, fields, every control on touch |
| `--control-height-xl` | 3.25rem | Search and code entry fields                    |

- Pages are a sticky header over one centered column, `--width-prose` wide, or
  `--width-wide` and `--width-docs` for wider sections; an article or a chat column is
  `--width-reading`. Sidebars and other surface-specific sizes stay in the surface's own
  stylesheet.
- Horizontal padding is fluid, such as `clamp(var(--space-lg), 3vw, var(--space-2xl))`,
  so pages hold up from phones to desktops without separate layouts. On narrow screens,
  header groups wrap onto their own rows instead of shrinking.
- Spacing comes from the `--space-*` scale: `--space-xs` and `--space-sm` inside
  controls, `--space-md` and `--space-lg` between related items, and `--space-xl` to
  `--space-3xl` between groups. Section padding grows with the viewport, from
  `--space-4xl` to about 5.5rem.
- Controls are at least `--control-height-md` tall, and primary actions and every
  control on a touch layout `--control-height-lg`, so they stay easy to hit.

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

| Token         | Use                                               |
| ------------- | ------------------------------------------------- |
| `--radius-sm` | Buttons, segmented controls, menu items (0.35rem) |
| `--radius-md` | Cards and fields (0.65rem)                        |
| `--radius-lg` | Panels, dialogs, and composers (0.9rem)           |

Small badges and keycaps use 0.25rem, inline code 0.3rem, and status dots and window
dots are circles. Borders are 1px; focus rings are 2px. A layout that wants a stricter
terminal look, such as a landing page, may set the three radius tokens to 0 on its
container so every panel inside turns square.

## Components

`base.css` provides these, and `gallery.html`, exported beside this guide, shows each
one in every state it styles, in both themes. Use the class names and markup below, and
add page rules after `base.css`. A page may place a component, with its margins,
position, flex or grid placement, and width, but never restyle it: a page rule that
names a component sets nothing else. Change the component here instead, so every page
keeps the same one.

- **Foundations:** the root declares `color-scheme: dark`; the body takes the page
  background, body text, and the sans-serif stack; `code`, `kbd`, `pre`, and `samp` take
  the monospace stack; links are `--color-accent`.
- **Focus ring:** links, buttons, inputs, selects, text areas, summaries, and elements
  with a `tabindex` show a 2px `--color-focus` outline, 3px out, on `:focus-visible`.
  Extend the same rule to any other focusable element.
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
  in the monospace stack and `--color-dim`. Below 560px the bar drops its title.
- **Eyebrow and lead paragraph:** `p.eyebrow` and `.lead`, as Typography describes. A
  `strong` inside a lead takes `--color-text`. Neither sets a margin.
- **Fields:** `.field` on an `input`, `select`, or `textarea`: a `--color-bg` fill, a
  1px `--color-border-strong` outline, `--radius-md`, and `--control-height-lg`, with
  placeholders in `--color-dim`. The outline turns `--color-focus` while the field has
  focus, `--color-error` with `aria-invalid="true"`, and a `disabled` field dims its
  text. For search and code entry, `label.field-prompt` holds a `.field-prompt-mark`,
  such as `>`, and a bare `input`; the label draws the field, `--control-height-xl`
  tall, and its focus ring while the input has focus. `.field-prompt--code` sets the
  input in the monospace stack at `--font-size-xl` for a code typed exactly.
- **Badges:** `.badge` is a small `--color-surface` label with a `--color-border`
  outline in the monospace stack at `--font-size-3xs`; `kbd.badge` is a keycap. A
  `.badge--pair` holds a `.badge-label` and a `.badge-value` in `--color-accent`, such
  as a count. Inside a hovered link, a badge's outline and label turn
  `--color-accent-dim`. A badge may hold a status.
- **Status:** `.status.status--<tone>` holds an `i.status-dot` and the label: a 0.45rem
  dot and monospace label at `--font-size-2xs`, both in the tone that Status Tones
  lists. The modifier also sets `--status-tone`, so a page's own element can take the
  same color. `.status--line` is a status that reads as a line of text and wraps, at
  `--font-size-sm` and `--line-height-compact`, with its dot centred on the first line.
  The pulse stops when the system asks for reduced motion. Badges and statuses set their
  own letter spacing and case, so a label around them cannot change either.
- **Inline code and tables:** `code.code` is inline code: `--color-surface` with a
  `--color-border` outline, a 0.3rem radius, and `--color-accent` monospace text at
  0.9em. `table.table` is a data table at `--font-size-md`: a `--color-surface` header
  row of uppercase `--color-accent` labels, `--color-muted` cells with the first column
  in `--color-accent-dim`, a divider between rows, and a hovered row on
  `--color-surface`. Rendered Markdown cannot carry classes, so mark its container
  `.prose`: bare `table` elements and `code` elements without a class outside `pre`
  inside it take the same styles.
- **Segmented control:** `fieldset.segmented` with a `legend.sr-only` holds `button`
  options on `--color-surface`, for a small set of exclusive options; the option with
  `aria-pressed="true"` sits on `--color-surface-interactive` in `--color-accent`, a
  hovered one turns `--color-text`, and a `disabled` one dims. On a touch screen each
  option is `--control-height-lg` tall. A theme switch is one possible use, with a
  `data-theme` attribute of `green` or `dark` on each option: its script toggles the
  `theme-dark` class on the root element, sets `aria-pressed` on the options, and saves
  the choice for the snippet in Themes.

### Gallery Check

`gallery-check.mjs`, exported beside this guide, checks that a page renders the
components as `gallery.html` does. Copy the gallery beside the shared stylesheets, and
give the script each page's whole stylesheet chain, in load order, as it reaches the
browser:

```sh
bun gallery-check.mjs gallery.html \
  --surface site=tokens.css,fonts.css,base.css,site.css \
  --markup site=templates \
  --surface app=compiled/app.css \
  --markup app=src/index.html,src/views
```

For each page, it renders the gallery's markup once with the stylesheets the gallery
links and once with the page's chain, in both themes, on a desktop and a touch phone
viewport, on a desktop that asks for reduced motion, and on one that asks for more
contrast, at rest and with each interactive element hovered, focused, and pressed, and
compares the computed styles of every element inside a `data-specimen` element, other
than the placement properties a page may set. A forced state compares the whole
specimen, so a field that shows its input's focus with `:focus-within` counts.
Animations and transitions are read, then frozen for the rest, and scripts are off. The
browser refuses every request but a `data:` URI, so nothing leaves the machine, and the
check fails on, and names, any stylesheet, font, or image a page tried to load. It also
fails on a page rule that names a component, or a bare element inside `.prose`, to set
anything but placement; hiding with `display: none` counts as placement. The optional
`--markup` files and directories are the page's templates and scripts: a class that
shares a quoted string with a component's class there, as in
`class="status status-line"` or a script's `"status status-line"`, sits on the
component's element, so a rule on it may only place it too. It reads only the files it
is given and runs on Bun or Node.js, since it imports only `node:` modules, with a
Chromium-based browser: Chrome, Chromium, or `chrome-headless-shell`, which
`CHROME_PATH` names, or which it finds in the usual install places. Compile a Sass or
bundled stylesheet first, and pass its output.

The check does not see a page's own markup around a component, so keep these by hand:

- A page rule on an element type in a page context, such as `.panel p`, outranks a
  component on that element, such as `p.eyebrow`. Exclude the component, as in
  `.panel p:where(:not(.eyebrow))`.
- A component inherits what it does not set, such as `letter-spacing` from a label
  around it. Components set the text properties they depend on; look at a component in
  any styled context it is placed in.
- A class the page adds to a component's element in a way the markup scan misses, such
  as a class string assembled from separate literals, escapes the rule audit.

## Native Apps

Agentty's native apps use the color roles, both themes, the status tones, and the
accessibility minimums above. The stylesheets, components, and gallery are the web's: in
an app, the platform's own components take their place, and the rules below say where
each platform's conventions replace the web's. An app's screens and navigation belong to
the app, with one rule: its top-level sections are tabs, three to five of them, and the
tab set is the same for every account, administrators included.

### System Components

- Tab bars, navigation and app bars, alerts and dialogs, sheets, menus, and refresh
  controls are the platform's own, and the app does not restyle them.
- On iOS, never give a bar a custom background, which turns off Liquid Glass, and never
  use Liquid Glass in the content layer.
- The brand lives in the content layer that scrolls beneath the bars: solid token
  surfaces, such as `color.bg` for the screen and `color.surface-raised` for cards, and
  the accent only on the one prominent action, the selected tab, links, and badges.

### Appearance and Color

- **Dark only:** the app forces the dark appearance, so system alerts, menus, and
  keyboards match its screens. This deliberately departs from Apple's guidance to follow
  the system appearance: Agentty's identity is a terminal's, and no light theme has a
  design for every token. The app offers both themes, Green and Dark.
- **Android color:** a static brand scheme mapped onto Material roles; the app never
  takes dynamic color from the wallpaper. The table sets every role of Material's dark
  color scheme, so none keeps a Material default, and the token source's contrast list
  holds every pair it creates.

| Material roles                                                                 | Token                       | Use                                                    |
| ------------------------------------------------------------------------------ | --------------------------- | ------------------------------------------------------ |
| `primary`, `primaryFixed`, `primaryFixedDim`                                   | `color.accent`              | The primary action                                     |
| `secondary`, `secondaryFixed`, `secondaryFixedDim`                             | `color.accent`              | The selected navigation item                           |
| `onPrimaryContainer`, `onSecondaryContainer`                                   | `color.accent`              | Text and icons on a tonal fill                         |
| `tertiary`, `tertiaryFixed`, `tertiaryFixedDim`, `onTertiaryContainer`         | `color.accent-dim`          | Quieter accents                                        |
| `onPrimary`, `onPrimaryFixed`, `onPrimaryFixedVariant`                         | `color.on-accent`           | Labels on an accent fill                               |
| `onSecondary`, `onSecondaryFixed`, `onSecondaryFixedVariant`                   | `color.on-accent`           | Labels on an accent fill                               |
| `onTertiary`, `onTertiaryFixed`, `onTertiaryFixedVariant`, `onError`           | `color.on-accent`           | Labels on a quieter-accent or error fill               |
| `inversePrimary`                                                               | `color.on-accent`           | An action on the inverse surface                       |
| `background`, `surface`, `surfaceDim`, `surfaceContainerLowest`                | `color.bg`                  | The screen                                             |
| `surfaceTint`                                                                  | `color.bg`                  | No tint: elevation comes from the container roles      |
| `inverseOnSurface`                                                             | `color.bg`                  | Text on the inverse surface                            |
| `surfaceContainerLow`                                                          | `color.surface`             | Inset surfaces and sheets                              |
| `surfaceVariant`, `surfaceContainer`, `surfaceContainerHigh`, `errorContainer` | `color.surface-raised`      | Navigation bars, cards, menus, dialogs, error fills    |
| `surfaceContainerHighest`, `surfaceBright`                                     | `color.surface-interactive` | The highest surfaces, such as filled fields and tracks |
| `primaryContainer`, `secondaryContainer`, `tertiaryContainer`                  | `color.surface-interactive` | Tonal fills, such as the selected item's indicator     |
| `onBackground`, `onSurface`                                                    | `color.text`                | Body text and headings                                 |
| `inverseSurface`                                                               | `color.text`                | The inverse surface, such as a snackbar                |
| `onSurfaceVariant`                                                             | `color.muted`               | Supporting text and icons                              |
| `outline`                                                                      | `color.border-strong`       | Field and button outlines                              |
| `outlineVariant`                                                               | `color.border`              | Dividers and card outlines                             |
| `error`, `onErrorContainer`                                                    | `color.error`               | Errors and destructive actions                         |
| `scrim`                                                                        | `color.scrim`               | The backdrop behind sheets and dialogs                 |

### Type

- Body text, titles, list rows, and buttons use the system sans: SF Pro through the iOS
  text styles, and Roboto through the Material type scale.
- Share Tech Mono is only for the brand name, eyebrows, status labels, badges, keycaps,
  and the pairing code. It is never smaller than 11pt or 11sp, and it scales with
  Dynamic Type and Android font scaling.
- Transcript code, diffs, and logs use the platform monospace: SF Mono on iOS and the
  system monospace on Android. Share Tech Mono has one weight; the platform monospace
  has the weights that Bold Text needs and reads better at small sizes. Code wraps or
  scrolls; it never truncates.
- Text scales with the system everywhere. Cap scaling only on bar chrome, never on
  content.

Each web size takes the platform style below, whose default size is in parentheses. A
lead paragraph starts at body size, as it does on a phone-width page.

| Web size token                       | iOS text style     | Material 3 role        |
| ------------------------------------ | ------------------ | ---------------------- |
| `--font-size-3xs`, `--font-size-2xs` | Caption 2 (11pt)   | Label Small (11sp)     |
| `--font-size-xs`, `--font-size-sm`   | Footnote (13pt)    | Body Small (12sp)      |
| `--font-size-md`, `--font-size-lg`   | Subheadline (15pt) | Body Medium (14sp)     |
| `--font-size-body`, `--font-size-xl` | Body (17pt)        | Body Large (16sp)      |
| `--font-size-2xl`                    | Title 3 (20pt)     | Title Large (22sp)     |
| `--font-size-3xl`                    | Title 1 (28pt)     | Headline Medium (28sp) |
| Page titles                          | Large Title (34pt) | Headline Large (32sp)  |

### Touch Targets

Every control's hit area is at least 44×44pt on iOS and 48×48dp on Android. A control
drawn smaller, such as an icon button, reaches it with a larger hit area, not a larger
drawing.

### Actions and Feedback

- **Destructive and irreversible actions,** such as a merge that bypasses its
  requirements or canceling a session, are confirmed in the system alert on iOS and the
  system dialog on Android. The confirm label is a verb that names the outcome, such as
  "Merge Anyway", beside a Cancel button, and neither is the default. The confirm button
  takes the destructive style on iOS and `color.error` text on Android.
- **Haptics:** notification haptics (success, warning, error) mark outcomes, selection
  haptics mark a changing value, and ordinary taps have none. On Android, use the
  system's haptic feedback constants, never raw vibration.
- **Motion** is short and functional, as on the web: Android uses Material's Standard
  motion scheme, not Expressive. When the system asks for reduced motion, the app drops
  zooming, scaling, and repeated motion, including the live statuses' pulse.

### Layout and States

- Respect the safe areas. Backgrounds extend under the bars, and on Android the app
  draws edge to edge, behind the status and navigation bars, and pads its controls by
  the system insets.
- A composer is keyboard-aware: it stays above the keyboard and moves with it.
- A screen shows something at once, cached data or placeholders while content loads, and
  refreshes on its own as well as on a pull.
- An empty section still shows and says why it is empty.
- Offline, the app shows cached data and a quiet offline label, never an alert. Any
  other error it can recover from shows in place, with a way to retry.

### App Icon

- The icon is the brand mark in the Green theme's `color.accent` on its `color.bg`, with
  no text, glows, or drawn shadows.
- On iOS, it is a layered icon whose dark and tinted appearances keep the mark legible.
- On Android, it is an adaptive icon with a monochrome layer, so the system never themes
  it on its own.

### Assistive Technology

- Every control has a label for VoiceOver and TalkBack.
- A status's dot and label are one accessibility element, and so are a row's title and
  its metadata.
- Reading order follows the visual order.
- A streaming transcript is not a live region; the app announces state changes, such as
  asking, ready, or failed, instead.
- Test every screen with VoiceOver and TalkBack, at the largest Dynamic Type size, and
  at 200% Android font scale.

## Do's and Don'ts

- Do build every color, font, radius, shadow, and transition from the custom properties;
  don't write a hex value, `rgb()`, or a named color in a stylesheet.
- Do take sizes, weights, line heights, letter spacing, spacing, column widths, and
  control heights from their scales; don't add a value a hair off a step. A dimension
  that belongs to one element, such as an icon or a sidebar, may stay literal.
- Do change a value only at its token source: regenerate `tokens.css` from it, and let
  each app's build generate its token code from `tokens.json`. Don't edit either output,
  and don't copy a hex value into an app.
- Do keep one primary action per page or app screen; don't fill several buttons with the
  accent.
- Do pair every status color with a label; don't signal state by color alone.
- Do outline form fields with `--color-border-strong`; don't outline them with
  `--color-border`, which is too faint to identify a control.
- Do test both themes and keyboard focus; don't add a light theme without a design for
  every token.
- Do keep motion short, `--transition-fast` for color and small movements, and honor
  reduced motion; don't animate layout or loop decoration other than live status.
- Don't load images, scripts, or fonts from other origins. Fonts come from `fonts.css`;
  the home page's display face is the one exception.
