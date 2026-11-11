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
