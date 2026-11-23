import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import { parsePairingLink } from "../../assets/relay.mjs";
import {
  THEME_KEY,
  applyTheme,
  createShell,
  loadTheme,
  nextDrawer,
  parseRoute,
  routeHash,
  saveTheme,
} from "../../assets/shell.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const INSTANCE_KEY = "ab".repeat(32);
const PAIRING_LINK = "#relay=wss%3A%2F%2Frelay.example&key=" + INSTANCE_KEY + "&code=ABCD-EFGH";

function pageWindow(url = "http://127.0.0.1/", options = {}) {
  const document = createDocument(INDEX_HTML);

  return createWindow(document, url, options);
}

// Mounts the shell on the client's page. `isNarrow` picks the drawer
// layout. Returns the window, the elements and the routes seen.
function mountShell({ isNarrow = true, url = "http://127.0.0.1/" } = {}) {
  const window = pageWindow(url, { matches: () => isNarrow });
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const elements = {
    app: byId("shell"),
    close: byId("drawer-close"),
    hide: byId("sidebar-hide"),
    main: byId("main"),
    menu: byId("menu"),
    scrim: byId("scrim"),
    sidebar: byId("sidebar"),
  };
  const routes = [];
  const shell = createShell({
    document: document,
    elements: elements,
    onRoute: (route) => routes.push(route),
    parsePairingLink: parsePairingLink,
    window: window,
  });

  return { document, elements, routes, shell, window };
}

test("loadTheme reads a saved theme and falls back to green", () => {
  // Arrange
  const saved = pageWindow();
  saved.localStorage.setItem(THEME_KEY, "dark");
  const unknown = pageWindow();
  unknown.localStorage.setItem(THEME_KEY, "light");
  const blocked = pageWindow();
  blocked.localStorage = new MemoryStorage({ failing: true });

  // Act
  const themes = [saved, unknown, pageWindow(), blocked].map(loadTheme);

  // Assert
  assert.deepEqual(themes, ["dark", "green", "green", "green"]);
});

test("saveTheme stores the theme and survives blocked storage", () => {
  // Arrange
  const window = pageWindow();
  const blocked = pageWindow();
  blocked.localStorage = new MemoryStorage({ failing: true });

  // Act
  saveTheme(window, "dark");
  saveTheme(blocked, "dark");

  // Assert
  assert.equal(window.localStorage.getItem(THEME_KEY), "dark");
  assert.equal(loadTheme(window), "dark");
});

test("applyTheme sets the site's class, the theme's page background for the browser and the switch", () => {
  // Arrange
  const stylesheet = ":root { --color-bg: #010101; }\n:root.theme-dark { --color-bg: #020202; }\n";
  const document = pageWindow("http://127.0.0.1/", { stylesheet: stylesheet }).document;
  const pressed = () =>
    Array.from(document.querySelectorAll("[data-theme]"), (button) => [
      button.dataset.theme,
      button.getAttribute("aria-pressed"),
    ]);
  const themeColor = () => document.querySelector('meta[name="theme-color"]').content;

  // Act
  applyTheme(document, "dark");
  const dark = {
    hasClass: document.documentElement.classList.contains("theme-dark"),
    pressed: pressed(),
    themeColor: themeColor(),
  };
  applyTheme(document, "green");

  // Assert
  assert.deepEqual(dark, {
    hasClass: true,
    pressed: [
      ["green", "false"],
      ["dark", "true"],
    ],
    themeColor: "#020202",
  });
  assert.equal(document.documentElement.classList.contains("theme-dark"), false);
  assert.deepEqual(pressed(), [
    ["green", "true"],
    ["dark", "false"],
  ]);
  assert.equal(themeColor(), "#010101");
});

test("parseRoute reads home, a session, and a pairing link before a session", () => {
  // Arrange
  const hashes = ["", "#", "#s=session-1", "#s=" + encodeURIComponent("a b/c"), "#other=1", PAIRING_LINK];

  // Act
  const routes = hashes.map((hash) => parseRoute(hash, parsePairingLink));
  const both = parseRoute(PAIRING_LINK + "&s=session-1", parsePairingLink);

  // Assert
  assert.deepEqual(routes.slice(0, 5), [
    { kind: "home" },
    { kind: "home" },
    { kind: "session", sessionId: "session-1" },
    { kind: "session", sessionId: "a b/c" },
    { kind: "home" },
  ]);
  assert.equal(routes[5].kind, "pairing");
  assert.equal(routes[5].link.code, "ABCD-EFGH");
  assert.equal(both.kind, "pairing");
});

test("routeHash builds the fragment parseRoute reads back", () => {
  // Arrange
  const sessionIds = ["session-1", "a b/c&s=x", null];

  // Act
  const hashes = sessionIds.map(routeHash);

  // Assert
  assert.deepEqual(hashes, ["#s=session-1", "#s=a%20b%2Fc%26s%3Dx", ""]);
  assert.deepEqual(parseRoute(hashes[1], parsePairingLink), {
    kind: "session",
    sessionId: "a b/c&s=x",
  });
});

test("nextDrawer moves between states with the history change each needs", () => {
  // Arrange
  const closed = { hasEntry: false, isOpen: false };
  const open = { hasEntry: true, isOpen: true };
  const cases = [
    [closed, "open", "push", open],
    [open, "open", null, open],
    [open, "dismiss", "back", closed],
    [closed, "dismiss", null, closed],
    [open, "pop", null, closed],
    [open, "choose", "replace", closed],
    [closed, "choose", "push", closed],
  ];

  // Act
  const results = cases.map(([state, event]) => nextDrawer(state, event));

  // Assert
  results.forEach((result, index) => {
    const [, event, history, state] = cases[index];
    assert.equal(result.history, history, event);
    assert.deepEqual(result.state, state, event);
  });
  assert.throws(() => nextDrawer(closed, "slide"), /Unknown drawer event/);
});

test("opening the drawer pushes an entry, makes the main area inert and focuses it", () => {
  // Arrange
  const { document, elements, shell, window } = mountShell();

  // Act
  elements.menu.click();

  // Assert
  assert.equal(shell.isOpen(), true);
  assert.equal(window.history.length, 2);
  assert.deepEqual(window.history.state, { drawer: true });
  assert.equal(elements.app.classList.contains("is-drawer-open"), true);
  assert.equal(elements.scrim.hidden, false);
  assert.equal(elements.main.hasAttribute("inert"), true);
  assert.equal(elements.menu.getAttribute("aria-expanded"), "true");
  assert.equal(document.activeElement, elements.close);
});

test("the back gesture closes the drawer without changing the route", () => {
  // Arrange
  const { elements, routes, shell, window } = mountShell({ url: "http://127.0.0.1/#s=one" });
  elements.menu.click();

  // Act
  window.history.back();

  // Assert
  assert.equal(shell.isOpen(), false);
  assert.equal(elements.main.hasAttribute("inert"), false);
  assert.equal(elements.scrim.hidden, true);
  assert.equal(window.location.hash, "#s=one");
  assert.deepEqual(routes, [{ kind: "session", sessionId: "one" }]);
});

test("the close button, the scrim and Escape each close the drawer through history", () => {
  // Arrange
  const closers = [
    (shell) => shell.elements.close.click(),
    (shell) => shell.elements.scrim.click(),
    (shell) => shell.document.dispatchEvent(new FakeEvent("keydown", { key: "Escape" })),
  ];

  // Act
  const results = closers.map((close) => {
    const mounted = mountShell();
    mounted.elements.menu.click();
    close(mounted);

    return {
      focused: mounted.document.activeElement === mounted.elements.menu,
      historyAt: mounted.window.history.state,
      isOpen: mounted.shell.isOpen(),
    };
  });

  // Assert
  for (const result of results) {
    assert.deepEqual(result, { focused: true, historyAt: null, isOpen: false });
  }
});

test("Escape leaves the drawer to a modal dialog open over it", () => {
  // Arrange
  const { document, elements, shell, window } = mountShell();
  elements.menu.click();
  const dialog = document.getElementById("forget-dialog");
  dialog.showModal();
  const event = new FakeEvent("keydown", { key: "Escape" });

  // Act
  document.dispatchEvent(event);

  // Assert
  assert.equal(shell.isOpen(), true);
  assert.equal(event.defaultPrevented, false);
  assert.deepEqual(window.history.state, { drawer: true });
  assert.equal(elements.main.hasAttribute("inert"), true);
});

test("Escape that something inside the drawer handled does not close it", () => {
  // Arrange
  const { document, elements, shell, window } = mountShell();
  elements.menu.click();
  const menuButton = document.getElementById("device-more");
  // As app.js does for the device menu: close it, keep focus on its button.
  elements.sidebar.addEventListener("keydown", (event) => {
    event.preventDefault();
    menuButton.focus();
  });

  // Act
  menuButton.dispatchEvent(new FakeEvent("keydown", { key: "Escape" }));

  // Assert
  assert.equal(shell.isOpen(), true);
  assert.deepEqual(window.history.state, { drawer: true });
  assert.equal(document.activeElement, menuButton);
});

test("closing the drawer by the back gesture or a choice moves focus to the menu button", () => {
  // Arrange
  const popped = mountShell();
  const chosen = mountShell();
  for (const mounted of [popped, chosen]) {
    mounted.elements.menu.click();
  }

  // Act
  popped.window.history.back();
  chosen.shell.showSession("two");

  // Assert
  for (const mounted of [popped, chosen]) {
    assert.equal(mounted.shell.isOpen(), false);
    assert.equal(mounted.document.activeElement, mounted.elements.menu);
  }
});

test("reset closes the drawer and drops its history entry", () => {
  // Arrange
  const { elements, shell, window } = mountShell({ url: "http://127.0.0.1/#s=one" });
  elements.menu.click();

  // Act
  shell.reset();
  shell.reset();

  // Assert
  assert.equal(shell.isOpen(), false);
  assert.equal(elements.main.hasAttribute("inert"), false);
  assert.equal(elements.scrim.hidden, true);
  assert.equal(window.history.state, null);
  assert.equal(window.location.hash, "#s=one");
});

test("choosing a session replaces the drawer's entry, so back returns to the last session", () => {
  // Arrange
  const { elements, routes, shell, window } = mountShell({ url: "http://127.0.0.1/#s=one" });
  elements.menu.click();

  // Act
  shell.showSession("two");
  const afterChoice = { hash: window.location.hash, length: window.history.length };
  window.history.back();

  // Assert
  assert.equal(shell.isOpen(), false);
  assert.deepEqual(afterChoice, { hash: "#s=two", length: 2 });
  assert.equal(window.location.hash, "#s=one");
  assert.deepEqual(routes, [{ kind: "session", sessionId: "one" }]);
});

test("on a wide screen a chosen session pushes a route and home drops the fragment", () => {
  // Arrange
  const { routes, shell, window } = mountShell({ isNarrow: false });

  // Act
  shell.showSession("one");
  shell.showSession("one");
  shell.showSession("two");
  shell.showSession(null);
  window.history.back();

  // Assert
  assert.equal(window.history.length, 4);
  assert.equal(window.location.hash, "#s=two");
  assert.deepEqual(routes, [{ kind: "session", sessionId: "two" }]);
  window.history.forward();
  assert.equal(window.location.href, "http://127.0.0.1/");
  assert.deepEqual(routes.at(-1), { kind: "home" });
});

test("on a wide screen the sidebar hides and the menu button shows it again", () => {
  // Arrange
  const { document, elements, shell } = mountShell({ isNarrow: false });
  const before = elements.menu.getAttribute("aria-expanded");

  // Act
  elements.hide.click();
  const hidden = {
    expanded: elements.menu.getAttribute("aria-expanded"),
    focused: document.activeElement === elements.menu,
    isHidden: elements.app.classList.contains("is-sidebar-hidden"),
  };
  elements.menu.click();

  // Assert
  assert.equal(before, "true");
  assert.deepEqual(hidden, { expanded: "false", focused: true, isHidden: true });
  assert.equal(elements.app.classList.contains("is-sidebar-hidden"), false);
  assert.equal(elements.menu.getAttribute("aria-expanded"), "true");
  assert.equal(shell.isOpen(), false);
});

test("a pairing link in the fragment is left to the pairing flow", () => {
  // Arrange
  const { routes, window } = mountShell();
  window.location.hash = "s=one";

  // Act
  window.location.hash = PAIRING_LINK;
  window.history.back();
  window.history.forward();

  // Assert
  assert.deepEqual(routes, [{ kind: "session", sessionId: "one" }]);
});
