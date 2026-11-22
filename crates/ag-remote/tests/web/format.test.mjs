import assert from "node:assert/strict";
import test from "node:test";

import {
  FILTERS,
  GROUPS,
  filterCounts,
  filterSessions,
  groupSessions,
  parseActivity,
  parseNotice,
  parseSeen,
  reconcileSeen,
  relativeTime,
  statusInfo,
  unreadIds,
} from "../../assets/format.mjs";

const ALL_STATUSES = [
  "Draft",
  "InProgress",
  "Review",
  "AgentReview",
  "Watching",
  "Question",
  "Queued",
  "Rebasing",
  "Merging",
  "Merged",
  "Done",
  "Canceled",
];

function session(id, status, title = id, updatedAt = 100) {
  return { id: id, status: status, title: title, updated_at: updatedAt };
}

test("statusInfo gives every status its label, tone and group", () => {
  // Arrange
  const expected = {
    Draft: ["Draft", "idle", "active"],
    InProgress: ["In progress", "working", "active"],
    Review: ["Review", "ready", "active"],
    AgentReview: ["Agent review", "ready", "active"],
    Watching: ["Watching", "ready", "active"],
    Question: ["Question", "asking", "active"],
    Queued: ["Queued", "queue", "merge_queue"],
    Rebasing: ["Syncing", "queue", "active"],
    Merging: ["Merging", "queue", "merge_queue"],
    Merged: ["Merged", "done", "active"],
    Done: ["Done", "done", "archive"],
    Canceled: ["Canceled", "failed", "archive"],
  };

  // Act
  const described = Object.fromEntries(
    ALL_STATUSES.map((name) => {
      const info = statusInfo(name);

      return [name, [info.label, info.tone, info.group]];
    }),
  );

  // Assert
  assert.deepEqual(described, expected);
});

test("statusInfo allows the actions ag_session::SessionStatus allows", () => {
  // Arrange
  const flags = ["review", "diff", "sync", "composer", "cancel"];
  const expected = {
    Draft: ["composer", "cancel"],
    InProgress: ["diff", "sync", "composer", "cancel"],
    Review: ["review", "diff", "sync", "composer", "cancel"],
    AgentReview: ["review", "diff", "sync", "composer", "cancel"],
    Watching: ["review", "diff", "sync", "composer", "cancel"],
    Question: ["composer", "cancel"],
    Queued: [],
    Rebasing: ["composer"],
    Merging: [],
    Merged: ["diff"],
    Done: ["diff"],
    Canceled: ["diff"],
  };

  // Act
  const allowed = Object.fromEntries(
    ALL_STATUSES.map((name) => [name, flags.filter((flag) => statusInfo(name)[flag])]),
  );

  // Assert
  assert.deepEqual(allowed, expected);
});

test("statusInfo shows an unknown status by name in the idle tone and allows nothing", () => {
  // Arrange
  const name = "Paused";

  // Act
  const info = statusInfo(name);
  const inherited = statusInfo("toString");

  // Assert
  assert.equal(info.label, "Paused");
  assert.equal(info.tone, "idle");
  assert.equal(info.group, "active");
  assert.deepEqual(
    ["review", "diff", "sync", "composer", "cancel", "needsYou", "running"].filter(
      (flag) => info[flag],
    ),
    [],
  );
  assert.equal(inherited.label, "toString");
});

test("relativeTime rounds to the largest whole unit", () => {
  // Arrange
  const now = 1_000_000;
  const cases = [
    [now, "now"],
    [now + 30, "now"],
    [now - 59, "now"],
    [now - 60, "1m"],
    [now - 59 * 60, "59m"],
    [now - 60 * 60, "1h"],
    [now - (3600 + 29 * 60 + 40), "1h"],
    [now - 23 * 3600, "23h"],
    [now - (23 * 3600 + 40 * 60), "1d"],
    [now - 36 * 3600, "2d"],
    [now - 30 * 86400, "30d"],
  ];

  // Act
  const formatted = cases.map(([updatedAt]) => relativeTime(updatedAt, now));

  // Assert
  assert.deepEqual(
    formatted,
    cases.map(([, label]) => label),
  );
});

test("groupSessions orders merge queue, active, archive and omits empty groups", () => {
  // Arrange
  const sessions = [
    session("done", "Done"),
    session("review", "Review"),
    session("merging", "Merging"),
    session("draft", "Draft"),
    session("unknown", "Paused"),
    session("queued", "Queued"),
  ];

  // Act
  const groups = groupSessions(sessions);
  const activeOnly = groupSessions([session("review", "Review")]);

  // Assert
  assert.deepEqual(
    groups.map((group) => [group.id, group.label, group.sessions.map((item) => item.id)]),
    [
      ["merge_queue", "Merge queue", ["merging", "queued"]],
      ["active", "Active", ["review", "draft", "unknown"]],
      ["archive", "Archive", ["done"]],
    ],
  );
  assert.deepEqual(
    activeOnly.map((group) => group.id),
    ["active"],
  );
  assert.deepEqual(groupSessions([]), []);
  assert.deepEqual(
    GROUPS.map((group) => group.id),
    ["merge_queue", "active", "archive"],
  );
});

test("filterSessions matches title substrings without case and applies the filter", () => {
  // Arrange
  const sessions = [
    session("a", "Question", "Fix the Login page"),
    session("b", "InProgress", "Add login tests"),
    session("c", "Done", "Write docs"),
    session("d", "Merged", null),
    session("e", "Queued", "Login cleanup"),
  ];
  const ids = (filtered) => filtered.map((item) => item.id);

  // Act
  const everything = filterSessions(sessions);
  const byQuery = filterSessions(sessions, { query: "  LOGIN " });
  const needsYou = filterSessions(sessions, { filter: "needs_you" });
  const runningLogin = filterSessions(sessions, { filter: "running", query: "login" });

  // Assert
  assert.deepEqual(ids(everything), ["a", "b", "c", "d", "e"]);
  assert.deepEqual(ids(byQuery), ["a", "b", "e"]);
  assert.deepEqual(ids(needsYou), ["a", "d"]);
  assert.deepEqual(ids(runningLogin), ["b", "e"]);
});

test("filterCounts counts each filter among the sessions that match the query", () => {
  // Arrange
  const sessions = ALL_STATUSES.map((name) => session(name, name, "Task " + name));

  // Act
  const counts = filterCounts(sessions);
  const narrowed = filterCounts(sessions, "review");

  // Assert
  assert.deepEqual(counts, { all: 12, needs_you: 5, running: 4 });
  assert.deepEqual(narrowed, { all: 2, needs_you: 2, running: 0 });
  assert.deepEqual(
    FILTERS.map((filter) => filter.id),
    Object.keys(counts),
  );
});

test("parseNotice splits the label and picks the tone from its last word", () => {
  // Arrange
  const cases = [
    ["\n[Commit] Committed 3 files\n", { body: "Committed 3 files", label: "Commit", tone: "info" }],
    ["[Commit Error] hook failed", { body: "hook failed", label: "Commit Error", tone: "error" }],
    [
      "[Follow-Up Task Error] no task",
      { body: "no task", label: "Follow-Up Task Error", tone: "error" },
    ],
    [
      "[Main Checkout Warning] changed",
      { body: "changed", label: "Main Checkout Warning", tone: "warning" },
    ],
    ["[Sync]", { body: "", label: "Sync", tone: "info" }],
    ["Plain *notice*", { body: "Plain *notice*", label: null, tone: "plain" }],
    ["[1] numbered", { body: "[1] numbered", label: null, tone: "plain" }],
    ["[Unclosed label", { body: "[Unclosed label", label: null, tone: "plain" }],
  ];

  // Act
  const parsed = cases.map(([content]) => parseNotice(content));

  // Assert
  assert.deepEqual(
    parsed,
    cases.map(([, expected]) => expected),
  );
});

test("parseActivity keeps non-blank lines and colours them by their first character", () => {
  // Arrange
  const content = "\n✓ Read file\n✗ Test failed  \n\n! Retrying\n… Running\nUsed 2 tools\n";

  // Act
  const lines = parseActivity(content);

  // Assert
  assert.deepEqual(lines, [
    { text: "✓ Read file", tone: "success" },
    { text: "✗ Test failed", tone: "error" },
    { text: "! Retrying", tone: "warning" },
    { text: "… Running", tone: "info" },
    { text: "Used 2 tools", tone: "muted" },
  ]);
  assert.deepEqual(parseActivity(""), []);
});

test("parseSeen reads a stored map and refuses anything else", () => {
  // Arrange
  const cases = [
    ['{"a":10}', { a: 10 }],
    [null, null],
    ["", null],
    ["not json", null],
    ["[1,2]", null],
    ["7", null],
    ["null", null],
  ];

  // Act
  const parsed = cases.map(([text]) => parseSeen(text));

  // Assert
  assert.deepEqual(
    parsed,
    cases.map(([, expected]) => expected),
  );
});

test("reconcileSeen marks everything seen on the first load", () => {
  // Arrange
  const sessions = [session("a", "Review", "A", 10), session("b", "Done", "B", 20)];

  // Act
  const seen = reconcileSeen(null, sessions, null);

  // Assert
  assert.deepEqual(seen, { a: 10, b: 20 });
  assert.deepEqual([...unreadIds(sessions, seen, null)], []);
});

test("reconcileSeen keeps old values, updates the open session and drops unlisted ids", () => {
  // Arrange
  const stored = { a: 10, b: 20, gone: 5 };
  const sessions = [
    session("a", "Review", "A", 15),
    session("b", "Review", "B", 25),
    session("new", "Draft", "New", 30),
  ];

  // Act
  const seen = reconcileSeen(stored, sessions, "b");

  // Assert
  assert.deepEqual(seen, { a: 10, b: 25 });
  assert.deepEqual(stored, { a: 10, b: 20, gone: 5 });
});

test("unreadIds lists sessions updated since seen, and new ones, but never the open one", () => {
  // Arrange
  const seen = { a: 10, b: 20, open: 1 };
  const sessions = [
    session("a", "Review", "A", 10),
    session("b", "Review", "B", 21),
    session("new", "Draft", "New", 5),
    session("open", "InProgress", "Open", 50),
  ];

  // Act
  const unread = unreadIds(sessions, seen, "open");
  const withoutStorage = unreadIds(sessions, null, "open");

  // Assert
  assert.deepEqual([...unread], ["b", "new"]);
  assert.deepEqual([...withoutStorage], []);
});
