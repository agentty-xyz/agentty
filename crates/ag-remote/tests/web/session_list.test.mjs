import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import { SEEN_KEY, createSessionList } from "../../assets/session_list.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const NOW = 1_000_000;

// One session of each group, out of order, plus one that waits for the
// user.
const SESSIONS = [
  { id: "done", status: "Done", title: "Encrypt relay streams", updated_at: NOW - 2 * 86400 },
  { id: "review", status: "Review", title: "Serve the pairing page", updated_at: NOW - 180 },
  { id: "merging", status: "Merging", title: "Renumber the migration", updated_at: NOW - 60 },
  { id: "running", status: "InProgress", title: "Add a container image", updated_at: NOW - 5 },
  { id: "draft", status: "Draft", title: null, updated_at: NOW - 3600 },
];

// Mounts the list on the client's page. Returns the page, the sessions
// opened through it, and the list.
function mountList({ storage = new MemoryStorage(), time = () => NOW, isFilterShown } = {}) {
  const window = createWindow(createDocument(INDEX_HTML));
  window.localStorage = storage;
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const opened = [];
  const list = createSessionList({
    document: document,
    elements: {
      cards: byId("home-cards"),
      chips: byId("chips"),
      empty: byId("sessions-empty"),
      filter: byId("filter"),
      filterKey: byId("filter-key"),
      list: byId("sessions"),
      summary: byId("filter-summary"),
      waiting: byId("home-waiting"),
    },
    isFilterShown: isFilterShown,
    now: time,
    onOpen: (sessionId) => opened.push(sessionId),
    window: window,
  });

  return { byId, document, list, opened, window };
}

function rowIds(byId) {
  return byId("sessions")
    .querySelectorAll(".row")
    .map((row) => row.dataset.sessionId);
}

function unreadIds(byId) {
  return byId("sessions")
    .querySelectorAll(".row")
    .filter((row) => row.querySelector(".unread"))
    .map((row) => row.dataset.sessionId);
}

function type(page, text) {
  page.byId("filter").value = text;
  page.byId("filter").dispatchEvent(new FakeEvent("input"));
}

test("groups follow merge queue, active, archive with counts, and the archive starts collapsed", () => {
  // Arrange
  const page = mountList();

  // Act
  page.list.update(SESSIONS, null);
  const groups = page.byId("sessions").children;
  const archive = groups[2];
  const collapsed = archive.querySelector(".rows").hidden;
  archive.querySelector(".group-toggle").click();
  const expanded = page.byId("sessions").querySelector('[data-group="archive"]');

  // Assert
  assert.deepEqual(
    groups.map((group) => [
      group.dataset.group,
      group.querySelector(".group-label").textContent,
    ]),
    [
      ["merge_queue", "Merge queue1"],
      ["active", "Active3"],
      ["archive", "+Archive1"],
    ],
  );
  assert.deepEqual(rowIds(page.byId), ["merging", "review", "running", "draft", "done"]);
  assert.equal(collapsed, true);
  assert.equal(archive.querySelector(".group-toggle").getAttribute("aria-expanded"), "false");
  assert.equal(expanded.querySelector(".rows").hidden, false);
  assert.equal(expanded.querySelector(".group-toggle").getAttribute("aria-expanded"), "true");
  assert.equal(expanded.querySelector(".marker").textContent, "-");
  assert.equal(page.byId("sessions-empty").hidden, true);
});

test("empty groups are left out and an empty project shows the empty line", () => {
  // Arrange
  const page = mountList();

  // Act
  page.list.update([SESSIONS[1]], null);
  const groups = page.byId("sessions").children.map((group) => group.dataset.group);
  page.list.update([], null);

  // Assert
  assert.deepEqual(groups, ["active"]);
  assert.equal(page.byId("sessions").children.length, 0);
  assert.equal(page.byId("sessions-empty").hidden, false);
  assert.equal(page.byId("filter-summary").hidden, true);
});

test("a row shows its title, status in its tone, time and whether it is open", () => {
  // Arrange
  const page = mountList();

  // Act
  page.list.update(SESSIONS, "review");
  const row = (id) => page.byId("sessions").querySelector('[data-session-id="' + id + '"]');

  // Assert
  assert.equal(row("review").querySelector(".row-title").textContent, "Serve the pairing page");
  assert.equal(row("review").getAttribute("aria-current"), "page");
  assert.equal(row("running").getAttribute("aria-current"), null);
  assert.equal(row("running").querySelector(".status").className, "status status--working");
  assert.equal(row("running").querySelector(".status").textContent, "In progress");
  assert.equal(row("running").querySelector(".time").textContent, "now");
  assert.equal(row("review").querySelector(".time").textContent, "3m");
  assert.equal(row("draft").querySelector(".row-title").textContent, "No title");
  assert.equal(row("done").classList.contains("is-archived"), true);
  assert.notEqual(row("done").querySelector(".time").title, "");
});

test("rows show size, model and PR ready, and cards add the line counts, when the host sends them", () => {
  // Arrange
  const page = mountList();
  const figures = {
    added_lines: 64,
    deleted_lines: 9,
    id: "review",
    model: "claude-opus-5-5",
    model_short_name: "opus-5.5",
    review_ready: "ready",
    size: "M",
    status: "Review",
    title: "Serve the pairing page",
    updated_at: NOW - 180,
  };
  const fresh = {
    added_lines: 0,
    deleted_lines: 0,
    id: "fresh",
    model: "gpt-5.4",
    model_short_name: "gpt-5.4",
    review_ready: "blocked",
    size: "XS",
    status: "Question",
    title: "Which lint?",
    updated_at: NOW - 720,
  };
  const old = { id: "old", status: "Watching", title: "From an older host", updated_at: NOW };

  // Act
  page.list.update([figures, fresh, old], null);
  const row = (id) => page.byId("sessions").querySelector('[data-session-id="' + id + '"]');
  const card = (id) => page.byId("home-cards").querySelector('[data-session-id="' + id + '"]');

  // Assert
  assert.equal(row("review").querySelector(".row-title").textContent, "[M] Serve the pairing page");
  assert.equal(row("review").querySelector(".size").textContent, "[M]");
  // A row leaves the line counts to the card and the header.
  assert.equal(row("review").querySelector(".row-meta").textContent, "Review·opus-5.5·PR ready3m");
  assert.equal(row("review").querySelector(".model").title, "opus-5.5");
  // Screen readers skip the separators.
  assert.equal(row("review").querySelector(".sep").getAttribute("aria-hidden"), "true");
  // A pull request that cannot merge shows nothing.
  assert.equal(row("fresh").querySelector(".row-meta").textContent, "Question·gpt-5.412m");
  assert.equal(row("old").querySelector(".row-title").textContent, "From an older host");
  assert.equal(row("old").querySelector(".row-meta").textContent, "Watchingnow");
  assert.equal(card("review").querySelector(".card-title").textContent, "[M] Serve the pairing page");
  assert.equal(card("review").querySelector(".card-figures").textContent, "+64 -9·opus-5.5·PR ready");
  assert.equal(card("review").querySelector(".add").textContent, "+64");
  assert.equal(card("review").querySelector(".del").textContent, "-9");
  // No changes yet, so no counts.
  assert.equal(card("fresh").querySelector(".card-figures").textContent, "gpt-5.4");
  assert.equal(card("old").querySelector(".card-figures"), null);
});

test("a row renders a hostile title only as a text node", () => {
  // Arrange
  const page = mountList();
  const title = '<img src=x onerror="alert(1)"><script>alert(2)</script>';

  // Act
  page.list.update([{ id: "x", status: "Question", title: title, updated_at: NOW }], null);
  const nodes = [
    page.byId("sessions").querySelector(".row-title"),
    page.byId("home-cards").querySelector(".card-title"),
  ];

  // Assert
  for (const node of nodes) {
    assert.equal(node.childNodes.length, 1);
    assert.equal(node.childNodes[0].nodeType, 3);
    assert.equal(node.textContent, title);
  }
  assert.equal(page.document.querySelectorAll("img").length, 0);
  assert.equal(page.document.querySelectorAll("script").length, 1);
});

test("choosing a row or a card opens its session", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);

  // Act
  page.byId("sessions").querySelector('[data-session-id="running"]').click();
  page.byId("home-cards").querySelector(".card").click();

  // Assert
  assert.deepEqual(page.opened, ["running", "review"]);
});

test("nothing starts unread, and a later update marks other sessions unread", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, "review");
  const firstUnread = unreadIds(page.byId);
  const stored = JSON.parse(page.window.localStorage.getItem(SEEN_KEY));

  // Act: every session changes, and a new one appears.
  const later = SESSIONS.map((session) => ({ ...session, updated_at: session.updated_at + 10 }));
  later.push({ id: "new", status: "Question", title: "New", updated_at: NOW });
  page.list.update(later, "review");
  const dot = page.byId("sessions").querySelector(".unread");

  // Assert
  assert.deepEqual(firstUnread, []);
  assert.deepEqual(Object.keys(stored).sort(), SESSIONS.map((session) => session.id).sort());
  assert.deepEqual(unreadIds(page.byId), ["merging", "running", "draft", "new", "done"]);
  assert.equal(dot.textContent, "Unread");
  assert.equal(dot.parentNode.classList.contains("is-unread"), true);
});

test("opening a session marks it seen, and the seen map keeps unlisted ids", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  const later = SESSIONS.slice(1).map((session) => ({ ...session, updated_at: NOW + 10 }));
  page.list.update(later, null);

  // Act
  page.list.setOpen("running");
  page.list.setOpen(null);
  const stored = JSON.parse(page.window.localStorage.getItem(SEEN_KEY));

  // Assert
  assert.deepEqual(unreadIds(page.byId), ["merging", "review", "draft"]);
  assert.equal(stored.running, NOW + 10);
  assert.equal(stored.review, NOW - 180);
  assert.equal(stored.done, NOW - 2 * 86400);
});

test("an update to the open session while the page is hidden waits until it shows", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, "review");
  page.document.visibilityState = "hidden";
  const later = SESSIONS.map((session) =>
    session.id === "review" ? { ...session, updated_at: NOW } : session,
  );

  // Act
  page.list.update(later, "review");
  const hiddenSeen = JSON.parse(page.window.localStorage.getItem(SEEN_KEY)).review;
  page.document.visibilityState = "visible";
  page.list.markSeen();
  const shownSeen = JSON.parse(page.window.localStorage.getItem(SEEN_KEY)).review;

  // Assert
  assert.equal(hiddenSeen, NOW - 180);
  assert.equal(shownSeen, NOW);
  assert.deepEqual(unreadIds(page.byId), []);
});

test("a damaged seen map counts as a first load", () => {
  // Arrange
  const storage = new MemoryStorage();
  storage.setItem(SEEN_KEY, "{not json");
  const page = mountList({ storage: storage });

  // Act
  page.list.update(SESSIONS, null);

  // Assert
  assert.deepEqual(unreadIds(page.byId), []);
  assert.equal(JSON.parse(storage.getItem(SEEN_KEY)).review, NOW - 180);
});

test("without storage no session shows as unread", () => {
  // Arrange
  const page = mountList({ storage: new MemoryStorage({ failing: true }) });
  page.list.update(SESSIONS, null);

  // Act
  page.list.update(
    SESSIONS.map((session) => ({ ...session, updated_at: NOW + 10 })),
    null,
  );

  // Assert
  assert.deepEqual(unreadIds(page.byId), []);
});

test("the filter matches titles without case, counts the chips and says how many match", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  const chips = () =>
    page.byId("chips").children.map((chip) => [chip.textContent, chip.getAttribute("aria-pressed")]);
  const unfiltered = chips();
  const summaryHiddenUnfiltered = page.byId("filter-summary").hidden;

  // Act
  type(page, "  THE ");
  const typed = {
    chips: chips(),
    key: page.byId("filter-key").textContent,
    rows: rowIds(page.byId),
    summary: page.byId("filter-summary").textContent,
    summaryHidden: page.byId("filter-summary").hidden,
  };
  // A keyboard user activates the focused chip, which keeps the focus.
  const chip = page.byId("chips").querySelector('[data-filter="needs_you"]');
  chip.focus();
  chip.click();

  // Assert
  assert.deepEqual(unfiltered, [
    ["All 5", "true"],
    ["Needs you 1", "false"],
    ["Running 2", "false"],
  ]);
  assert.equal(summaryHiddenUnfiltered, true);
  // A query shows the archive's matches even though it is collapsed.
  assert.deepEqual(typed, {
    chips: [
      ["All 2", "true"],
      ["Needs you 1", "false"],
      ["Running 1", "false"],
    ],
    key: "esc",
    rows: ["merging", "review"],
    summary: "2 of 5 sessions match.",
    summaryHidden: false,
  });
  assert.deepEqual(rowIds(page.byId), ["review"]);
  assert.equal(page.byId("filter-summary").textContent, "1 of 5 sessions match.");
  assert.equal(page.document.activeElement?.dataset.filter, "needs_you");
});

test("Escape in the filter clears it before anything else sees the key", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  type(page, "relay");
  let reachedDocument = 0;
  page.document.addEventListener("keydown", () => {
    reachedDocument += 1;
  });

  // Act
  const escape = new FakeEvent("keydown", { key: "Escape" });
  page.byId("filter").dispatchEvent(escape);
  const second = new FakeEvent("keydown", { key: "Escape" });
  page.byId("filter").dispatchEvent(second);

  // Assert
  assert.equal(page.byId("filter").value, "");
  assert.equal(page.byId("filter-key").textContent, "/");
  assert.equal(rowIds(page.byId).length, 5);
  assert.equal(escape.defaultPrevented, true);
  // An empty field lets Escape through, so it can close the drawer.
  assert.equal(second.defaultPrevented, false);
  assert.equal(reachedDocument, 1);
});

test("slash focuses the filter unless the user is typing", () => {
  // Arrange
  const page = mountList();
  const message = page.byId("message");

  // Act
  const fromMessage = new FakeEvent("keydown", { key: "/" });
  message.dispatchEvent(fromMessage);
  const focusedWhileTyping = page.document.activeElement;
  const fromPage = new FakeEvent("keydown", { key: "/" });
  page.document.body.dispatchEvent(fromPage);

  // Assert
  assert.equal(fromMessage.defaultPrevented, false);
  assert.notEqual(focusedWhileTyping, page.byId("filter"));
  assert.equal(fromPage.defaultPrevented, true);
  assert.equal(page.document.activeElement, page.byId("filter"));
});

test("the home view shows up to six sessions that wait for the user", () => {
  // Arrange
  const page = mountList();
  const waiting = Array.from({ length: 8 }, (_, index) => ({
    id: "q" + index,
    status: index % 2 ? "Question" : "Watching",
    title: "Waiting " + index,
    updated_at: NOW - 720,
  }));

  // Act
  page.list.update(SESSIONS.filter((session) => session.id !== "review"), null);
  const hiddenWithout = page.byId("home-waiting").hidden;
  page.list.update([...SESSIONS, ...waiting], null);
  const cards = page.byId("home-cards").querySelectorAll(".card");

  // Assert
  assert.equal(hiddenWithout, true);
  assert.equal(page.byId("home-waiting").hidden, false);
  assert.equal(cards.length, 6);
  assert.deepEqual(
    [cards[0].querySelector(".card-meta").textContent, cards[0].querySelector(".card-title").textContent],
    ["Review3m", "Serve the pairing page"],
  );
  assert.equal(cards[2].querySelector(".status").className, "status status--asking");
});

test("tick refreshes relative times without rebuilding the rows", () => {
  // Arrange
  let time = NOW;
  const page = mountList({ time: () => time });
  page.list.update(SESSIONS, null);
  const row = page.byId("sessions").querySelector('[data-session-id="running"]');

  // Act
  time = NOW + 3600;
  page.list.tick();

  // Assert
  assert.equal(page.byId("sessions").querySelector('[data-session-id="running"]'), row);
  assert.equal(row.querySelector(".time").textContent, "1h");
  assert.equal(page.byId("home-cards").querySelector(".time").textContent, "1h");
});

test("an update keeps the keyboard focus on the same row, card, chip and toggle", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  const later = SESSIONS.map((session) => ({ ...session, updated_at: session.updated_at + 1 }));
  const focusedAfterUpdate = (find) => {
    find().focus();
    page.list.update(later, null);

    return page.document.activeElement === find() && find().isConnected;
  };

  // Act
  const results = {
    card: focusedAfterUpdate(() => page.byId("home-cards").querySelector(".card")),
    chip: focusedAfterUpdate(() => page.byId("chips").querySelector('[data-filter="running"]')),
    row: focusedAfterUpdate(() =>
      page.byId("sessions").querySelector('[data-session-id="running"]'),
    ),
    toggle: focusedAfterUpdate(() => page.byId("sessions").querySelector(".group-toggle")),
  };
  // Opening a row by keyboard keeps the focus on it too.
  const row = page.byId("sessions").querySelector('[data-session-id="draft"]');
  row.focus();
  page.list.setOpen("draft");
  const opened = page.document.activeElement;

  // Assert
  assert.deepEqual(results, { card: true, chip: true, row: true, toggle: true });
  assert.equal(opened.dataset.sessionId, "draft");
  assert.equal(opened.getAttribute("aria-current"), "page");
});

test("two tabs keep each other's seen times", () => {
  // Arrange: both tabs share one storage and start from the same listing.
  const storage = new MemoryStorage();
  const first = mountList({ storage: storage });
  const second = mountList({ storage: storage });
  const later = SESSIONS.map((session) => ({ ...session, updated_at: NOW + 10 }));
  first.list.update(SESSIONS, null);
  second.list.update(SESSIONS, null);

  // Act: each tab opens a different updated session.
  first.list.update(later, "running");
  second.list.update(later, "review");
  const stored = JSON.parse(storage.getItem(SEEN_KEY));

  // Assert
  assert.equal(stored.running, NOW + 10);
  assert.equal(stored.review, NOW + 10);
});

test("a storage event from another tab clears the dots it saw", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  const later = SESSIONS.map((session) =>
    session.id === "running" ? { ...session, updated_at: NOW + 10 } : session,
  );
  page.list.update(later, null);
  const before = unreadIds(page.byId);

  // Act
  const seen = Object.fromEntries(later.map((session) => [session.id, session.updated_at]));
  page.window.dispatchEvent(
    new FakeEvent("storage", { key: SEEN_KEY, newValue: JSON.stringify(seen) }),
  );
  page.window.dispatchEvent(new FakeEvent("storage", { key: "other", newValue: "{}" }));

  // Assert
  assert.deepEqual(before, ["running"]);
  assert.deepEqual(unreadIds(page.byId), []);
});

test("the match count is written only when it changes", () => {
  // Arrange
  const page = mountList();
  page.list.update(SESSIONS, null);
  type(page, "relay");
  const summary = page.byId("filter-summary");
  const node = summary.firstChild;

  // Act
  page.list.update(SESSIONS, null);

  // Assert
  assert.equal(summary.textContent, "1 of 5 sessions match.");
  assert.equal(summary.firstChild, node);
});

test("slash does nothing while the filter is off screen", () => {
  // Arrange
  let isShown = false;
  const page = mountList({ isFilterShown: () => isShown });

  // Act
  const hidden = new FakeEvent("keydown", { key: "/" });
  page.document.body.dispatchEvent(hidden);
  const focusedWhileHidden = page.document.activeElement;
  isShown = true;
  const shown = new FakeEvent("keydown", { key: "/" });
  page.document.body.dispatchEvent(shown);

  // Assert
  assert.equal(hidden.defaultPrevented, false);
  assert.notEqual(focusedWhileHidden, page.byId("filter"));
  assert.equal(shown.defaultPrevented, true);
  assert.equal(page.document.activeElement, page.byId("filter"));
});
