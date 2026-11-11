import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

import {
  CARD_FONTS,
  CARD_HEIGHT,
  CARD_WIDTH,
  OUTPUT_PATH,
  PLAYWRIGHT_VERSION,
  buildCardHtml,
  captureCommands,
  generateSocialCard,
  readDomain,
  readHero,
  readThemeColors,
} from '../scripts/social-card.mjs';

const FONT_LINK = '<link href="https://fonts.googleapis.com/css2?family=Martian+Mono&display=swap" rel="stylesheet">';

/** Card inputs with a small stand-in for `glyph-art.js`. */
function cardInputs(overrides = {}) {
  return {
    hero: { eyebrow: '// eyebrow', title: 'Title text.', fontHref: 'https://fonts.example/css' },
    colors: { '--color-bg': '#000000', '--glyph-0': '#111111' },
    domain: 'agentty.xyz',
    glyphArtSource: 'globalThis.glyphArtLoaded = true;',
    ...overrides,
  };
}

test('readHero reads the eyebrow, title, and font stylesheet of the home page', () => {
  // Arrange
  const indexHtml = readFileSync('docs/site/templates/index.html', 'utf8');

  // Act
  const hero = readHero(indexHtml);

  // Assert
  assert.equal(hero.eyebrow, '// terminal-first agent workflows');
  assert.equal(hero.title, 'Ship AI-assisted changes with control.');
  assert.match(hero.fontHref, /^https:\/\/fonts\.googleapis\.com\/css2\?family=Martian\+Mono/);
});

test('readHero rejects a template without a hero title', () => {
  // Arrange
  const indexHtml = `${FONT_LINK}<p class="hero-eyebrow">// eyebrow</p>`;

  // Act
  const read = () => readHero(indexHtml);

  // Assert
  assert.throws(read, /Could not find the hero title/);
});

test('readHero rejects hero text that comes from a template expression', () => {
  // Arrange
  const indexHtml = `${FONT_LINK}<p class="hero-eyebrow">// eyebrow</p>`
    + '<h1 class="hero-title" id="t">{{ section.title }}</h1>';

  // Act
  const read = () => readHero(indexHtml);

  // Assert
  assert.throws(read, /The hero title must be plain text/);
});

test('readThemeColors keeps only the default theme color and glyph properties', () => {
  // Arrange
  const siteScss = [
    ':root {',
    '    --color-bg: #070b08;',
    '    --glyph-hot: #4ae477;',
    '    --font-mono: monospace;',
    '}',
    '',
    ':root.theme-dark {',
    '    --color-bg: #080808;',
    '}',
  ].join('\n');

  // Act
  const colors = readThemeColors(siteScss);

  // Assert
  assert.deepEqual(colors, { '--color-bg': '#070b08', '--glyph-hot': '#4ae477' });
});

test('readThemeColors reads every glyph level from the site stylesheet', () => {
  // Arrange
  const siteScss = readFileSync('docs/site/sass/site.scss', 'utf8');

  // Act
  const colors = readThemeColors(siteScss);

  // Assert
  for (let level = 0; level < 8; level++) assert.ok(colors[`--glyph-${level}`], `--glyph-${level}`);
  assert.ok(colors['--color-bg']);
});

test('readDomain returns the host of the site base URL', () => {
  // Arrange
  const configToml = 'title = "Agentty"\nbase_url = "https://agentty.xyz"\n';

  // Act
  const domain = readDomain(configToml);

  // Assert
  assert.equal(domain, 'agentty.xyz');
});

test('buildCardHtml places the hero text, colors, and glyph art on a card-sized page', () => {
  // Arrange
  const inputs = cardInputs();

  // Act
  const html = buildCardHtml(inputs);

  // Assert
  assert.ok(html.includes(`width: ${CARD_WIDTH}px; height: ${CARD_HEIGHT}px;`));
  assert.ok(html.includes(':root { --color-bg: #000000; --glyph-0: #111111; }'));
  assert.ok(html.includes('<p class="eyebrow">// eyebrow</p>'));
  assert.ok(html.includes('<h1 class="title">Title text.</h1>'));
  assert.ok(html.includes('<p class="domain">agentty.xyz</p>'));
  assert.ok(html.includes('<link href="https://fonts.example/css" rel="stylesheet">'));
  assert.ok(html.includes('<script>globalThis.glyphArtLoaded = true;</script>'));
  assert.ok(html.indexOf('window.matchMedia') < html.indexOf('glyphArtLoaded'));
  assert.ok(html.includes("document.body.dataset.ready = 'true'"));
});

/** Run the card's readiness script against live boxes and a stubbed `document.fonts.load`. */
async function runReadyScript(loadFont) {
  const scripts = [...buildCardHtml(cardInputs()).matchAll(/<script>([\s\S]*?)<\/script>/g)];
  const box = { classList: { contains: (name) => name === 'is-live' } };
  const loadedFonts = [];
  const document = {
    body: { dataset: {} },
    fonts: {
      load: async (font) => {
        loadedFonts.push(font);

        return loadFont(font);
      },
    },
    querySelectorAll: () => [box, box],
  };
  const context = vm.createContext({
    Array,
    Promise,
    document,
    requestAnimationFrame: (callback) => setImmediate(callback),
  });
  const ready = vm.runInContext(scripts.at(-1)[1], context);
  const outcome = await ready.then(() => null, (error) => error);

  return { dataset: document.body.dataset, loadedFonts, outcome };
}

test('the card page marks itself ready once every Martian Mono weight loads', async () => {
  // Arrange
  const loadFont = () => [{ family: 'Martian Mono' }];

  // Act
  const { dataset, loadedFonts, outcome } = await runReadyScript(loadFont);

  // Assert
  assert.equal(outcome, null);
  assert.deepEqual(loadedFonts, CARD_FONTS);
  assert.equal(dataset.ready, 'true');
});

test('the card page never marks itself ready when Martian Mono is missing', async () => {
  // Arrange
  const loadFont = (font) => (font.startsWith('700') ? [] : [{ family: 'Martian Mono' }]);

  // Act
  const { dataset, outcome } = await runReadyScript(loadFont);

  // Assert
  assert.match(outcome.message, /Martian Mono did not load/);
  assert.equal(dataset.ready, undefined);
});

test('buildCardHtml refuses glyph-art source that would close the inline script', () => {
  // Arrange
  const inputs = cardInputs({ glyphArtSource: 'var s = "</script>";' });

  // Act
  const build = () => buildCardHtml(inputs);

  // Assert
  assert.throws(build, /cannot be inlined/);
});

test('captureCommands installs the pinned headless shell and then captures the card', () => {
  // Arrange
  const htmlPath = '/tmp/card dir/card.html';

  // Act
  const commands = captureCommands(htmlPath, OUTPUT_PATH);

  // Assert
  const playwright = ['--yes', `playwright@${PLAYWRIGHT_VERSION}`];
  assert.deepEqual(commands, [
    ['npx', [...playwright, 'install', '--only-shell', 'chromium']],
    [
      'npx',
      [
        ...playwright,
        'screenshot',
        '--browser=chromium',
        '--viewport-size=1200,630',
        '--color-scheme=dark',
        '--wait-for-selector=body[data-ready]',
        '--timeout=60000',
        'file:///tmp/card%20dir/card.html',
        'docs/site/static/social-card.png',
      ],
    ],
  ]);
});

test('generateSocialCard captures the site-derived card page and then removes it', () => {
  // Arrange
  const calls = [];
  const run = (command, args) => {
    const cardPath = fileURLToPath(args.find((arg) => arg.startsWith('file://')) ?? 'file:///none');
    calls.push({ command, args, card: existsSync(cardPath) ? readFileSync(cardPath, 'utf8') : null, cardPath });

    return { status: 0 };
  };

  // Act
  const outputPath = generateSocialCard(run);

  // Assert
  assert.equal(outputPath, OUTPUT_PATH);
  assert.deepEqual(calls.map((call) => call.args[2]), ['install', 'screenshot']);
  const capture = calls[1];
  assert.ok(capture.card.includes('<h1 class="title">Ship AI-assisted changes with control.</h1>'));
  assert.ok(capture.card.includes('<p class="domain">agentty.xyz</p>'));
  assert.ok(capture.card.includes('AgenttyGlyphArt'));
  assert.equal(existsSync(capture.cardPath), false);
});

test('generateSocialCard stops at a failed command and still removes the card page', () => {
  // Arrange
  const calls = [];
  const run = (command, args) => {
    calls.push(args);

    return { status: 1 };
  };

  // Act
  const generate = () => generateSocialCard(run);

  // Assert
  assert.throws(generate, /install --only-shell chromium` failed/);
  assert.equal(calls.length, 1);
});
