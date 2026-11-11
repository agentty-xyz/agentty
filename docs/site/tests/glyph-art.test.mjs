import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

// Load the browser script without a `document`, so only its pure helpers run.
const source = readFileSync('docs/site/static/glyph-art.js', 'utf8');
const context = {};
context.globalThis = context;
runInNewContext(source, context);
const art = context.AgenttyGlyphArt;
// Helpers return arrays from the script realm; copy them before deep comparisons.

/** Build an RGBA buffer whose red channel follows `valueAt(x, y)`. */
function pixelsFrom(width, height, valueAt) {
  const pixels = new Uint8ClampedArray(width * height * 4);
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) pixels[(y * width + x) * 4] = valueAt(x, y);
  }

  return pixels;
}

test('quantizeLevel keeps near-black cells empty and caps bright cells at 8', () => {
  // Arrange
  const brightness = [0, 0.069, 0.07, 0.5, 0.99, 1];

  // Act
  const levels = brightness.map(art.quantizeLevel);

  // Assert
  assert.deepEqual(levels, [0, 0, 1, 5, 8, 8]);
});

test('sampleLevels averages each cell of the source buffer', () => {
  // Arrange
  const pixels = pixelsFrom(4, 2, (x) => (x < 2 ? 255 : 0));

  // Act
  const levels = [...art.sampleLevels(pixels, 4, 2, 2, 1)];

  // Assert
  assert.deepEqual(levels, ['80']);
});

test('buildCells prints source characters in reading order across lit cells', () => {
  // Arrange
  const levels = ['10', '03'];

  // Act
  const cells = art.buildCells(levels, 'ab');

  // Assert
  assert.deepEqual(
    Array.from(cells, (cell) => [cell.x, cell.y, cell.level, cell.ch]),
    [[0, 0, 1, 'a'], [1, 1, 3, 'b']],
  );
});

test('columnsFor clamps the grid width to the scene limits', () => {
  // Arrange
  const scene = { cellPx: 5, minCols: 10, maxCols: 20 };

  // Act
  const columns = [20, 75, 500].map((width) => art.columnsFor(width, scene));

  // Assert
  assert.deepEqual(columns, [10, 15, 20]);
});

test('heatAround heats nearby cells without wrapping across grid edges', () => {
  // Arrange
  const cols = 4;
  const rows = 3;
  const cells = [];
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) cells.push({ x, y, heat: 0 });
  }
  const byIndex = new Map(cells.map((cell) => [cell.y * cols + cell.x, cell]));

  // Act: point just past the left edge of row 1, then just past the right edge of row 0.
  art.heatAround(byIndex, cols, rows, -1, 1, 2);
  art.heatAround(byIndex, cols, rows, 4, 0, 2);

  // Assert: only the edge cell beside each point heats; no row wraps.
  assert.deepEqual(
    Array.from({ length: rows }, (_, y) => cells.filter((cell) => cell.y === y).map((cell) => cell.heat)),
    [
      [0, 0, 0, 0.5],
      [0.5, 0, 0, 0],
      [0, 0, 0, 0],
    ],
  );
});

test('settle marks every cell as arrived and clears hover heat', () => {
  // Arrange
  const cells = [{ at: 900, heat: 0.8 }, { at: 0, heat: 0 }];

  // Act
  art.settle(cells);

  // Assert
  assert.deepEqual(
    cells.map((cell) => [cell.at, cell.heat]),
    [[-1e6, 0], [-1e6, 0]],
  );
});

test('source text has no whitespace so printed rows read as one stream', () => {
  // Assert
  assert.match(art.SOURCE_TEXT, /^\S+$/);
});

test('addPoses records per-pose levels and adds cells lit only in later poses', () => {
  // Arrange
  const cells = art.buildCells(['10'], 'ab');

  // Act
  art.addPoses(cells, [['10'], ['03']], 2, 'xy');

  // Assert: the original cell keeps its character; the new one starts unlit.
  assert.deepEqual(
    Array.from(cells, (cell) => [cell.x, cell.level, cell.ch, Array.from(cell.levels)]),
    [[0, 1, 'a', [1, 0]], [1, 0, 'y', [0, 3]]],
  );
});

test('poseAt steps through the wag sequence and ends with -1', () => {
  // Arrange
  const wag = { step: 100, sequence: [1, 2, 0] };

  // Act
  const poses = [0, 99, 100, 250, 300].map((elapsed) => art.poseAt(wag, elapsed));

  // Assert
  assert.deepEqual(poses, [1, 1, 2, 0, -1]);
});

test('rotationAbout keeps the pivot fixed and turns other points around it', () => {
  // Arrange
  const m = art.rotationAbout(Math.PI / 2, 10, 20);
  const apply = ([x, y]) => [m.a * x + m.c * y + m.e, m.b * x + m.d * y + m.f].map((v) => Math.round(v));

  // Act
  const points = [[10, 20], [11, 20]].map(apply);

  // Assert
  assert.deepEqual(points, [[10, 20], [10, 21]]);
});

test('nerpa wag only uses defined poses and ends at rest', () => {
  // Arrange
  const { poses, wag } = art.SCENES.nerpa;

  // Assert
  assert.equal(poses[0], 0);
  assert.ok(wag.sequence.every((pose) => pose >= 0 && pose < poses.length));
  assert.equal(wag.sequence.at(-1), 0);
});

/** 2D context stub: records nothing, returns itself from every call, and applies `overrides`. */
function fakeContext(overrides) {
  const ctx = new Proxy({}, {
    get(target, key) {
      if (key in overrides) return overrides[key];
      if (key in target) return target[key];

      return () => ctx;
    },
  });

  return ctx;
}

/**
 * Mount the nerpa scene in a fake browser with a manual clock, timers, and frames.
 *
 * Rasterized poses are fully lit when the hind flippers are turned and dark at
 * rest, so each drawn frame's `fillText` count shows whether the tail is wagging.
 */
async function mountNerpa() {
  let now = 0;
  let turn = 0;
  let drawn = 0;
  let nextFrame = 0;
  const frames = new Map();
  const timers = [];
  const rect = { width: 400, height: 240, top: 0, bottom: 240 };
  const motion = { matches: false, addEventListener: (type, listener) => { motion.listener = listener; } };
  const screen = fakeContext({ fillText: () => { drawn++; } });
  const raster = fakeContext({
    getImageData: (x, y, width, height) => ({ data: new Uint8ClampedArray(width * height * 4).fill(turn ? 255 : 0) }),
  });
  const box = {
    dataset: { glyphArt: 'nerpa' },
    classList: { add: () => {} },
    addEventListener: () => {},
    getBoundingClientRect: () => rect,
    querySelector: () => ({ getContext: () => screen }),
  };
  const documentRef = {
    hidden: false,
    querySelectorAll: () => [box],
    createElement: () => ({ getContext: () => raster }),
  };
  const win = {
    document: documentRef,
    innerHeight: 800,
    performance: { now: () => now },
    matchMedia: () => motion,
    getComputedStyle: () => ({ getPropertyValue: () => '#fff' }),
    addEventListener: () => {},
    setTimeout: (callback) => { timers.push(callback); },
    requestAnimationFrame: (callback) => { frames.set(++nextFrame, callback); return nextFrame; },
    cancelAnimationFrame: (id) => { frames.delete(id); },
    Path2D: class {
      addPath(path, transform) { turn = transform.b; }
    },
  };
  win.globalThis = win;
  runInNewContext(source, win);
  await new Promise((resolve) => setImmediate(resolve));

  const page = {
    document: documentRef,
    frames,
    rect,
    timers,
    advance(ms) { now += ms; },
    fireTimer() { timers.shift()(); },
    /** Run the queued frame and return how many glyphs it drew. */
    runFrame() {
      const [[id, callback]] = frames;
      frames.delete(id);
      drawn = 0;
      callback(now);

      return drawn;
    },
    /** Run frames `stepMs` apart until none are queued; return each frame's glyph count. */
    runFrames(stepMs = 16) {
      const counts = [];
      while (frames.size) {
        if (counts.length) now += stepMs;
        counts.push(page.runFrame());
      }

      return counts;
    },
    /** Switch reduced motion on and return how many glyphs the settled redraw drew. */
    setReducedMotion() {
      motion.matches = true;
      drawn = 0;
      motion.listener();

      return drawn;
    },
  };
  page.runFrames();

  return page;
}

test('nerpa plays its wag on the timer, then rests and queues the next one', async () => {
  // Arrange
  const page = await mountNerpa();
  page.advance(5000);

  // Act
  page.fireTimer();
  const counts = page.runFrames(art.SCENES.nerpa.wag.step);

  // Assert: frames follow the wag sequence (pose 0 draws nothing), then stop.
  assert.deepEqual(
    counts.map((count) => count > 0),
    [...art.SCENES.nerpa.wag.sequence.map((pose) => pose !== 0), false],
  );
  assert.equal(page.frames.size, 0);
  assert.equal(page.timers.length, 1);
});

for (const [condition, apply] of [
  ['the tab is hidden', (page) => { page.document.hidden = true; }],
  ['the art is off screen', (page) => { page.rect.top = 900; page.rect.bottom = 1140; }],
  ['reduced motion is on', (page) => { page.setReducedMotion(); }],
]) {
  test(`nerpa skips its wag and reschedules while ${condition}`, async () => {
    // Arrange
    const page = await mountNerpa();
    apply(page);

    // Act
    page.fireTimer();

    // Assert
    assert.equal(page.frames.size, 0);
    assert.equal(page.timers.length, 1);
  });
}

test('turning reduced motion on mid-wag stops it and redraws the tail at rest', async () => {
  // Arrange
  const page = await mountNerpa();
  page.advance(5000);
  page.fireTimer();
  const wagging = page.runFrame();

  // Act
  const atRest = page.setReducedMotion();

  // Assert
  assert.ok(wagging > 0);
  assert.equal(atRest, 0);
  assert.equal(page.frames.size, 0);
  assert.equal(page.timers.length, 1);
});
