// Session list of the remote control client: the groups and rows in the
// sidebar, the filter field and its chips, unread dots, and the cards on
// the home view. Titles are untrusted and only ever become text nodes.
// Functions take `document` and `window` as parameters, so the Node tests
// in crates/ag-remote/tests/web/session_list.test.mjs pass the fake DOM.

import {
  FILTERS,
  filterCounts,
  filterSessions,
  groupSessions,
  mergeSeen,
  parseSeen,
  reconcileSeen,
  relativeTime,
  statusInfo,
  unreadIds,
} from "./format.mjs";

// Map from session id to the `updated_at` this browser last saw.
export const SEEN_KEY = "agentty.remote.seen";
// How often relative times refresh, without a request.
export const TICK_MS = 30000;
// Most cards the home view shows.
const MAX_CARDS = 6;
// The group that starts collapsed.
const COLLAPSED_GROUP = "archive";

// Wires the list. `elements` holds `list` (the groups), `empty` (the line
// shown when the project has no sessions), `summary` (how many sessions
// match a filter), `filter` (the text field), `filterKey` (the key hint in
// it), `chips` (the filter buttons), and `cards` with `waiting`, the home
// view's cards and their section. `onOpen(sessionId)` runs when a row or a
// card is chosen. `now()` returns the time in seconds, and
// `isFilterShown()` whether the filter field is on screen for `/`.
//
// Returns `update(sessions, openId)` for a new listing while `openId` is
// open, `setOpen(sessionId)` when another session (or `null`, home) opens,
// `markSeen()` once the page shows again,
// and `tick()`, which refreshes relative times.
export function createSessionList({
  document,
  window,
  elements,
  onOpen,
  now = defaultNow,
  isFilterShown = () => true,
}) {
  const state = {
    filter: FILTERS[0].id,
    isArchiveOpen: false,
    // Whether storage failed; then no session shows as unread.
    isStorageBroken: false,
    openId: null,
    query: "",
    seen: null,
    sessions: [],
  };
  state.seen = readStored();

  // Reads the map another tab of this page may have written since.
  function readStored() {
    if (state.isStorageBroken) {
      return null;
    }
    try {
      return parseSeen(window.localStorage.getItem(SEEN_KEY));
    } catch (_error) {
      state.isStorageBroken = true;

      return null;
    }
  }

  // Records what this browser has seen. The stored map is read again first,
  // so two tabs keep each other's later times. The open session counts as
  // seen only while the page is visible.
  function reconcile() {
    const visibleOpenId = document.visibilityState === "hidden" ? null : state.openId;
    const known = mergeSeen(state.seen, readStored());
    state.seen = reconcileSeen(known, state.sessions, visibleOpenId);
    if (state.isStorageBroken) {
      return;
    }
    try {
      window.localStorage.setItem(SEEN_KEY, JSON.stringify(state.seen));
    } catch (_error) {
      state.isStorageBroken = true;
    }
  }

  // Rebuilds the list, chips and cards. A focused row, card, chip or
  // toggle is focused again in its new copy, so keyboard users keep their
  // place when an update arrives.
  function render() {
    const focused = focusedKey();
    const unread = state.isStorageBroken
      ? new Set()
      : unreadIds(state.sessions, state.seen, state.openId);
    const shown = filterSessions(state.sessions, { filter: state.filter, query: state.query });
    const isFiltering = state.query.trim() !== "" || state.filter !== FILTERS[0].id;
    const time = now();
    elements.list.replaceChildren(
      ...groupSessions(shown).map((group) => renderGroup(group, unread, time)),
    );
    elements.empty.hidden = state.sessions.length > 0;
    elements.summary.hidden = !isFiltering || state.sessions.length === 0;
    // The summary is a live region: unchanged text is not written again,
    // so an update does not announce it once more.
    const summary = shown.length + " of " + state.sessions.length + " sessions match.";
    if (elements.summary.textContent !== summary) {
      elements.summary.textContent = summary;
    }
    renderChips();
    renderCards(time);
    restoreFocus(focused);
  }

  function focusedKey() {
    const active = document.activeElement;
    for (const container of [elements.list, elements.chips, elements.cards]) {
      if (active && container.contains(active)) {
        return { container: container, key: keyOf(active) };
      }
    }

    return null;
  }


  // A collapsed group still shows its label and count. A query shows its
  // matches in every group.
  function renderGroup(group, unread, time) {
    const section = element(document, "section", "session-group");
    section.dataset.group = group.id;
    const count = element(document, "span", "count", String(group.sessions.length));
    const heading = element(document, "h2", "group-label");
    const rows = element(document, "ul", "rows");
    if (group.id === COLLAPSED_GROUP) {
      const isOpen = state.isArchiveOpen || state.query.trim() !== "";
      const toggle = element(document, "button", "group-toggle");
      toggle.type = "button";
      toggle.setAttribute("aria-expanded", String(isOpen));
      toggle.append(element(document, "span", "marker", isOpen ? "-" : "+"), group.label, count);
      toggle.addEventListener("click", () => {
        state.isArchiveOpen = !isOpen;
        render();
      });
      heading.append(toggle);
      rows.hidden = !isOpen;
    } else {
      heading.append(group.label, count);
    }
    rows.append(
      ...group.sessions.map((session) =>
        renderRow(session, {
          isArchived: group.id === COLLAPSED_GROUP,
          isUnread: unread.has(session.id),
          time: time,
        }),
      ),
    );
    section.append(heading, rows);

    return section;
  }

  function renderRow(session, { isArchived, isUnread, time }) {
    const item = element(document, "li");
    const row = element(document, "button", "row");
    row.type = "button";
    row.dataset.sessionId = session.id;
    row.classList.toggle("is-unread", isUnread);
    row.classList.toggle("is-archived", isArchived);
    if (session.id === state.openId) {
      row.setAttribute("aria-current", "page");
    }
    const meta = element(document, "span", "row-meta");
    meta.append(statusLine(document, session.status), timeNode(session.updated_at, time));
    row.append(element(document, "span", "row-title", displayTitle(session)), meta);
    if (isUnread) {
      const dot = element(document, "span", "unread");
      dot.append(element(document, "span", "visually-hidden", "Unread"));
      row.append(dot);
    }
    row.addEventListener("click", () => onOpen(session.id));
    item.append(row);

    return item;
  }

  function timeNode(updatedAt, time) {
    const node = element(document, "time", "time", relativeTime(updatedAt, time));
    node.dataset.updatedAt = String(updatedAt);
    node.title = fullDate(updatedAt);

    return node;
  }

  function renderChips() {
    const counts = filterCounts(state.sessions, state.query);
    elements.chips.replaceChildren(
      ...FILTERS.map((filter) => {
        const button = element(document, "button");
        button.type = "button";
        button.dataset.filter = filter.id;
        button.setAttribute("aria-pressed", String(filter.id === state.filter));
        button.append(filter.label + " ", element(document, "span", "count", String(counts[filter.id])));
        button.addEventListener("click", () => {
          state.filter = filter.id;
          render();
        });

        return button;
      }),
    );
  }

  // Up to six sessions that wait for the user, as cards on the home view.
  function renderCards(time) {
    const waiting = filterSessions(state.sessions, { filter: "needs_you" }).slice(0, MAX_CARDS);
    elements.waiting.hidden = waiting.length === 0;
    elements.cards.replaceChildren(
      ...waiting.map((session) => {
        const item = element(document, "li");
        const card = element(document, "button", "card");
        card.type = "button";
        card.dataset.sessionId = session.id;
        const line = element(document, "span", "card-meta");
        line.append(statusLine(document, session.status), timeNode(session.updated_at, time));
        card.append(line, element(document, "strong", "card-title", displayTitle(session)));
        card.addEventListener("click", () => onOpen(session.id));
        item.append(card);

        return item;
      }),
    );
  }

  function setQuery(query) {
    state.query = query;
    elements.filterKey.textContent = query ? "esc" : "/";
    render();
  }

  elements.filter.addEventListener("input", () => setQuery(elements.filter.value));
  elements.filter.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && elements.filter.value !== "") {
      // Clearing the field comes before closing the drawer.
      event.preventDefault();
      event.stopPropagation();
      elements.filter.value = "";
      setQuery("");
    }
  });
  // `/` focuses the field when it is on screen, unless the user is typing
  // somewhere already.
  document.addEventListener("keydown", (event) => {
    if (event.key !== "/" || event.ctrlKey || event.metaKey || event.altKey || !isFilterShown()) {
      return;
    }
    const target = event.target;
    if (target?.closest?.("input, textarea, select, dialog")) {
      return;
    }
    event.preventDefault();
    elements.filter.focus();
  });
  // Another tab of this page marked sessions seen.
  window.addEventListener("storage", (event) => {
    if (event.key !== SEEN_KEY || state.isStorageBroken) {
      return;
    }
    state.seen = mergeSeen(state.seen, parseSeen(event.newValue));
    render();
  });

  return {
    markSeen: () => {
      reconcile();
      render();
    },
    setOpen: (sessionId) => {
      state.openId = sessionId;
      reconcile();
      render();
    },
    tick: () => {
      const time = now();
      for (const node of document.querySelectorAll("time[data-updated-at]")) {
        node.textContent = relativeTime(Number(node.dataset.updatedAt), time);
      }
    },
    update: (sessions, openId = null) => {
      state.sessions = sessions;
      state.openId = openId;
      reconcile();
      render();
    },
  };
}

// The title a row shows; the terminal also says `No title` for a session
// without one.
export function displayTitle(session) {
  return session.title || "No title";
}

// A status dot and label in the status's tone.
export function statusLine(document, statusName) {
  const info = statusInfo(statusName);
  const node = element(document, "span", "status status--" + info.tone);
  node.append(element(document, "i"), info.label);

  return node;
}

// Focuses the button in `focused.container` with the key `focusedKey`
// recorded before a render, if it is still there.
function restoreFocus(focused) {
  if (!focused?.key) {
    return;
  }
  for (const button of focused.container.querySelectorAll("button")) {
    if (keyOf(button) === focused.key) {
      button.focus({ preventScroll: true });

      return;
    }
  }
}

// What identifies a list button across renders: its session, its filter,
// or the archive toggle.
function keyOf(node) {
  if (node.dataset?.sessionId) {
    return "session:" + node.dataset.sessionId;
  }
  if (node.dataset?.filter) {
    return "filter:" + node.dataset.filter;
  }

  return node.classList?.contains("group-toggle") ? "toggle" : null;
}

function fullDate(unixSeconds) {
  return new Date(unixSeconds * 1000).toLocaleString();
}

function defaultNow() {
  return Date.now() / 1000;
}

function element(document, tag, className, text) {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }

  return node;
}
