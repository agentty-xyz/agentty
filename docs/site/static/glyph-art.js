// Agentty glyph art: pictures printed in the product's own command lines.
//
// Each `[data-glyph-art]` box names a scene (`wordmark` or `nerpa`). The scene
// is painted in grayscale on an offscreen canvas, sampled into a grid of
// brightness levels (0 = empty, 8 = brightest), and redrawn on a visible
// canvas where every lit cell is one character of `SOURCE_TEXT`. Dim cells use
// smaller, darker glyphs so the picture keeps its tone. Cells arrive top to
// bottom as scrambled noise and scramble again under the pointer.
(function (root) {
  'use strict';

  var SOURCE_TEXT = [
    'agentty session new --agent codex',
    'git worktree add ~/.agentty/wt/5e6bb04f -b wt/5e6bb04f',
    'plan > edit > test > review > merge',
    'git rebase --continue',
    'prek run zola-check',
    'cargo install agentty',
    'gh pr create --draft',
    'status=Review queued=2 done=14',
    'agent=claude agent=gemini agent=codex',
    'diff +42 -7 crates/agentty/src/app/session.rs',
    'commit="Keep session output responsive"',
    'npm install -g agentty',
    'stack=parent>child rebase=auto',
  ].join(' ').replace(/\s+/g, '');
  var NOISE = '!#$%&*+-./:;<=>?@[]^_{|}~0123456789abcdefghijklmnopqrstuvwxyz';
  var CELL_RATIO = 1.85;
  var GLYPH_FONT = '"Martian Mono", "Share Tech Mono", monospace';
  var WORDMARK_FONT = '800 200px "Martian Mono", "Arial Black", sans-serif';
  var SAMPLES_PER_CELL = 4;

  /** Convert one cell's average brightness (`0..1`) into a level (`0..8`). */
  function quantizeLevel(brightness) {
    if (brightness < 0.07) return 0;

    return Math.min(8, 1 + Math.floor(brightness * 8));
  }

  /**
   * Average the red channel of an RGBA buffer into `cols` x `rows` levels.
   * Returns one string per row, with one digit per cell.
   */
  function sampleLevels(pixels, width, height, cols, rows) {
    var levels = [];
    for (var y = 0; y < rows; y++) {
      var top = Math.floor((y * height) / rows);
      var bottom = Math.max(top + 1, Math.floor(((y + 1) * height) / rows));
      var row = '';
      for (var x = 0; x < cols; x++) {
        var left = Math.floor((x * width) / cols);
        var right = Math.max(left + 1, Math.floor(((x + 1) * width) / cols));
        var sum = 0;
        for (var py = top; py < bottom; py++) {
          for (var px = left; px < right; px++) sum += pixels[(py * width + px) * 4];
        }
        row += quantizeLevel(sum / ((bottom - top) * (right - left) * 255));
      }
      levels.push(row);
    }

    return levels;
  }

  /** Assign source characters, in reading order, to every lit cell. */
  function buildCells(levels, text) {
    var cells = [];
    levels.forEach(function (row, y) {
      for (var x = 0; x < row.length; x++) {
        var level = Number(row[x]);
        if (level) cells.push({ x: x, y: y, level: level, ch: text[cells.length % text.length], at: 0, heat: 0 });
      }
    });

    return cells;
  }

  /**
   * Heat cells inside an ellipse around grid point (`gx`, `gy`).
   *
   * The scan stays inside the `cols` x `rows` grid so that `y * cols + x` never
   * wraps into a neighbouring row near the left or right edge.
   */
  function heatAround(byIndex, cols, rows, gx, gy, radius) {
    var top = Math.max(0, Math.floor(gy - radius / 2));
    var bottom = Math.min(rows - 1, gy + radius / 2);
    var left = Math.max(0, Math.floor(gx - radius));
    var right = Math.min(cols - 1, gx + radius);
    for (var y = top; y <= bottom; y++) {
      for (var x = left; x <= right; x++) {
        var cell = byIndex.get(y * cols + x);
        if (!cell) continue;
        var distance = Math.hypot((x - gx) / radius, (y - gy) / (radius / 2));
        if (distance < 1) cell.heat = Math.max(cell.heat, 1 - distance);
      }
    }
  }

  /** Show every cell as settled art: already arrived and with no hover heat. */
  function settle(cells) {
    cells.forEach(function (cell) {
      cell.at = -1e6;
      cell.heat = 0;
    });
  }

  /** Pick a grid width from the box width and the scene's preferred cell size. */
  function columnsFor(boxWidth, scene) {
    return Math.max(scene.minCols, Math.min(scene.maxCols, Math.round(boxWidth / scene.cellPx)));
  }

  function gray(value) {
    var channel = Math.round(value * 255);

    return 'rgb(' + channel + ',' + channel + ',' + channel + ')';
  }

  /** Paint `AGENTTY` in the display face; the scene box hugs the letters. */
  var wordmark = {
    cellPx: 5.2,
    minCols: 120,
    maxCols: 240,
    align: 'center',
    bounds: function (ctx) {
      ctx.font = WORDMARK_FONT;
      var metrics = ctx.measureText('AGENTTY');
      var ascent = metrics.actualBoundingBoxAscent || 150;
      var descent = metrics.actualBoundingBoxDescent || 0;
      var padX = 24;
      var padY = 18;

      return {
        width: metrics.width + padX * 2,
        height: ascent + descent + padY * 2,
        originX: padX,
        baseline: padY + ascent,
      };
    },
    paint: function (ctx, bounds) {
      ctx.font = WORDMARK_FONT;
      ctx.textBaseline = 'alphabetic';
      ctx.fillStyle = '#fff';
      ctx.fillText('AGENTTY', bounds.originX, bounds.baseline);
    },
  };

  /** Paint a Baikal seal resting on an ice floe, facing left. */
  var nerpa = {
    cellPx: 3.4,
    minCols: 76,
    maxCols: 210,
    align: 'end',
    bounds: function () {
      return { width: 1000, height: 600 };
    },
    paint: function (ctx) {
      // Ice floe and water.
      ctx.fillStyle = gray(0.16);
      ctx.beginPath();
      ctx.moveTo(70, 492);
      ctx.lineTo(930, 482);
      ctx.lineTo(985, 532);
      ctx.lineTo(30, 548);
      ctx.closePath();
      ctx.fill();
      ctx.strokeStyle = gray(0.42);
      ctx.lineWidth = 5;
      ctx.beginPath();
      ctx.moveTo(70, 492);
      ctx.lineTo(930, 482);
      ctx.stroke();
      ctx.strokeStyle = gray(0.13);
      ctx.lineWidth = 9;
      [[90, 572, 330], [420, 566, 640], [700, 578, 900], [220, 592, 470]].forEach(function (ripple) {
        ctx.beginPath();
        ctx.moveTo(ripple[0], ripple[1]);
        ctx.lineTo(ripple[2], ripple[1]);
        ctx.stroke();
      });

      var body = new Path2D(
        'M110 300 C112 180 210 118 300 122 C410 128 520 190 640 262 C730 318 820 352 872 372 ' +
        'C910 338 952 300 982 306 C994 332 970 378 930 402 C972 420 994 456 978 480 ' +
        'C950 490 905 468 868 440 C760 488 580 496 440 490 C330 486 230 452 185 400 ' +
        'C160 372 115 345 110 300 Z'
      );
      var shade = ctx.createLinearGradient(0, 120, 0, 492);
      shade.addColorStop(0, gray(0.56));
      shade.addColorStop(1, gray(0.2));
      ctx.fillStyle = shade;
      ctx.fill(body);

      ctx.save();
      ctx.clip(body);
      var sheen = ctx.createRadialGradient(330, 190, 10, 330, 190, 260);
      sheen.addColorStop(0, 'rgba(255,255,255,0.3)');
      sheen.addColorStop(1, 'rgba(255,255,255,0)');
      ctx.fillStyle = sheen;
      ctx.fillRect(0, 0, 1000, 600);
      // Neck crease and a few faint coat spots.
      ctx.strokeStyle = 'rgba(0,0,0,0.28)';
      ctx.lineWidth = 16;
      ctx.beginPath();
      ctx.moveTo(352, 150);
      ctx.quadraticCurveTo(300, 290, 330, 470);
      ctx.stroke();
      ctx.fillStyle = 'rgba(0,0,0,0.2)';
      [[470, 230, 16], [560, 262, 12], [640, 318, 14], [520, 330, 10], [720, 360, 11], [600, 400, 9]].forEach(function (spot) {
        ctx.beginPath();
        ctx.ellipse(spot[0], spot[1], spot[2] * 1.4, spot[2], 0.3, 0, Math.PI * 2);
        ctx.fill();
      });
      ctx.restore();

      ctx.strokeStyle = gray(0.95);
      ctx.lineWidth = 11;
      ctx.stroke(body);

      // Front flipper.
      ctx.save();
      ctx.translate(390, 452);
      ctx.rotate(-0.42);
      ctx.beginPath();
      ctx.ellipse(0, 0, 74, 24, 0, 0, Math.PI * 2);
      ctx.fillStyle = gray(0.58);
      ctx.fill();
      ctx.strokeStyle = gray(0.12);
      ctx.lineWidth = 7;
      ctx.stroke();
      ctx.restore();

      // Muzzle, nose, and mouth.
      ctx.fillStyle = gray(0.88);
      ctx.beginPath();
      ctx.ellipse(152, 306, 50, 34, -0.15, 0, Math.PI * 2);
      ctx.fill();
      ctx.fillStyle = gray(0);
      ctx.beginPath();
      ctx.ellipse(124, 292, 14, 10, -0.2, 0, Math.PI * 2);
      ctx.fill();
      ctx.strokeStyle = gray(0.05);
      ctx.lineWidth = 5;
      ctx.beginPath();
      ctx.moveTo(126, 328);
      ctx.quadraticCurveTo(152, 344, 184, 332);
      ctx.stroke();

      // Large dark eyes with catchlights.
      [[204, 232, 40, 44, 216, 216, 11], [290, 224, 27, 35, 297, 211, 7]].forEach(function (eye) {
        ctx.fillStyle = gray(0);
        ctx.beginPath();
        ctx.ellipse(eye[0], eye[1], eye[2], eye[3], 0, 0, Math.PI * 2);
        ctx.fill();
        ctx.fillStyle = gray(1);
        ctx.beginPath();
        ctx.arc(eye[4], eye[5], eye[6], 0, Math.PI * 2);
        ctx.fill();
      });

      // Whiskers trail past the snout.
      ctx.strokeStyle = gray(1);
      ctx.lineWidth = 6;
      [[140, 302, 30, 276], [140, 311, 22, 314], [146, 319, 38, 352], [154, 326, 74, 384], [178, 318, 252, 350]].forEach(function (whisker) {
        ctx.beginPath();
        ctx.moveTo(whisker[0], whisker[1]);
        ctx.lineTo(whisker[2], whisker[3]);
        ctx.stroke();
      });
    },
  };

  var SCENES = { wordmark: wordmark, nerpa: nerpa };

  /** Render one scene into a level grid sized for `cols` columns. */
  function rasterize(scene, cols, documentRef) {
    var probe = documentRef.createElement('canvas').getContext('2d');
    var bounds = scene.bounds(probe);
    var rows = Math.max(1, Math.round((cols * bounds.height) / bounds.width / CELL_RATIO));
    var canvas = documentRef.createElement('canvas');
    canvas.width = cols * SAMPLES_PER_CELL;
    canvas.height = Math.round(rows * SAMPLES_PER_CELL * CELL_RATIO);
    var ctx = canvas.getContext('2d', { willReadFrequently: true });
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, canvas.width, canvas.height);
    ctx.scale(canvas.width / bounds.width, canvas.height / bounds.height);
    scene.paint(ctx, bounds);
    var pixels = ctx.getImageData(0, 0, canvas.width, canvas.height).data;

    return { cols: cols, rows: rows, levels: sampleLevels(pixels, canvas.width, canvas.height, cols, rows) };
  }

  /** Attach a live renderer to one `[data-glyph-art]` box; returns its starter. */
  function mount(box, scene, win) {
    var documentRef = win.document;
    var canvas = box.querySelector('canvas');
    var ctx = canvas.getContext('2d');
    var motion = win.matchMedia('(prefers-reduced-motion: reduce)');
    var still = motion.matches;
    var grid = null;
    var cells = [];
    var byIndex = new Map();
    var ramp = [];
    var hot = '';
    var cw = 0;
    var ch = 0;
    var ox = 0;
    var oy = 0;
    var fontSize = 0;
    var started = 0;
    var frame = 0;
    var live = false;

    function readColors() {
      var styles = win.getComputedStyle(box);
      ramp = [];
      for (var i = 0; i < 8; i++) ramp.push(styles.getPropertyValue('--glyph-' + i).trim());
      hot = styles.getPropertyValue('--glyph-hot').trim();
    }

    function setFont(level) {
      var weight = level > 6 ? 700 : 500;
      var scale = 0.62 + 0.38 * Math.min(1, level / 6);
      ctx.font = weight + ' ' + (scale * fontSize).toFixed(2) + 'px ' + GLYPH_FONT;
    }

    function rebuild(width) {
      var cols = columnsFor(width, scene);
      if (grid && grid.cols === cols) return false;
      grid = rasterize(scene, cols, documentRef);
      cells = buildCells(grid.levels, SOURCE_TEXT);
      byIndex = new Map(cells.map(function (cell) { return [cell.y * grid.cols + cell.x, cell]; }));

      return true;
    }

    function arrive(now) {
      started = now;
      if (still) {
        settle(cells);
        return;
      }
      cells.forEach(function (cell) {
        cell.at = (cell.y * 1100) / grid.rows + Math.random() * 420;
      });
    }

    function size() {
      var rect = box.getBoundingClientRect();
      var dpr = Math.min(win.devicePixelRatio || 1, 2);
      var rebuilt = rebuild(rect.width);
      cw = Math.min(rect.width / grid.cols, rect.height / (grid.rows * CELL_RATIO));
      ch = cw * CELL_RATIO;
      fontSize = cw * 1.62;
      ox = scene.align === 'end' ? rect.width - cw * grid.cols : (rect.width - cw * grid.cols) / 2;
      oy = scene.align === 'end' ? rect.height - ch * grid.rows : (rect.height - ch * grid.rows) / 2;
      canvas.width = Math.round(rect.width * dpr);
      canvas.height = Math.round(rect.height * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';

      return rebuilt;
    }

    function draw(now) {
      frame = 0;
      var elapsed = now - started;
      var busy = false;
      var lastLevel = -1;
      ctx.clearRect(0, 0, canvas.width, canvas.height);
      cells.forEach(function (cell) {
        var px = ox + cell.x * cw + cw / 2;
        var py = oy + cell.y * ch + ch / 2;
        if (elapsed < cell.at) {
          busy = true;
          return;
        }
        var arriving = elapsed < cell.at + 260;
        var level = cell.heat > 0.04 || arriving ? 8 : cell.level;
        if (level !== lastLevel) {
          setFont(level);
          lastLevel = level;
        }
        if (arriving) {
          busy = true;
          ctx.fillStyle = ramp[Math.min(7, cell.level)];
          ctx.fillText(NOISE[(Math.random() * NOISE.length) | 0], px, py);
          return;
        }
        if (cell.heat > 0.04) {
          busy = true;
          ctx.fillStyle = cell.heat > 0.5 ? hot : ramp[Math.min(7, cell.level + 1)];
          ctx.fillText(cell.heat > 0.25 ? NOISE[(Math.random() * NOISE.length) | 0] : cell.ch, px, py);
          cell.heat *= 0.9;
          return;
        }
        cell.heat = 0;
        ctx.fillStyle = ramp[cell.level - 1];
        ctx.fillText(cell.ch, px, py);
      });
      if (busy) frame = win.requestAnimationFrame(draw);
    }

    function kick() {
      if (!frame) frame = win.requestAnimationFrame(draw);
    }

    function start() {
      if (live) return;
      live = true;
      readColors();
      size();
      box.classList.add('is-live');
      arrive(win.performance.now());
      kick();
    }

    if ('ResizeObserver' in win) {
      new win.ResizeObserver(function () {
        if (!live) return;
        if (size()) arrive(win.performance.now());
        kick();
      }).observe(box);
    }
    win.addEventListener('agentty-themechange', function () {
      if (!live) return;
      readColors();
      kick();
    });
    box.addEventListener('pointermove', function (event) {
      if (!live || still) return;
      var rect = canvas.getBoundingClientRect();
      var gx = (event.clientX - rect.left - ox) / cw;
      var gy = (event.clientY - rect.top - oy) / ch;
      heatAround(byIndex, grid.cols, grid.rows, gx, gy, 7);
      kick();
    });

    // Follow motion-preference changes without a reload; turning reduced motion
    // on stops any running animation and draws the settled art.
    function onMotionChange() {
      still = motion.matches;
      if (!still || !live) return;
      if (frame) win.cancelAnimationFrame(frame);
      settle(cells);
      draw(win.performance.now());
    }
    if (motion.addEventListener) motion.addEventListener('change', onMotionChange);
    else if (motion.addListener) motion.addListener(onMotionChange);

    return start;
  }

  /** Mount every glyph-art box once the glyph font is ready and the box is visible. */
  function init(win) {
    var documentRef = win.document;
    var fontsReady = documentRef.fonts
      ? Promise.all([
        documentRef.fonts.load('500 12px "Martian Mono"'),
        documentRef.fonts.load(WORDMARK_FONT),
      ])
      : Promise.resolve();
    documentRef.querySelectorAll('[data-glyph-art]').forEach(function (box) {
      var scene = SCENES[box.dataset.glyphArt];
      if (!scene) return;
      var start = mount(box, scene, win);
      var whenReady = function () { fontsReady.then(start, start); };
      if (!('IntersectionObserver' in win)) {
        whenReady();
        return;
      }
      new win.IntersectionObserver(function (entries, observer) {
        if (!entries[0].isIntersecting) return;
        observer.disconnect();
        whenReady();
      }, { threshold: 0.2 }).observe(box);
    });
  }

  root.AgenttyGlyphArt = {
    SOURCE_TEXT: SOURCE_TEXT,
    buildCells: buildCells,
    columnsFor: columnsFor,
    heatAround: heatAround,
    quantizeLevel: quantizeLevel,
    sampleLevels: sampleLevels,
    settle: settle,
  };
  if (root.document) init(root);
})(globalThis);
