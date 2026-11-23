#!/usr/bin/env node
// Checks that pages render the shared components as the design gallery does.
//
//   node gallery-check.mjs gallery.html --surface <name>=<stylesheet>[,<stylesheet>...] \
//     [--markup <name>=<path>[,<path>...]] ...
//
// The gallery loads the shared stylesheets by relative path. For each
// surface, this script renders the gallery's markup once with those
// stylesheets alone and once with the surface's whole stylesheet chain in
// their place, in both themes, on a desktop and a touch phone viewport,
// at rest and with each interactive element hovered, focused, and pressed.
// It fails when any computed style of an element inside a `data-specimen`
// element differs, other than the properties that place or size a
// component in its page, and when a surface rule names a shared component
// to set any other property. With `--markup`, the surface's templates and
// scripts, a rule on a class the markup sets beside a shared class counts
// as naming that component too. It reads only local files and needs a
// Chromium-based browser: set CHROME_PATH, or it looks in the usual places.

import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

// Properties that place or size a component in its page. A surface may set
// them; they are neither compared nor audited.
export const LAYOUT_PROPERTIES = new Set([
  "align-self",
  "bottom",
  "flex",
  "flex-basis",
  "flex-grow",
  "flex-shrink",
  "grid-area",
  "grid-column",
  "grid-column-end",
  "grid-column-start",
  "grid-row",
  "grid-row-end",
  "grid-row-start",
  "inline-size",
  "inset",
  "inset-block",
  "inset-block-end",
  "inset-block-start",
  "inset-inline",
  "inset-inline-end",
  "inset-inline-start",
  "justify-self",
  "left",
  "margin",
  "margin-block",
  "margin-block-end",
  "margin-block-start",
  "margin-bottom",
  "margin-inline",
  "margin-inline-end",
  "margin-inline-start",
  "margin-left",
  "margin-right",
  "margin-top",
  "max-inline-size",
  "max-width",
  "min-inline-size",
  "min-width",
  "order",
  "place-self",
  "position",
  "right",
  "top",
  "width",
  "z-index",
]);

// Themes: each renders the page with this class on the root element.
export const THEMES = [
  { className: "", name: "green" },
  { className: "theme-dark", name: "dark" },
];

// Viewports: a desktop with a mouse, and a phone with touch.
export const VIEWPORTS = [
  { height: 900, mobile: false, name: "desktop", width: 1280 },
  { height: 844, mobile: true, name: "phone", width: 390 },
];

// Forced states and the pseudo-classes each forces on the element and on
// its ancestors inside the specimen.
export const STATES = [
  { ancestors: ["hover"], forced: ["hover"], name: "hover", target: "hover" },
  { ancestors: ["focus-within"], forced: ["focus", "focus-visible"], name: "focus", target: "focus" },
  { ancestors: ["hover"], forced: ["active", "hover"], name: "active", target: "hover" },
];

// The specimens and their elements; the elements a pointer can hover; and
// the elements keyboard focus can reach.
export const SELECTORS = Object.freeze({
  focus: "a[href], button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, [tabindex]",
  hover: "a, button, input, select, textarea, summary, label, tr, [tabindex]",
  specimens: "[data-specimen], [data-specimen] *",
});
const SPECIMENS = SELECTORS.specimens;
const FREEZE = "*, *::before, *::after { transition: none !important; animation: none !important; }";
const COMMAND_TIMEOUT_MS = 30000;
// Files under a markup directory that can hold class names.
const MARKUP = /\.(html?|m?[jt]sx?|vue|svelte)$/;
const REPORT_LIMIT = 200;

// Runs the check for `argv` and returns the exit code; output goes to `io`.
export async function main(argv, environment, io, launch = Browser.launch) {
  let options;
  try {
    options = parseArguments(argv);
  } catch (error) {
    io.error(error.message);

    return 2;
  }
  const chrome = findChrome(environment, process.platform, existsSync, readdirSync);
  if (!chrome) {
    io.error(
      "No Chromium-based browser found; set CHROME_PATH to Chrome, Chromium, or chrome-headless-shell.",
    );

    return 2;
  }
  const gallery = readFileSync(options.gallery, "utf8");
  const reference = galleryStylesheets(gallery).map((name) => resolve(dirname(options.gallery), name));
  const browser = await launch(chrome, { linux: process.platform === "linux" });
  try {
    const result = await check(browser, {
      gallery: gallery,
      reference: readStylesheets(reference),
      surfaces: options.surfaces.map((surface) => ({
        css: readStylesheets(surface.files),
        markup: surface.markup.flatMap(readMarkup),
        name: surface.name,
      })),
    });
    for (const line of formatReport(result)) {
      (result.passed ? io.log : io.error)(line);
    }

    return result.passed ? 0 : 1;
  } finally {
    await browser.close();
  }
}

// Reads `--surface name=a.css,b.css` options after the gallery path, and
// for any of them `--markup name=templates,scripts`, files or directories.
export function parseArguments(argv) {
  const [gallery, ...rest] = argv;
  if (!gallery || gallery.startsWith("--")) {
    throw new Error(
      "usage: gallery-check.mjs <gallery.html> --surface <name>=<stylesheet>[,<stylesheet>...] " +
        "[--markup <name>=<path>[,<path>...]] ...",
    );
  }
  const surfaces = [];
  const markup = [];
  for (let index = 0; index < rest.length; index += 2) {
    const value = rest[index + 1] ?? "";
    const separator = value.indexOf("=");
    const option = rest[index];
    if (!["--surface", "--markup"].includes(option) || separator < 1 || separator === value.length - 1) {
      throw new Error("expected --surface or --markup <name>=<path>[,<path>...], not " + rest.slice(index).join(" "));
    }
    const entry = { name: value.slice(0, separator), paths: value.slice(separator + 1).split(",") };
    (option === "--surface" ? surfaces : markup).push(entry);
  }
  if (surfaces.length === 0) {
    throw new Error("name at least one --surface to compare with the gallery");
  }
  const stray = markup.find((candidate) => !surfaces.some((surface) => surface.name === candidate.name));
  if (stray) {
    throw new Error("--markup names " + stray.name + ", which no --surface names");
  }

  return {
    gallery: gallery,
    surfaces: surfaces.map((surface) => ({
      files: surface.paths,
      markup: markup.filter((entry) => entry.name === surface.name).flatMap((entry) => entry.paths),
      name: surface.name,
    })),
  };
}

// Returns the text of the markup at `path`: a file, or every template and
// script file under a directory, in a stable order.
export function readMarkup(path) {
  if (!statSync(path).isDirectory()) {
    return [readFileSync(path, "utf8")];
  }

  return readdirSync(path)
    .sort()
    .flatMap((name) => {
      const child = join(path, name);

      return statSync(child).isDirectory() || MARKUP.test(name) ? readMarkup(child) : [];
    });
}

// Returns the browser to run: CHROME_PATH, then the usual install places,
// including the headless shell that Playwright downloads.
export function findChrome(environment, platform, exists, list) {
  if (environment.CHROME_PATH) {
    return exists(environment.CHROME_PATH) ? environment.CHROME_PATH : null;
  }
  const home = environment.HOME ?? "";
  const candidates =
    platform === "darwin"
      ? [
          ...playwrightShells(join(home, "Library/Caches/ms-playwright"), ["chrome-headless-shell-mac-arm64", "chrome-headless-shell-mac-x64"], list),
          "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
          "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
      : [
          "/usr/bin/google-chrome",
          "/usr/bin/google-chrome-stable",
          "/usr/bin/chromium",
          "/usr/bin/chromium-browser",
          ...playwrightShells(join(home, ".cache/ms-playwright"), ["chrome-headless-shell-linux64"], list),
        ];

  return candidates.find((candidate) => exists(candidate)) ?? null;
}

// Returns the headless shells in a Playwright cache, newest first.
function playwrightShells(cache, platforms, list) {
  let entries = [];
  try {
    entries = list(cache);
  } catch (_error) {
    return [];
  }

  return entries
    .filter((entry) => entry.startsWith("chromium_headless_shell-"))
    .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
    .flatMap((entry) => platforms.map((platform) => join(cache, entry, platform, "chrome-headless-shell")));
}

// Returns the stylesheets the gallery links, in order.
export function galleryStylesheets(html) {
  const names = [];
  for (const tag of html.match(/<link\b[^>]*>/gi) ?? []) {
    const href = /\bhref\s*=\s*["']([^"']+)["']/i.exec(tag);
    if (/\brel\s*=\s*["']stylesheet["']/i.test(tag) && href) {
      names.push(href[1]);
    }
  }
  if (names.length === 0) {
    throw new Error("the gallery links no stylesheet");
  }

  return names;
}

// Reads and joins stylesheets, without the byte order mark a compiler may
// write, which a style element would read as part of the first selector.
function readStylesheets(files) {
  return files.map((file) => readFileSync(file, "utf8").replace(/^﻿/, "")).join("\n");
}

// Returns the gallery with its stylesheet links replaced by `css`, inline,
// and `theme` set on the root element.
export function galleryPage(html, css, theme) {
  const withoutLinks = html.replace(/<link\b[^>]*\brel\s*=\s*["']stylesheet["'][^>]*>\s*/gi, "");
  const styled = withoutLinks.replace(/<head\b[^>]*>/i, (head) => head + "<style data-gallery-chain>" + css + "</style>");

  return theme ? styled.replace(/<html\b/i, '<html class="' + theme + '"') : styled;
}

// Runs `step` on each item strictly in turn, each once the previous one has
// settled, and resolves to their results in order. The steps drive one
// shared page, so they must never overlap.
export function inSequence(items, step) {
  return items.reduce(
    (done, item) => done.then((results) => step(item).then((result) => results.concat([result]))),
    Promise.resolve([]),
  );
}

// Renders the reference and every surface and returns the differences.
export async function check(browser, input) {
  const page = await browser.newPage();
  const surfaces = input.surfaces.map((surface) => ({ differences: [], findings: [], name: surface.name }));
  const variants = THEMES.flatMap((theme) => VIEWPORTS.map((viewport) => ({ theme: theme, viewport: viewport })));
  let captures = 0;
  let specimens = 0;
  await inSequence(variants, async ({ theme, viewport }) => {
    const variant = theme.name + ", " + viewport.name;
    const reference = await capture(page, galleryPage(input.gallery, input.reference, theme.className), viewport);
    if (reference.specimens === 0) {
      throw new Error("the gallery marks no element with data-specimen");
    }
    specimens = reference.specimens;
    captures += 1;
    const pairs = input.surfaces.map((surface, index) => ({ result: surfaces[index], surface: surface }));
    await inSequence(pairs, async ({ result, surface }) => {
      const rendered = await capture(page, galleryPage(input.gallery, surface.css, theme.className), viewport);
      captures += 1;
      result.differences.push(...compareStyles(reference.styles, rendered.styles, variant));
      if (theme === THEMES[0] && viewport === VIEWPORTS[0]) {
        result.findings.push(...auditRules(reference.rules, rendered.rules, surface.markup));
      }
    });
  });

  return {
    captures: captures,
    passed: surfaces.every((surface) => surface.differences.length === 0 && surface.findings.length === 0),
    specimens: specimens,
    surfaces: surfaces,
  };
}

// Renders `html` in `viewport` and returns its specimens' computed styles
// at rest and in each forced state, and its stylesheet chain's rules.
export async function capture(page, html, viewport) {
  await page.load(html, viewport);
  const elements = await page.evaluate(collectPage, {
    focus: SELECTORS.focus,
    hover: SELECTORS.hover,
    mode: "list",
    selector: SPECIMENS,
  });
  const rules = await page.evaluate(collectPage, { mode: "rules" });
  const layout = [...LAYOUT_PROPERTIES];
  const styles = await page.evaluate(collectPage, { layout: layout, mode: "styles", motion: true, selector: SPECIMENS });
  await page.evaluate(collectPage, { css: FREEZE, mode: "freeze" });
  // Each element's motion read before the freeze joins the rest read after.
  const frozen = await page.evaluate(collectPage, { layout: layout, mode: "styles", motion: false, selector: SPECIMENS });
  for (const [key, values] of Object.entries(frozen)) {
    styles[key] = { ...styles[key], ...values };
  }
  const nodes = await page.nodes(SPECIMENS);
  // One page, so each state is forced, read, and released in turn.
  const targets = STATES.flatMap((state) =>
    elements.filter((candidate) => candidate[state.target]).map((element) => ({ element: element, state: state })),
  );
  await inSequence(targets, async ({ element, state }) => {
    const forced = await readForced(page, nodes, element, state, layout);
    Object.assign(styles, stateChanges(styles, forced, state.name + " " + element.key + " | "));
  });

  return { rules: rules, specimens: elements.filter((element) => element.root).length, styles: styles };
}

// Forces `state` on `element` and its ancestors inside the specimen, reads
// the whole specimen, since a forced state also styles ancestors, such as a
// field that shows its input's focus with :focus-within, and releases it.
async function readForced(page, nodes, element, state, layout) {
  await page.force(nodes[element.index], state.forced);
  await inSequence(element.ancestors, (ancestor) => page.force(nodes[ancestor], state.ancestors));
  const forced = await page.evaluate(collectPage, {
    layout: layout,
    mode: "styles",
    motion: false,
    root: element.specimen,
    selector: SPECIMENS,
  });
  await inSequence([element.index, ...element.ancestors], (node) => page.force(nodes[node], []));

  return forced;
}

// Returns, under `prefix`, only what a forced state changes from `rest`, so
// a difference at rest is reported once rather than again in every state.
// A pseudo-element that only a state gives content, such as a hovered
// `::after`, has no styles at rest, so all of its styles count.
export function stateChanges(rest, forced, prefix) {
  const changes = {};
  for (const [key, values] of Object.entries(forced)) {
    const before = rest[key] ?? {};
    const changed = Object.entries(values).filter(([name, value]) => before[name] !== value);
    if (changed.length > 0) {
      changes[prefix + key] = Object.fromEntries(changed);
    }
  }

  return changes;
}

// Runs in the page, and in tests against a fake window: lists the
// specimens' elements, reads their computed styles, freezes motion, or
// reads the stylesheet chain's rules. It must stay self-contained, because
// the browser receives only its source.
export function collectPage(window, options) {
  const document = window.document;
  const elements = options.selector ? Array.from(document.querySelectorAll(options.selector)) : [];
  const isSpecimen = (element) => "specimen" in element.dataset;
  const keyOf = (element) => {
    const parts = [];
    let node = element;
    while (!isSpecimen(node)) {
      const parent = node.parentElement;
      const name = node.localName + Array.from(node.classList, (name) => "." + name).join("");
      parts.unshift(name + ":" + Array.prototype.indexOf.call(parent.children, node));
      node = parent;
    }
    parts.unshift(node.dataset.specimen);

    return parts.join(" > ");
  };
  const list = () =>
    elements.map((element, index) => {
      const ancestors = [];
      let node = element;
      while (!isSpecimen(node)) {
        node = node.parentElement;
        ancestors.unshift(elements.indexOf(node));
      }
      const root = isSpecimen(element);

      return {
        ancestors: root ? [] : ancestors.slice(1),
        focus: element.matches(options.focus),
        hover: element.matches(options.hover),
        index: index,
        key: keyOf(element),
        root: root,
        specimen: root ? index : ancestors[0],
      };
    });
  const freeze = () => {
    const style = document.createElement("style");
    style.textContent = options.css;
    document.head.append(style);

    return true;
  };
  // The at-rule a grouping rule's conditions start with.
  const conditionPrefix = (rule) => {
    if (rule.media) {
      return "@media ";
    }
    if (rule.conditionText === undefined) {
      return "@layer ";
    }

    return rule.containerName === undefined ? "@supports " : "@container ";
  };
  // A rule is known by its conditions, selector, and the properties it
  // sets: a compiler may rewrite values, as Sass does with `transparent`,
  // without changing what they mean.
  const styleRule = (rule, conditions) => {
    const declarations = Array.from(rule.style, (name) => name);

    return {
      conditions: conditions,
      declarations: declarations,
      hidden: rule.style.getPropertyValue("display") === "none",
      selector: rule.selectorText,
      text: conditions.concat(rule.selectorText, "{" + declarations.join(";") + "}").join(" "),
    };
  };
  const visit = (rules, conditions) =>
    Array.from(rules).flatMap((rule) => {
      if (rule.selectorText !== undefined) {
        return [styleRule(rule, conditions)];
      }

      return rule.cssRules ? visit(rule.cssRules, conditions.concat(conditionPrefix(rule) + (rule.conditionText ?? rule.name))) : [];
    });
  const chainRules = () =>
    Array.from(document.styleSheets)
      .filter((sheet) => "galleryChain" in sheet.ownerNode.dataset)
      .flatMap((sheet) => visit(sheet.cssRules, []));
  const layout = new Set(options.layout);
  const isMotion = (name) => name.startsWith("transition") || name.startsWith("animation");
  const read = (element, pseudo) => {
    const style = window.getComputedStyle(element, pseudo || null);
    if (pseudo && pseudo !== "::placeholder" && (style.content === "none" || style.content === "normal")) {
      return null;
    }
    const names = Array.from(style).filter(
      (name) => !name.startsWith("--") && !layout.has(name) && isMotion(name) === options.motion,
    );

    return Object.fromEntries(names.map((name) => [name, style.getPropertyValue(name)]));
  };
  const styles = () => {
    const root = options.root === undefined ? null : elements[options.root];
    const inScope = (element) => !isSpecimen(element) && (!root || root === element || root.contains(element));
    const collected = {};
    for (const element of elements.filter(inScope)) {
      const key = keyOf(element);
      const pseudos = ["", "::before", "::after"].concat(element.matches("input, textarea") ? ["::placeholder"] : []);
      for (const pseudo of pseudos) {
        const values = read(element, pseudo);
        if (values) {
          collected[key + pseudo] = values;
        }
      }
    }

    return collected;
  };
  const modes = { freeze: freeze, list: list, rules: chainRules };

  return (modes[options.mode] ?? styles)();
}

// Returns every property whose computed value on the surface differs from
// the reference, under `variant`.
export function compareStyles(reference, surface, variant) {
  const differences = [];
  for (const [key, expected] of Object.entries(reference)) {
    const actual = surface[key] ?? {};
    for (const name of Object.keys({ ...expected, ...actual })) {
      if (expected[name] !== actual[name]) {
        differences.push({ actual: actual[name], expected: expected[name], key: key, property: name, variant: variant });
      }
    }
  }
  for (const key of Object.keys(surface).filter((key) => !(key in reference))) {
    differences.push({ actual: "present", expected: "absent", key: key, property: "(element)", variant: variant });
  }

  return differences;
}

// Returns the surface's own rules that name a shared component and set
// something other than its placement. Shared components are the classes,
// and the element types under a class that styles only its descendants
// (such as the container of rendered Markdown), that the reference rules
// select; an element type under another shared class counts only with
// that class. `markup`, the text of the surface's templates and scripts,
// adds the surface's own classes that sit beside a shared class in one
// quoted string, such as a class attribute: a rule on one of those
// restyles the component's element too.
export function auditRules(reference, surface, markup = []) {
  const known = new Set(reference.map((rule) => rule.text));
  const scopedTypes = new Set();
  const pairs = new Set();
  const parts = (selector) => selectorParts(selector).filter((part) => !part.subject.root);
  const referenceParts = reference.flatMap((rule) => parts(rule.selector));
  const classes = new Set(referenceParts.flatMap((part) => part.subject.classes));
  // A modifier, such as `.block--variant`, belongs to its block.
  const component = (name) => classes.has(name.split("--")[0]) ? name.split("--")[0] : null;
  for (const part of referenceParts.filter((candidate) => candidate.subject.classes.length === 0)) {
    for (const type of part.subject.types) {
      for (const name of part.context) {
        if (component(name)) {
          pairs.add(component(name) + " " + type);
        } else {
          scopedTypes.add(type);
        }
      }
    }
  }
  const companions = companionClasses(markup, component);
  const findings = [];
  for (const rule of surface.filter((candidate) => !known.has(candidate.text))) {
    const named = parts(rule.selector).flatMap((part) => [
      ...part.subject.classes.filter((name) => component(name)).map((name) => "." + name),
      ...part.subject.classes
        .filter((name) => companions.has(name))
        .map((name) => "." + name + " (with " + companions.get(name) + ")"),
      ...part.subject.types.filter((type) => scopedTypes.has(type)),
      ...part.subject.types.flatMap((type) =>
        part.context
          .filter((name) => component(name) && pairs.has(component(name) + " " + type))
          .map((name) => "." + name + " " + type),
      ),
    ]);
    // Hiding a component, as a narrow layout may, places it too.
    const properties = rule.declarations.filter(
      (name) => !LAYOUT_PROPERTIES.has(name) && !(name === "display" && rule.hidden),
    );
    if (named.length > 0 && properties.length > 0) {
      findings.push({
        components: [...new Set(named)],
        conditions: rule.conditions,
        properties: properties,
        selector: rule.selector,
      });
    }
  }

  return findings;
}

// Returns each class that shares a quoted string in `markup` with a shared
// class, which `component` recognizes, mapped to the first such shared
// class. Scripts that build a class name by concatenation keep the shared
// class and its companions in one literal, as in "status status-line".
// Block comments, HTML comments, and lines that hold only a comment are
// skipped, since their prose quotes words, not classes.
export function companionClasses(markup, component) {
  const companions = new Map();
  for (const text of markup) {
    const code = withoutComments(text)
      .split("\n")
      .filter((line) => !line.trimStart().startsWith("//"))
      .join("\n");
    for (const quoted of code.matchAll(/"([^"\n]*)"|'([^'\n]*)'|`([^`\n]*)`/g)) {
      const tokens = (quoted[1] ?? quoted[2] ?? quoted[3]).split(/\s+/).filter((token) => /^-?[A-Za-z_][\w-]*$/.test(token));
      const shared = tokens.find((token) => component(token));
      for (const token of shared ? tokens.filter((candidate) => !component(candidate)) : []) {
        if (!companions.has(token)) {
          companions.set(token, "." + shared);
        }
      }
    }
  }

  return companions;
}

// Returns `text` with every block comment and HTML comment cut out, scanning
// for each opener and its closer, so the result is never itself a
// comment that one pass of a pattern left behind. An unclosed comment runs
// to the end.
function withoutComments(text) {
  let kept = "";
  let index = 0;
  while (index < text.length) {
    const block = text.indexOf("/*", index);
    const html = text.indexOf("<!--", index);
    const starts = [block, html].filter((start) => start >= 0);
    if (starts.length === 0) {
      return kept + text.slice(index);
    }
    const start = Math.min(...starts);
    const [opener, closer] = start === block ? ["/*", "*/"] : ["<!--", "-->"];
    const end = text.indexOf(closer, start + opener.length);
    kept += text.slice(index, start);
    index = end < 0 ? text.length : end + closer.length;
  }

  return kept;
}

// Splits a selector list into complex selectors and returns, for each, the
// classes and types of its subject compound and the classes before it.
// Arguments of :is() and :where() count as part of their compound;
// those of other functional pseudo-classes do not.
export function selectorParts(selector) {
  return splitTopLevel(selector, ",").flatMap((complex) => {
    const compounds = splitCompounds(complex.trim());
    const subject = compoundNames(compounds.at(-1) ?? "");
    const context = compounds.slice(0, -1).flatMap((compound) => compoundNames(compound).classes);

    return subject.alternatives.length > 0
      ? subject.alternatives.map((alternative) => ({ context: context.concat(alternative.context), subject: alternative.subject }))
      : [{ context: context, subject: { classes: subject.classes, root: subject.root, types: subject.types } }];
  });
}

// Returns how `character` changes the nesting depth of parentheses and
// attribute brackets.
function nesting(character) {
  if ("([".includes(character)) {
    return 1;
  }

  return ")]".includes(character) ? -1 : 0;
}

function splitTopLevel(text, separator) {
  const parts = [];
  let depth = 0;
  let current = "";
  for (const character of text) {
    depth += nesting(character);
    if (character === separator && depth === 0) {
      parts.push(current);
      current = "";
    } else {
      current += character;
    }
  }

  return parts.concat(current);
}

function splitCompounds(complex) {
  const compounds = [];
  let depth = 0;
  let current = "";
  for (const character of complex) {
    depth += nesting(character);
    if (depth === 0 && /[\s>+~]/.test(character)) {
      if (current) {
        compounds.push(current);
      }
      current = "";
    } else {
      current += character;
    }
  }

  return current ? compounds.concat(current) : compounds;
}

// Splits a compound into its own text and its functional pseudo-classes,
// each with its name and argument, nested parentheses kept in the argument.
function splitPseudoFunctions(compound) {
  let own = "";
  const functions = [];
  let depth = 0;
  for (const character of compound) {
    const closes = character === ")" && depth === 1;
    if (character === "(" && depth === 0) {
      const name = /:([\w-]+)$/.exec(own)?.[1] ?? "";
      own = own.slice(0, own.length - name.length - 1);
      functions.push({ argument: "", name: name });
    } else if (depth > 0 && !closes) {
      functions.at(-1).argument += character;
    } else if (depth === 0) {
      own += character;
    }
    depth += character === "(" ? 1 : 0;
    depth -= character === ")" ? 1 : 0;
  }

  return { functions: functions, own: own };
}

// Returns `text` without attribute selectors, such as `[aria-pressed="true"]`.
function withoutAttributes(text) {
  return text
    .split("[")
    .map((part, index) => (index === 0 ? part : part.slice(part.indexOf("]") + 1)))
    .join("");
}

// Returns a compound's own classes and type, and for :is() and :where()
// arguments the subject of each alternative.
function compoundNames(compound) {
  const { functions, own } = splitPseudoFunctions(compound);
  const alternatives = functions
    .filter((pseudo) => pseudo.name === "is" || pseudo.name === "where")
    .flatMap((pseudo) => selectorParts(pseudo.argument));
  const bare = withoutAttributes(own);
  const classes = Array.from(bare.matchAll(/\.([\w-]+)/g), (match) => match[1]);
  const type = /^[a-z][\w-]*/i.exec(bare)?.[0]?.toLowerCase();
  const types = type ? [type] : [];
  // A class on the root element, such as a theme, names no component.
  const root = type === "html" || bare.includes(":root");

  return {
    alternatives: alternatives.map((alternative) => ({
      context: alternative.context,
      subject: {
        classes: classes.concat(alternative.subject.classes),
        root: root || alternative.subject.root,
        types: types.concat(alternative.subject.types),
      },
    })),
    classes: classes,
    root: root,
    types: types,
  };
}

// Returns the report's lines: a summary when every surface matches, and
// otherwise each difference and finding, a bounded number of them.
export function formatReport(result) {
  if (result.passed) {
    return [
      "Gallery check: " +
        result.surfaces.map((surface) => surface.name).join(", ") +
        " render all " +
        result.specimens +
        " specimens as the gallery does (" +
        result.captures +
        " renders).",
    ];
  }
  const lines = [];
  for (const surface of result.surfaces) {
    for (const finding of surface.findings) {
      lines.push(
        surface.name +
          ": " +
          (finding.conditions.length > 0 ? finding.conditions.join(" ") + " " : "") +
          finding.selector +
          " restyles " +
          finding.components.join(", ") +
          " with " +
          finding.properties.join(", "),
      );
    }
    // A forced state records only what it changes, so a missing value is
    // the value at rest.
    const value = (text) => text ?? "as at rest";
    for (const difference of surface.differences) {
      lines.push(
        surface.name +
          " [" +
          difference.variant +
          "] " +
          difference.key +
          ": " +
          difference.property +
          " is " +
          value(difference.actual) +
          ", gallery " +
          value(difference.expected),
      );
    }
  }
  const total = lines.length;

  return [
    "Gallery check: a surface renders a shared component differently from the gallery.",
    ...lines.slice(0, REPORT_LIMIT),
    ...(total > REPORT_LIMIT ? ["... and " + (total - REPORT_LIMIT) + " more"] : []),
  ];
}

// A browser driven over the DevTools protocol through a pipe, so it opens
// no port.
export class Browser {
  buffer = "";
  nextId = 1;
  pending = new Map();

  constructor(child, input, output, profile, timeout = COMMAND_TIMEOUT_MS) {
    this.child = child;
    this.input = input;
    this.profile = profile;
    this.timeout = timeout;
    this.exited = new Promise((resolveExit) => child.once("exit", resolveExit));
    output.on("data", (chunk) => this.receive(chunk));
    child.once("exit", () => this.fail(new Error("the browser exited")));
  }

  // Starts `binary` headless with a fresh profile.
  static async launch(binary, options = {}, spawnProcess = spawn) {
    const profile = mkdtempSync(join(tmpdir(), "gallery-check-"));
    const flags = [
      "--headless",
      "--remote-debugging-pipe",
      "--user-data-dir=" + profile,
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-background-networking",
      "--disable-component-update",
      "--disable-extensions",
      "--disable-gpu",
      "--disable-sync",
      "--force-color-profile=srgb",
      "--hide-scrollbars",
      "--mute-audio",
      "--password-store=basic",
      "--use-mock-keychain",
      // Linux CI runners may not allow the sandbox's namespaces; the page is
      // local markup with scripts disabled.
      ...(options.linux ? ["--no-sandbox"] : []),
      "about:blank",
    ];
    const child = spawnProcess(binary, flags, { stdio: ["ignore", "ignore", "ignore", "pipe", "pipe"] });

    return new Browser(child, child.stdio[3], child.stdio[4], profile);
  }

  receive(chunk) {
    this.buffer += chunk.toString();
    let end = this.buffer.indexOf("\0");
    while (end >= 0) {
      const message = JSON.parse(this.buffer.slice(0, end));
      this.buffer = this.buffer.slice(end + 1);
      const waiting = this.pending.get(message.id);
      if (waiting) {
        this.pending.delete(message.id);
        clearTimeout(waiting.timer);
        if (message.error) {
          waiting.reject(new Error(waiting.method + ": " + message.error.message));
        } else {
          waiting.resolve(message.result);
        }
      }
      end = this.buffer.indexOf("\0");
    }
  }

  fail(error) {
    for (const waiting of this.pending.values()) {
      clearTimeout(waiting.timer);
      waiting.reject(error);
    }
    this.pending.clear();
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId;
    this.nextId += 1;

    return new Promise((resolveSend, rejectSend) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        rejectSend(new Error(method + ": no answer within " + this.timeout + " ms"));
      }, this.timeout);
      this.pending.set(id, { method: method, reject: rejectSend, resolve: resolveSend, timer: timer });
      this.input.write(JSON.stringify({ id: id, method: method, params: params, sessionId: sessionId }) + "\0");
    });
  }

  // Opens a page with scripts disabled.
  async newPage() {
    const { targetId } = await this.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await this.send("Target.attachToTarget", { flatten: true, targetId: targetId });
    const page = new Page(this, sessionId);
    await inSequence(["Page.enable", "DOM.enable", "CSS.enable"], (method) => page.send(method));
    await page.send("Emulation.setScriptExecutionDisabled", { value: true });

    return page;
  }

  async close() {
    try {
      await this.send("Browser.close");
    } catch (_error) {
      this.child.kill();
    }
    await this.exited;
    rmSync(this.profile, { force: true, recursive: true });
  }
}

// One page of the browser.
export class Page {
  constructor(browser, sessionId) {
    this.browser = browser;
    this.sessionId = sessionId;
  }

  send(method, params = {}) {
    return this.browser.send(method, params, this.sessionId);
  }

  // Shows `html` in `viewport` and waits for its fonts.
  async load(html, viewport) {
    await this.send("Emulation.setDeviceMetricsOverride", {
      deviceScaleFactor: 1,
      height: viewport.height,
      mobile: viewport.mobile,
      width: viewport.width,
    });
    await this.send("Emulation.setTouchEmulationEnabled", { enabled: viewport.mobile, maxTouchPoints: 1 });
    await this.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "" }] });
    const { frameTree } = await this.send("Page.getFrameTree");
    await this.send("Page.setDocumentContent", { frameId: frameTree.frame.id, html: html });
    await this.send("Runtime.evaluate", { awaitPromise: true, expression: "document.fonts.ready.then(() => true)" });
  }

  // Runs `collect(window, options)` in the page and returns its value.
  async evaluate(collect, options) {
    const { exceptionDetails, result } = await this.send("Runtime.evaluate", {
      expression: "(" + collect.toString() + ")(window, " + JSON.stringify(options) + ")",
      returnByValue: true,
    });
    if (exceptionDetails) {
      throw new Error("the page could not run the collector: " + (exceptionDetails.exception?.description ?? exceptionDetails.text));
    }

    return result.value;
  }

  // Returns the node identifiers of the elements `selector` selects, in
  // document order.
  async nodes(selector) {
    const { root } = await this.send("DOM.getDocument", { depth: 0 });
    const { nodeIds } = await this.send("DOM.querySelectorAll", { nodeId: root.nodeId, selector: selector });

    return nodeIds;
  }

  force(nodeId, pseudoClasses) {
    return this.send("CSS.forcePseudoState", { forcedPseudoClasses: pseudoClasses, nodeId: nodeId });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  process.exitCode = await main(process.argv.slice(2), process.env, console);
}
