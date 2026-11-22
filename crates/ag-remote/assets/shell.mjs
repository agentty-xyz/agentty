// Frame of the remote control client: the colour theme, the route kept in
// the address fragment, and the session sidebar, which becomes a drawer on
// narrow screens. Functions take `document` and `window` as parameters, so
// the Node tests in crates/ag-remote/tests/web/shell.test.mjs pass the fake
// DOM.

export const THEME_KEY = "agentty.remote.theme";
// The site's two themes, both dark. The first is the default.
export const THEMES = ["green", "dark"];
// `--color-bg` of each theme, for the browser's own interface.
const THEME_COLORS = { dark: "#080808", green: "#070b08" };
// Below this width the sidebar is a drawer; app.css uses the same width.
export const NARROW_QUERY = "(max-width: 860px)";

// Returns the theme saved in `window`'s storage, or the default when none
// is saved or storage cannot be read.
export function loadTheme(window) {
  let saved = null;
  try {
    saved = window.localStorage.getItem(THEME_KEY);
  } catch (_error) {
    // Blocked storage, as in some private windows, keeps the default.
  }

  return THEMES.includes(saved) ? saved : THEMES[0];
}

// Saves `theme` in `window`'s storage. Without storage the choice lasts for
// this page only.
export function saveTheme(window, theme) {
  try {
    window.localStorage.setItem(THEME_KEY, theme);
  } catch (_error) {
    // The theme still applies to this page.
  }
}

// Applies `theme` as the site does, with the `theme-dark` class on the
// root element, and marks the matching switch button pressed.
export function applyTheme(document, theme) {
  document.documentElement.classList.toggle("theme-dark", theme === "dark");
  document.querySelector('meta[name="theme-color"]')?.setAttribute("content", THEME_COLORS[theme]);
  for (const button of document.querySelectorAll("[data-theme]")) {
    button.setAttribute("aria-pressed", String(button.dataset.theme === theme));
  }
}

// Reads the address fragment. A pairing link wins over a session route,
// so `parsePairingLink` from relay.mjs is asked first. The open session
// lives in the fragment as `#s=<session id>`, which the browser never
// sends to a server.
export function parseRoute(hash, parsePairingLink) {
  const link = parsePairingLink(hash);
  if (link) {
    return { kind: "pairing", link: link };
  }
  const sessionId = new URLSearchParams(hash.replace(/^#/, "")).get("s");

  return sessionId ? { kind: "session", sessionId: sessionId } : { kind: "home" };
}

// Returns the fragment of the route to `sessionId`, or an empty one for
// home.
export function routeHash(sessionId) {
  return sessionId ? "#s=" + encodeURIComponent(sessionId) : "";
}

// The drawer's state machine. `isOpen` is whether the drawer shows, and
// `hasEntry` whether opening it pushed a history entry that the back
// gesture can pop. Each event returns the next state and the history
// change it needs: `push` a new entry, go `back` over the drawer's own
// entry, `replace` it, or none.
//
// - `open`: the menu button opened the drawer.
// - `dismiss`: the close button, the scrim, `Esc`, or a wider screen
//   closed it.
// - `pop`: the back gesture popped its entry.
// - `choose`: a session was chosen; its route replaces the drawer's entry,
//   so going back returns to the previous session.
export function nextDrawer(state, event) {
  switch (event) {
    case "open":
      return state.isOpen
        ? { history: null, state: state }
        : { history: "push", state: { hasEntry: true, isOpen: true } };
    case "dismiss":
      return {
        history: state.isOpen && state.hasEntry ? "back" : null,
        state: CLOSED,
      };
    case "pop":
      return { history: null, state: CLOSED };
    case "choose":
      return { history: state.hasEntry ? "replace" : "push", state: CLOSED };
    default:
      throw new Error("Unknown drawer event: " + event);
  }
}

const CLOSED = Object.freeze({ hasEntry: false, isOpen: false });

// Wires the sidebar, its drawer, and the route. `elements` holds `app`,
// `sidebar`, `main`, `scrim`, `menu` (the button that shows the sessions),
// `close` (the drawer's close button) and `hide` (the button that hides
// the sidebar on wide screens). `onRoute` runs with the parsed route when
// the back or forward gesture changes it.
//
// Returns `showSession(sessionId)`, which routes to a session chosen in
// the list (or home with `null`), and `isOpen()`.
export function createShell({ document, window, elements, parsePairingLink, onRoute }) {
  const narrow = window.matchMedia(NARROW_QUERY);
  let drawer = CLOSED;

  function render() {
    elements.app.classList.toggle("is-drawer-open", drawer.isOpen);
    elements.scrim.hidden = !drawer.isOpen;
    elements.main.toggleAttribute("inert", drawer.isOpen);
    const isShown = narrow.matches
      ? drawer.isOpen
      : !elements.app.classList.contains("is-sidebar-hidden");
    elements.menu.setAttribute("aria-expanded", String(isShown));
  }

  function navigate(change, hash) {
    const url = hash || window.location.pathname + window.location.search;
    if (change === "back") {
      window.history.back();
    } else if (change === "replace") {
      window.history.replaceState(null, "", url);
    } else if (change === "push" && window.location.hash !== hash) {
      window.history.pushState(null, "", url);
    }
  }

  function apply(event, hash) {
    const next = nextDrawer(drawer, event);
    const wasOpen = drawer.isOpen;
    drawer = next.state;
    if (event === "open") {
      // The drawer's entry keeps the current address, so popping it only
      // closes the drawer.
      if (next.history === "push") {
        window.history.pushState({ drawer: true }, "", window.location.href);
      }
    } else {
      navigate(next.history, hash);
    }
    render();
    if (drawer.isOpen && !wasOpen) {
      elements.close.focus();
    }
  }

  function dismiss() {
    if (drawer.isOpen) {
      apply("dismiss");
      elements.menu.focus();
    }
  }

  elements.menu.addEventListener("click", () => {
    if (narrow.matches) {
      apply("open");
    } else {
      elements.app.classList.remove("is-sidebar-hidden");
      render();
    }
  });
  elements.hide.addEventListener("click", () => {
    elements.app.classList.add("is-sidebar-hidden");
    render();
    elements.menu.focus();
  });
  elements.close.addEventListener("click", dismiss);
  elements.scrim.addEventListener("click", dismiss);
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && drawer.isOpen) {
      event.preventDefault();
      dismiss();
    }
  });
  narrow.addEventListener("change", () => {
    if (!narrow.matches) {
      apply("dismiss");
    }
    render();
  });
  window.addEventListener("popstate", () => {
    if (drawer.isOpen) {
      apply("pop");
    }
    const route = parseRoute(window.location.hash, parsePairingLink);
    if (route.kind !== "pairing") {
      onRoute(route);
    }
  });
  render();

  return {
    isOpen: () => drawer.isOpen,
    showSession: (sessionId) => apply("choose", routeHash(sessionId)),
  };
}
