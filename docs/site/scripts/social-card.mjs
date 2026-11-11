// Regenerate `docs/site/static/social-card.png`, the 1200x630 link preview.
//
// Run from the repository root: `node docs/site/scripts/social-card.mjs`.
//
// The card copies the home page hero text from `templates/index.html`, the
// default theme colors from `sass/site.scss`, the domain from `config.toml`,
// and the glyph art from `static/glyph-art.js`. A pinned Playwright CLI
// captures it with `chrome-headless-shell`. The glyph font loads from Google
// Fonts, so the capture needs network access.
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export const PLAYWRIGHT_VERSION = '1.62.1';
export const CARD_WIDTH = 1200;
export const CARD_HEIGHT = 630;
export const SITE_ROOT = 'docs/site';
export const OUTPUT_PATH = `${SITE_ROOT}/static/social-card.png`;
/** Martian Mono weights the card text and the glyph art draw with. */
export const CARD_FONTS = ['500 16px "Martian Mono"', '700 16px "Martian Mono"', '800 16px "Martian Mono"'];

/** Return the first capture group of `pattern` in `source`, or throw naming `label`. */
function capture(source, pattern, label) {
  const match = source.match(pattern);
  if (!match) throw new Error(`Could not find ${label}`);

  return match[1].trim();
}

/**
 * Read the hero eyebrow, title, and font stylesheet from the home page template.
 *
 * Template expressions are rejected because the card shows the text verbatim.
 */
export function readHero(indexHtml) {
  const hero = {
    eyebrow: capture(indexHtml, /<p class="hero-eyebrow">([^<]*)<\/p>/, 'the hero eyebrow'),
    title: capture(indexHtml, /<h1 class="hero-title"[^>]*>([^<]*)<\/h1>/, 'the hero title'),
    fontHref: capture(indexHtml, /href="(https:\/\/fonts\.googleapis\.com\/[^"]+)"/, 'the font stylesheet'),
  };
  for (const [name, value] of Object.entries(hero)) {
    if (value.includes('{')) throw new Error(`The hero ${name} must be plain text: ${value}`);
  }

  return hero;
}

/** Read the custom properties of the first (default theme) `:root` block. */
export function readThemeColors(siteScss) {
  const block = capture(siteScss, /^:root \{\n([^}]*)\n\}/m, 'the default `:root` theme block');
  const colors = {};
  for (const match of block.matchAll(/^\s*(--(?:color|glyph)-[\w-]+):\s*([^;]+);/gm)) {
    colors[match[1]] = match[2].trim();
  }

  return colors;
}

/** Read the site host, for example `agentty.xyz`, from `base_url`. */
export function readDomain(configToml) {
  return new URL(capture(configToml, /^base_url\s*=\s*"([^"]+)"/m, '`base_url`')).host;
}

/**
 * Build the card page.
 *
 * Reduced motion is forced so `glyph-art.js` draws settled art without the
 * arrival animation or the tail wag. The page sets `body[data-ready]` once
 * both glyph-art boxes are live and painted and every `CARD_FONTS` face has
 * loaded. If a face is missing, the page never sets it, so the capture times
 * out instead of writing a card with fallback lettering.
 */
export function buildCardHtml({ hero, colors, domain, glyphArtSource }) {
  if (glyphArtSource.includes('</script')) throw new Error('`glyph-art.js` cannot be inlined');
  const properties = Object.entries(colors)
    .map(([name, value]) => `${name}: ${value};`)
    .join(' ');

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<link href="${hero.fontHref}" rel="stylesheet">
<style>
:root { ${properties} }
html, body { margin: 0; width: ${CARD_WIDTH}px; height: ${CARD_HEIGHT}px; overflow: hidden; background: var(--color-bg); }
body { position: relative; font-family: "Martian Mono", monospace; }
.glyph-art { position: absolute; }
.glyph-art canvas { position: absolute; inset: 0; width: 100%; height: 100%; }
.wordmark { top: 48px; left: 64px; width: 1072px; height: 224px; }
.nerpa { right: 20px; bottom: 16px; width: 500px; height: 300px; }
.text { position: absolute; left: 64px; bottom: 44px; width: 520px; }
.eyebrow { margin: 0 0 28px; color: var(--color-accent); font-size: 22px; font-weight: 500; }
.title { margin: 0; color: var(--color-text); font-size: 40px; font-weight: 700; letter-spacing: -0.04em; line-height: 1.12; max-width: 460px; }
.domain { margin: 32px 0 0; color: var(--color-muted); font-size: 22px; font-weight: 500; }
</style>
</head>
<body>
<div class="glyph-art wordmark" data-glyph-art="wordmark"><canvas></canvas></div>
<div class="glyph-art nerpa" data-glyph-art="nerpa"><canvas></canvas></div>
<div class="text">
<p class="eyebrow">${hero.eyebrow}</p>
<h1 class="title">${hero.title}</h1>
<p class="domain">${domain}</p>
</div>
<script>
window.matchMedia = function (query) {
  return { matches: /prefers-reduced-motion/.test(query), media: query, addEventListener: function () {}, addListener: function () {} };
};
</script>
<script>${glyphArtSource}</script>
<script>
(async function () {
  var frame = function () { return new Promise(function (resolve) { requestAnimationFrame(resolve); }); };
  var boxes = Array.from(document.querySelectorAll('[data-glyph-art]'));
  while (!boxes.every(function (box) { return box.classList.contains('is-live'); })) await frame();
  var faces = await Promise.all(${JSON.stringify(CARD_FONTS)}.map(function (font) { return document.fonts.load(font); }));
  if (faces.some(function (loaded) { return loaded.length === 0; })) throw new Error('Martian Mono did not load');
  await frame();
  await frame();
  document.body.dataset.ready = 'true';
})();
</script>
</body>
</html>
`;
}

/** Commands, as `[command, args]`, that install the browser and capture `htmlPath`. */
export function captureCommands(htmlPath, outputPath) {
  const playwright = ['--yes', `playwright@${PLAYWRIGHT_VERSION}`];

  return [
    ['npx', [...playwright, 'install', '--only-shell', 'chromium']],
    [
      'npx',
      [
        ...playwright,
        'screenshot',
        '--browser=chromium',
        `--viewport-size=${CARD_WIDTH},${CARD_HEIGHT}`,
        '--color-scheme=dark',
        '--wait-for-selector=body[data-ready]',
        '--timeout=60000',
        pathToFileURL(htmlPath).href,
        outputPath,
      ],
    ],
  ];
}

/**
 * Build the card from the site sources and capture it to `OUTPUT_PATH`.
 *
 * `run` launches each capture command and defaults to `spawnSync`. The
 * temporary card page is removed whether or not the capture succeeds.
 */
export function generateSocialCard(run = spawnSync) {
  const read = (path) => readFileSync(`${SITE_ROOT}/${path}`, 'utf8');
  const html = buildCardHtml({
    hero: readHero(read('templates/index.html')),
    colors: readThemeColors(read('sass/site.scss')),
    domain: readDomain(read('config.toml')),
    glyphArtSource: read('static/glyph-art.js'),
  });
  const workDir = mkdtempSync(join(tmpdir(), 'agentty-social-card-'));
  try {
    const htmlPath = join(workDir, 'card.html');
    writeFileSync(htmlPath, html);
    for (const [command, args] of captureCommands(htmlPath, OUTPUT_PATH)) {
      const result = run(command, args, { stdio: 'inherit' });
      if (result.status !== 0) throw new Error(`\`${command} ${args.join(' ')}\` failed`);
    }
  } finally {
    rmSync(workDir, { recursive: true, force: true });
  }

  return OUTPUT_PATH;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  console.log(`Wrote ${generateSocialCard()}`);
}
