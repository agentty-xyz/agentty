import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

import {
  Browser,
  LAYOUT_PROPERTIES,
  Page,
  SELECTORS,
  auditRules,
  capture,
  check,
  collectPage,
  companionClasses,
  compareStyles,
  findChrome,
  formatReport,
  galleryPage,
  galleryStylesheets,
  inSequence,
  main,
  parseArguments,
  readMarkup,
  selectorParts,
  stateChanges,
} from "../gallery-check.mjs";
import { FakeElement, FakePage, fakeBrowser, fakeProcess, fakeSheet, fakeWindow, styleRule } from "./support.mjs";

const SCRIPT = fileURLToPath(new URL("../gallery-check.mjs", import.meta.url));
const VIEWPORT = { height: 900, mobile: false, name: "desktop", width: 1280 };

// One specimen: a link holding a badge, and an input; plus an element
// outside every specimen, which the check ignores.
function specimenBody(badgeColor = "green", hoverColor = "focus") {
  const body = new FakeElement("body");
  const root = new FakeElement("div", { dataset: { specimen: "badge" } });
  const link = new FakeElement("a", {
    selectors: [SELECTORS.hover, SELECTORS.focus],
    states: { hover: { color: hoverColor } },
    styles: { color: "accent", "margin-top": "0px", "transition-duration": "0.14s" },
  });
  const badge = new FakeElement("span", {
    classes: ["badge"],
    pseudo: { "::before": { color: "dim", content: '"#"' } },
    states: { hover: { "border-color": "accent-dim" } },
    styles: { "border-color": "border", color: badgeColor, "--status-tone": "x" },
  });
  const input = new FakeElement("input", {
    pseudo: { "::placeholder": { color: "dim" } },
    selectors: [SELECTORS.hover, SELECTORS.focus],
    states: { "focus-visible": { outline: "2px" } },
    styles: { color: "text" },
  });
  root.append(link.append(badge), input);

  return body.append(root, new FakeElement("p", { styles: { color: "outside" } }));
}

// A page that renders the reference, or, when the page's stylesheet holds
// `BAD`, a surface that recolours the badge with a rule of its own.
function galleryWindows() {
  return new FakePage((html) => {
    const isBad = html.includes("BAD");
    const rules = [styleRule(".badge", ["color", "border-color"])];
    if (isBad) {
      rules.push(styleRule(".site .badge", ["color"]));
    }

    return fakeWindow(specimenBody(isBad ? "red" : "green", isBad ? "error" : "focus"), [fakeSheet(rules)]);
  });
}

const GALLERY =
  '<!DOCTYPE html><html lang="en"><head><link rel="stylesheet" href="tokens.css">' +
  '<link rel="icon" href="favicon.ico"><link rel="stylesheet" href="base.css"></head><body></body></html>';

test("parseArguments reads the gallery and each surface's stylesheets and markup", () => {
  const argv = ["gallery.html", "--surface", "site=a.css,b.css", "--markup", "site=templates,app.js", "--surface", "app=c.css"];

  assert.deepEqual(parseArguments(argv), {
    gallery: "gallery.html",
    surfaces: [
      { files: ["a.css", "b.css"], markup: ["templates", "app.js"], name: "site" },
      { files: ["c.css"], markup: [], name: "app" },
    ],
  });
});

test("parseArguments rejects a missing gallery, a malformed option, no surface, and stray markup", () => {
  assert.throws(() => parseArguments([]), /usage/);
  assert.throws(() => parseArguments(["--surface", "a=b.css"]), /usage/);
  assert.throws(() => parseArguments(["gallery.html", "--other", "a=b.css"]), /expected --surface or --markup/);
  assert.throws(() => parseArguments(["gallery.html", "--surface", "=b.css"]), /expected --surface or --markup/);
  assert.throws(() => parseArguments(["gallery.html", "--surface", "a="]), /expected --surface or --markup/);
  assert.throws(() => parseArguments(["gallery.html", "--surface"]), /expected --surface or --markup/);
  assert.throws(() => parseArguments(["gallery.html"]), /at least one --surface/);
  assert.throws(
    () => parseArguments(["gallery.html", "--surface", "a=b.css", "--markup", "c=d.html"]),
    /--markup names c, which no --surface names/,
  );
});

test("readMarkup reads a file, or the templates and scripts under a directory in name order", () => {
  const directory = mkdtempSync(join(tmpdir(), "gallery-check-test-"));
  try {
    mkdirSync(join(directory, "partials"));
    writeFileSync(join(directory, "b.html"), "b");
    writeFileSync(join(directory, "a.mjs"), "a");
    writeFileSync(join(directory, "site.css"), "css");
    writeFileSync(join(directory, "partials", "c.htm"), "c");

    assert.deepEqual(readMarkup(directory), ["a", "b", "c"]);
    assert.deepEqual(readMarkup(join(directory, "site.css")), ["css"]);
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("companionClasses finds the classes quoted beside a shared class, outside comments", () => {
  const component = (name) => (["status", "badge"].includes(name.split("--")[0]) ? name.split("--")[0] : null);
  const markup = [
    '<span class="badge github-badge">x</span>\n<!-- "badge in-comment" -->',
    "/* 'status in-block' */\n  // \"status in-line\"\nnode.className = 'status status--line status-line status--' + tone;",
    "const label = `status narrow-only`; const text = \"the status of $1\";",
    '<i class="badge kept"></i> /* an unclosed comment runs to the end: "badge cut"',
  ];

  assert.deepEqual(
    [...companionClasses(markup, component)],
    [
      ["github-badge", ".badge"],
      ["status-line", ".status"],
      ["narrow-only", ".status"],
      ["the", ".status"],
      ["of", ".status"],
      ["kept", ".badge"],
    ],
  );
});

test("findChrome prefers CHROME_PATH and otherwise the newest headless shell or an installed browser", () => {
  const cache = join("/home", "Library/Caches/ms-playwright");
  const listing = () => ["chromium-1234", "chromium_headless_shell-1228", "chromium_headless_shell-1234"];
  const newest = join(cache, "chromium_headless_shell-1234", "chrome-headless-shell-mac-arm64", "chrome-headless-shell");
  const older = join(cache, "chromium_headless_shell-1228", "chrome-headless-shell-mac-arm64", "chrome-headless-shell");
  const found = (paths) => (path) => paths.includes(path);

  assert.equal(findChrome({ CHROME_PATH: "/opt/chrome" }, "linux", found(["/opt/chrome"]), listing), "/opt/chrome");
  assert.equal(findChrome({ CHROME_PATH: "/opt/chrome" }, "linux", found([]), listing), null);
  assert.equal(findChrome({ HOME: "/home" }, "darwin", found([newest, older]), listing), newest);
  assert.equal(findChrome({ HOME: "/home" }, "darwin", found([older]), listing), older);
  const unreadable = () => {
    throw new Error("no cache");
  };
  const chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
  assert.equal(findChrome({}, "darwin", found([chrome]), unreadable), chrome);
  assert.equal(findChrome({}, "linux", found(["/usr/bin/chromium"]), unreadable), "/usr/bin/chromium");
  const shell = join("/home", ".cache/ms-playwright", "chromium_headless_shell-1234", "chrome-headless-shell-linux64", "chrome-headless-shell");
  assert.equal(findChrome({ HOME: "/home" }, "linux", found([shell]), listing), shell);
  assert.equal(findChrome({}, "linux", found([]), unreadable), null);
});

test("galleryStylesheets lists the linked stylesheets in order and requires one", () => {
  assert.deepEqual(galleryStylesheets(GALLERY), ["tokens.css", "base.css"]);
  assert.throws(() => galleryStylesheets('<link rel="stylesheet"><link rel="icon" href="x">'), /links no stylesheet/);
  assert.throws(() => galleryStylesheets("<p>No links at all.</p>"), /links no stylesheet/);
});

test("inSequence runs each step only after the previous one settles, and keeps their results in order", async () => {
  const events = [];
  const step = async (item) => {
    events.push("start " + item);
    // The first step takes longest, so overlapping steps would interleave.
    await new Promise((resolve) => setTimeout(resolve, 3 - item));
    events.push("end " + item);

    return item * 10;
  };

  assert.deepEqual(await inSequence([1, 2, 3], step), [10, 20, 30]);
  assert.deepEqual(events, ["start 1", "end 1", "start 2", "end 2", "start 3", "end 3"]);
  assert.deepEqual(await inSequence([], step), []);
  await assert.rejects(inSequence([1], () => Promise.reject(new Error("stop"))), /stop/);
});

test("stateChanges keeps what a state changes, and every style of an element only the state shows", () => {
  const rest = { a: { color: "red", width: "1px" } };
  const forced = { a: { color: "blue", width: "1px" }, "a::after": { content: '"!"' } };

  assert.deepEqual(stateChanges(rest, forced, "hover a | "), {
    "hover a | a": { color: "blue" },
    "hover a | a::after": { content: '"!"' },
  });
  assert.deepEqual(stateChanges(rest, { a: rest.a }, "hover a | "), {});
});

test("galleryPage inlines the stylesheet chain and sets the theme on the root element", () => {
  const page = galleryPage(GALLERY, "a{}", "theme-dark");

  assert.equal(page.includes('rel="stylesheet"'), false);
  assert.ok(page.includes('<head><style data-gallery-chain>a{}</style><link rel="icon"'));
  assert.ok(page.includes('<html class="theme-dark" lang="en">'));
  assert.ok(galleryPage(GALLERY, "", "").includes('<html lang="en">'));
});

test("collectPage lists specimen elements with their targets and ancestors", () => {
  const window = fakeWindow(specimenBody());

  const list = collectPage(window, { focus: SELECTORS.focus, hover: SELECTORS.hover, mode: "list", selector: SELECTORS.specimens });

  assert.deepEqual(
    list.map((element) => [element.key, element.root, element.hover, element.focus, element.ancestors, element.specimen]),
    [
      ["badge", true, false, false, [], 0],
      ["badge > a:0", false, true, true, [], 0],
      ["badge > a:0 > span.badge:0", false, false, false, [1], 0],
      ["badge > input:1", false, true, true, [], 0],
    ],
  );
});

test("collectPage reads computed styles without layout, custom, or motion properties, or only motion", () => {
  const window = fakeWindow(specimenBody());
  const options = { layout: [...LAYOUT_PROPERTIES], mode: "styles", selector: SELECTORS.specimens };

  const rest = collectPage(window, { ...options, motion: false });
  const motion = collectPage(window, { ...options, motion: true });
  const subtree = collectPage(window, { ...options, motion: false, root: 1 });

  assert.deepEqual(rest, {
    "badge > a:0": { color: "accent" },
    "badge > a:0 > span.badge:0": { "border-color": "border", color: "green" },
    "badge > a:0 > span.badge:0::before": { color: "dim", content: '"#"' },
    "badge > input:1": { color: "text" },
    "badge > input:1::placeholder": { color: "dim" },
  });
  assert.deepEqual(motion["badge > a:0"], { "transition-duration": "0.14s" });
  assert.deepEqual(Object.keys(subtree), ["badge > a:0", "badge > a:0 > span.badge:0", "badge > a:0 > span.badge:0::before"]);
});

test("collectPage freezes motion with a style element and reads only the chain's rules", () => {
  const window = fakeWindow(specimenBody(), [
    fakeSheet([
      styleRule(".badge", ["color"]),
      { conditionText: "(max-width: 560px)", cssRules: [styleRule(".badge", ["padding-top"])], media: {} },
      { cssRules: [styleRule(".status", ["color"])], name: "base" },
      { conditionText: "(display: grid)", cssRules: [styleRule(".grid", ["display"])] },
      { conditionText: "(width > 1px)", containerName: "", cssRules: [styleRule(".card", ["color"])] },
      { keyText: "50%" },
    ]),
    fakeSheet([styleRule(".gallery", ["color"])], false),
  ]);

  assert.equal(collectPage(window, { css: "* {}", mode: "freeze" }), true);
  assert.deepEqual(window.document.head.appended, [{ localName: "style", textContent: "* {}" }]);
  assert.deepEqual(
    collectPage(window, { mode: "rules" }).map((rule) => [rule.text, rule.hidden]),
    [
      [".badge {color}", false],
      ["@media (max-width: 560px) .badge {padding-top}", false],
      ["@layer base .status {color}", false],
      ["@supports (display: grid) .grid {display}", false],
      ["@container (width > 1px) .card {color}", false],
    ],
  );
  const hiding = fakeWindow(specimenBody(), [fakeSheet([styleRule(".wide-only", ["display"], { display: "none" })])]);
  assert.equal(collectPage(hiding, { mode: "rules" })[0].hidden, true);
});

test("compareStyles reports changed, missing, and extra properties and elements", () => {
  const differences = compareStyles(
    { a: { color: "red", width: "1px" }, gone: { color: "red" } },
    { a: { color: "blue", width: "1px", height: "2px" }, extra: { color: "red" } },
    "green, desktop",
  );

  assert.deepEqual(differences, [
    { actual: "blue", expected: "red", key: "a", property: "color", variant: "green, desktop" },
    { actual: "2px", expected: undefined, key: "a", property: "height", variant: "green, desktop" },
    { actual: undefined, expected: "red", key: "gone", property: "color", variant: "green, desktop" },
    { actual: "present", expected: "absent", key: "extra", property: "(element)", variant: "green, desktop" },
  ]);
});

test("selectorParts finds each subject's classes and types, and the classes before it", () => {
  assert.deepEqual(selectorParts(".prose :not(pre) > code:not([class]), a:hover .badge::before"), [
    { context: ["prose"], subject: { classes: [], root: false, types: ["code"] } },
    { context: [], subject: { classes: ["badge"], root: false, types: [] } },
  ]);
  assert.deepEqual(selectorParts(':where(a, .x button):focus-visible, [data-x="a b"] .y, :root.theme-dark'), [
    { context: [], subject: { classes: [], root: false, types: ["a"] } },
    { context: ["x"], subject: { classes: [], root: false, types: ["button"] } },
    { context: [], subject: { classes: ["y"], root: false, types: [] } },
    { context: [], subject: { classes: ["theme-dark"], root: true, types: [] } },
  ]);
  assert.deepEqual(selectorParts(".a:is(:not(.b), .c)"), [
    { context: [], subject: { classes: ["a"], root: false, types: [] } },
    { context: [], subject: { classes: ["a", "c"], root: false, types: [] } },
  ]);
  // An empty selector after a trailing comma names nothing, and a
  // parenthesis without a pseudo-class name opens no argument by name.
  assert.deepEqual(selectorParts(".a,"), [
    { context: [], subject: { classes: ["a"], root: false, types: [] } },
    { context: [], subject: { classes: [], root: false, types: [] } },
  ]);
  assert.deepEqual(selectorParts("(.b)"), [{ context: [], subject: { classes: [], root: false, types: [] } }]);
});

test("auditRules reports a surface's own rules that restyle a shared component", () => {
  const rule = (selector, declarations, conditions = [], hidden = false) => ({
    conditions: conditions,
    declarations: declarations,
    hidden: hidden,
    selector: selector,
    text: conditions.concat(selector, "{" + declarations.join(";") + "}").join(" "),
  });
  const reference = [
    rule(":root.theme-dark", ["--color-bg"]),
    rule(":where(a, button):focus-visible", ["outline-color"]),
    rule(".badge", ["color"]),
    rule(".prose table", ["width"]),
    rule(".table th, .prose table th", ["color"]),
    rule(".segmented", ["display"]),
    rule(".segmented button", ["color"]),
    rule(".field-prompt", ["color"]),
    rule(".field-prompt--code input", ["font-family"]),
  ];
  const surface = [
    rule(".badge", ["color"]),
    rule(".segmented button", ["color"]),
    rule(".site .badge", ["margin-top", "width"]),
    rule(".site .badge", ["color"], ["@media (max-width: 560px)"]),
    rule(".badge--pair", ["color"]),
    rule(":is(.other, .badge)", ["color"]),
    rule(".docs table th", ["padding-top"]),
    rule(".segmented button", ["padding-top"]),
    rule(".field-prompt--code input", ["letter-spacing"]),
    rule(".chips button, .filter input", ["color"]),
    rule(":root.theme-dark", ["--color-demo"]),
    rule(".x:not(.badge)", ["color"]),
    rule(".status-line", ["font-size", "margin-top"]),
    rule(".wide-only", ["display"], ["@media (max-width: 860px)"], true),
    rule(".wide-only", ["display"]),
  ];
  const markup = ['<span class="badge status-line wide-only">'];

  assert.deepEqual(auditRules(reference, surface).length, 6);
  assert.deepEqual(auditRules(reference, surface, markup).slice(6), [
    { components: [".status-line (with .badge)"], conditions: [], properties: ["font-size"], selector: ".status-line" },
    { components: [".wide-only (with .badge)"], conditions: [], properties: ["display"], selector: ".wide-only" },
  ]);
  assert.deepEqual(auditRules(reference, surface), [
    { components: [".badge"], conditions: ["@media (max-width: 560px)"], properties: ["color"], selector: ".site .badge" },
    { components: [".badge--pair"], conditions: [], properties: ["color"], selector: ".badge--pair" },
    { components: [".badge"], conditions: [], properties: ["color"], selector: ":is(.other, .badge)" },
    { components: ["th"], conditions: [], properties: ["padding-top"], selector: ".docs table th" },
    { components: [".segmented button"], conditions: [], properties: ["padding-top"], selector: ".segmented button" },
    { components: [".field-prompt--code input"], conditions: [], properties: ["letter-spacing"], selector: ".field-prompt--code input" },
  ]);
});

test("formatReport summarizes a pass and lists a bounded number of failures", () => {
  const surfaces = [{ differences: [], findings: [], name: "site" }];
  assert.deepEqual(formatReport({ captures: 8, passed: true, specimens: 3, surfaces: surfaces }), [
    "Gallery check: site render all 3 specimens as the gallery does (8 renders).",
  ]);
  const difference = { actual: "red", expected: "green", key: "badge > span:0", property: "color", variant: "dark, phone" };
  const failing = {
    captures: 8,
    passed: false,
    specimens: 3,
    surfaces: [
      {
        differences: [
          difference,
          { actual: undefined, expected: "red", key: "hover a:0 | a:0", property: "color", variant: "green, desktop" },
          { actual: "red", expected: undefined, key: "hover a:0 | a:0", property: "border-color", variant: "green, desktop" },
        ],
        findings: [
          { components: [".badge"], conditions: [], properties: ["color"], selector: ".site .badge" },
          { components: ["td"], conditions: ["@media (max-width: 560px)"], properties: ["padding-top", "color"], selector: ".docs td" },
        ],
        name: "site",
      },
    ],
  };
  assert.deepEqual(formatReport(failing), [
    "Gallery check: a surface renders a shared component differently from the gallery.",
    "site: .site .badge restyles .badge with color",
    "site: @media (max-width: 560px) .docs td restyles td with padding-top, color",
    "site [dark, phone] badge > span:0: color is red, gallery green",
    "site [green, desktop] hover a:0 | a:0: color is as at rest, gallery red",
    "site [green, desktop] hover a:0 | a:0: border-color is red, gallery as at rest",
  ]);
  failing.surfaces[0].differences = Array.from({ length: 205 }, () => difference);
  const lines = formatReport(failing);
  assert.equal(lines.length, 202);
  assert.equal(lines.at(-1), "... and 7 more");
});

test("check renders the reference and each surface in every theme and viewport, at rest and in each state", async () => {
  const page = galleryWindows();

  const result = await check(fakeBrowser(page), { gallery: GALLERY, reference: "", surfaces: [{ css: "", name: "site" }] });

  assert.deepEqual(result, {
    captures: 8,
    passed: true,
    specimens: 1,
    surfaces: [{ differences: [], findings: [], name: "site" }],
  });
  assert.deepEqual(
    page.loads.map((load) => [load.html.includes('class="theme-dark"'), load.viewport.name]),
    [
      [false, "desktop"],
      [false, "desktop"],
      [false, "phone"],
      [false, "phone"],
      [true, "desktop"],
      [true, "desktop"],
      [true, "phone"],
      [true, "phone"],
    ],
  );
  // The link is hovered with no specimen ancestors to force; the badge in
  // it is no target; focus forces focus-within on no ancestor either.
  assert.deepEqual(page.forcedCalls.slice(0, 4), [
    [101, ["hover"]],
    [101, []],
    [103, ["hover"]],
    [103, []],
  ]);
});

test("check reports a surface's differences in each state and its own rules once", async () => {
  const result = await check(fakeBrowser(galleryWindows()), {
    gallery: GALLERY,
    reference: "",
    surfaces: [{ css: "BAD", name: "site" }],
  });

  assert.equal(result.passed, false);
  assert.deepEqual(result.surfaces[0].findings, [
    { components: [".badge"], conditions: [], properties: ["color"], selector: ".site .badge" },
  ]);
  assert.deepEqual(
    result.surfaces[0].differences.filter((difference) => difference.variant === "green, desktop").map((difference) => difference.key),
    // The badge's colour differs at rest, reported once; the link's colour
    // differs only while hovered and pressed.
    ["badge > a:0 > span.badge:0", "hover badge > a:0 | badge > a:0", "active badge > a:0 | badge > a:0"],
  );
});

test("capture forces each state on a target's ancestors too, and reads them", async () => {
  const page = new FakePage(() => {
    const body = new FakeElement("body");
    const root = new FakeElement("div", { dataset: { specimen: "table" } });
    const row = new FakeElement("tr", {
      selectors: [SELECTORS.hover],
      states: { "focus-within": { "border-color": "focus" }, hover: { background: "surface" } },
    });
    const cell = new FakeElement("td", { selectors: [SELECTORS.hover, SELECTORS.focus], styles: { color: "muted" } });
    root.append(row.append(cell));

    return fakeWindow(body.append(root));
  });

  const result = await capture(page, "", VIEWPORT);

  assert.deepEqual(page.forcedCalls, [
    [101, ["hover"]],
    [101, []],
    [102, ["hover"]],
    [101, ["hover"]],
    [102, []],
    [101, []],
    [102, ["focus", "focus-visible"]],
    [101, ["focus-within"]],
    [102, []],
    [101, []],
    [101, ["active", "hover"]],
    [101, []],
    [102, ["active", "hover"]],
    [101, ["hover"]],
    [102, []],
    [101, []],
  ]);
  assert.equal(result.specimens, 1);
  assert.deepEqual(result.styles["hover table > tr:0 | table > tr:0"], { background: "surface" });
  // The cell's focus shows on its row, which the capture reads too; a state
  // records only what it changes from rest.
  assert.deepEqual(result.styles["focus table > tr:0 > td:0 | table > tr:0"], { "border-color": "focus" });
  assert.equal(result.styles["focus table > tr:0 > td:0 | table > tr:0 > td:0"], undefined);
});

test("check compares motion read before the freeze alongside everything read after it", async () => {
  // Arrange: a surface that only shortens the link's transition, which
  // the freeze would otherwise hide.
  const page = new FakePage((html) => {
    const body = specimenBody();
    if (html.includes("STILL")) {
      body.children[0].children[0].styles["transition-duration"] = "0s";
    }

    return fakeWindow(body);
  });

  // Act
  const rest = await capture(page, "", VIEWPORT);
  const result = await check(fakeBrowser(page), { gallery: GALLERY, reference: "", surfaces: [{ css: "STILL", name: "site" }] });

  // Assert
  assert.deepEqual(rest.styles["badge > a:0"], { color: "accent", "transition-duration": "0.14s" });
  assert.equal(result.passed, false);
  assert.deepEqual(
    result.surfaces[0].differences.filter((difference) => difference.variant === "green, desktop"),
    [{ actual: "0s", expected: "0.14s", key: "badge > a:0", property: "transition-duration", variant: "green, desktop" }],
  );
});

test("check fails when the gallery marks no specimen", async () => {
  const page = new FakePage(() => fakeWindow(new FakeElement("body")));

  await assert.rejects(check(fakeBrowser(page), { gallery: GALLERY, reference: "", surfaces: [] }), /no element with data-specimen/);
});

test("main checks the surfaces against the gallery and exits with the result", async () => {
  const directory = mkdtempSync(join(tmpdir(), "gallery-check-test-"));
  try {
    writeFileSync(join(directory, "gallery.html"), GALLERY);
    writeFileSync(join(directory, "tokens.css"), "﻿:root{}");
    writeFileSync(join(directory, "base.css"), ".badge{}");
    writeFileSync(join(directory, "site.css"), "BAD");
    const output = { error: [], log: [] };
    const io = { error: (line) => output.error.push(line), log: (line) => output.log.push(line) };
    const environment = { CHROME_PATH: SCRIPT };
    const launches = [];
    const launch = async (binary, options) => {
      launches.push([binary, options]);
      browser = fakeBrowser(galleryWindows());

      return browser;
    };
    let browser;
    const gallery = join(directory, "gallery.html");

    const same = [gallery, "--surface", "same=" + join(directory, "base.css"), "--markup", "same=" + directory];
    assert.equal(await main(same, environment, io, launch), 0);
    assert.deepEqual(output.log, ["Gallery check: same render all 1 specimens as the gallery does (8 renders)."]);
    assert.equal(browser.closed, true);
    assert.deepEqual(launches, [[SCRIPT, { linux: process.platform === "linux" }]]);
    assert.equal(await main([gallery, "--surface", "site=" + join(directory, "site.css")], environment, io, launch), 1);
    assert.equal(output.error[0], "Gallery check: a surface renders a shared component differently from the gallery.");
    assert.equal(await main([gallery], environment, io, launch), 2);
    assert.equal(output.error.at(-1), "name at least one --surface to compare with the gallery");
    assert.equal(await main([gallery, "--surface", "a=b.css"], { CHROME_PATH: join(directory, "none") }, io, launch), 2);
    assert.match(output.error.at(-1), /set CHROME_PATH/);
  } finally {
    rmSync(directory, { force: true, recursive: true });
  }
});

test("the script runs main when started directly", () => {
  const run = spawnSync(process.execPath, [SCRIPT], { encoding: "utf8" });

  assert.equal(run.status, 2);
  assert.match(run.stderr, /usage: gallery-check.mjs/);
});

test("Browser.launch starts the browser headless over a pipe with a fresh profile", async () => {
  const calls = [];
  const spawnProcess = (binary, flags, options) => {
    calls.push({ binary: binary, flags: flags, options: options });

    return fakeProcess(() => ({ result: {} }));
  };

  const browser = await Browser.launch("/opt/chrome", { linux: true }, spawnProcess);
  const desktop = await Browser.launch("/opt/chrome", {}, spawnProcess);

  assert.equal(calls[0].binary, "/opt/chrome");
  assert.ok(calls[0].flags.includes("--remote-debugging-pipe"));
  assert.ok(calls[0].flags.includes("--no-sandbox"));
  assert.equal(calls[1].flags.includes("--no-sandbox"), false);
  assert.deepEqual(calls[0].options.stdio, ["ignore", "ignore", "ignore", "pipe", "pipe"]);
  assert.ok(existsSync(browser.profile));
  for (const launched of [browser, desktop]) {
    setImmediate(() => launched.child.emit("exit", 0));
    await launched.close();
    assert.equal(existsSync(launched.profile), false);
  }
});

test("a page loads markup in a viewport, runs collectors, lists nodes, and forces states", async () => {
  const window = fakeWindow(specimenBody());
  const child = fakeProcess((message) => {
    const results = {
      "DOM.getDocument": { root: { nodeId: 1 } },
      "DOM.querySelectorAll": { nodeIds: [7, 8] },
      "Page.getFrameTree": { frameTree: { frame: { id: "frame" } } },
      "Target.attachToTarget": { sessionId: "session" },
      "Target.createTarget": { targetId: "target" },
    };
    if (message.method === "Runtime.evaluate" && message.params.returnByValue) {
      return { result: { result: { value: vm.runInNewContext(message.params.expression, { window: window }) } } };
    }

    return { result: results[message.method] ?? {} };
  });
  const directory = mkdtempSync(join(tmpdir(), "gallery-check-test-"));
  const browser = new Browser(child, child.stdio[3], child.stdio[4], directory);

  const page = await browser.newPage();
  await page.load("<p>", { height: 844, mobile: true, name: "phone", width: 390 });
  const list = await page.evaluate(collectPage, { focus: SELECTORS.focus, hover: SELECTORS.hover, mode: "list", selector: SELECTORS.specimens });
  const nodes = await page.nodes(SELECTORS.specimens);
  await page.force(7, ["hover"]);

  assert.equal(list.length, 4);
  assert.deepEqual(nodes, [7, 8]);
  assert.deepEqual(
    child.received.map((message) => [message.method, message.sessionId]),
    [
      ["Target.createTarget", undefined],
      ["Target.attachToTarget", undefined],
      ["Page.enable", "session"],
      ["DOM.enable", "session"],
      ["CSS.enable", "session"],
      ["Emulation.setScriptExecutionDisabled", "session"],
      ["Emulation.setDeviceMetricsOverride", "session"],
      ["Emulation.setTouchEmulationEnabled", "session"],
      ["Emulation.setEmulatedMedia", "session"],
      ["Page.getFrameTree", "session"],
      ["Page.setDocumentContent", "session"],
      ["Runtime.evaluate", "session"],
      ["Runtime.evaluate", "session"],
      ["DOM.getDocument", "session"],
      ["DOM.querySelectorAll", "session"],
      ["CSS.forcePseudoState", "session"],
    ],
  );
  assert.deepEqual(child.received[7].params, { enabled: true, maxTouchPoints: 1 });
  assert.deepEqual(child.received[15].params, { forcedPseudoClasses: ["hover"], nodeId: 7 });
  setImmediate(() => child.emit("exit", 0));
  await browser.close();
});

test("a page reports a collector that throws in the page", async () => {
  const answers = [
    { result: { exceptionDetails: { exception: { description: "TypeError: boom" } } } },
    { result: { exceptionDetails: { text: "Uncaught" } } },
  ];
  const child = fakeProcess(() => answers.shift());
  const page = new Page(new Browser(child, child.stdio[3], child.stdio[4], "/unused"), "session");

  await assert.rejects(page.evaluate(collectPage, {}), /could not run the collector: TypeError: boom/);
  await assert.rejects(page.evaluate(collectPage, {}), /could not run the collector: Uncaught/);
});

test("the browser connection rejects errors, ignores events, times out, and fails when the browser exits", async () => {
  const child = fakeProcess((message, process) => {
    if (message.method === "Fail") {
      return { error: { message: "unknown method" } };
    }
    if (message.method === "Event") {
      process.stdio[4].write(JSON.stringify({ method: "Page.loadEventFired", params: {} }) + "\0");

      return { result: { ok: true } };
    }

    return undefined;
  });
  const browser = new Browser(child, child.stdio[3], child.stdio[4], "/unused", 20);

  await assert.rejects(browser.send("Fail"), /Fail: unknown method/);
  assert.deepEqual(await browser.send("Event"), { ok: true });
  await assert.rejects(browser.send("Silent"), /Silent: no answer within 20 ms/);
  const pending = browser.send("Silent");
  child.emit("exit", 1);
  await assert.rejects(pending, /the browser exited/);
});

test("closing kills a browser that does not answer Browser.close", async () => {
  const directory = mkdtempSync(join(tmpdir(), "gallery-check-test-"));
  mkdirSync(join(directory, "Default"));
  const child = fakeProcess(() => ({ error: { message: "gone" } }));
  const browser = new Browser(child, child.stdio[3], child.stdio[4], directory);

  await browser.close();

  assert.equal(child.killed, true);
  assert.equal(existsSync(directory), false);
});
