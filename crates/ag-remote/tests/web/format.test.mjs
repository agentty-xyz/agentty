import assert from "node:assert/strict";
import test from "node:test";

import {
  FILTERS,
  GROUPS,
  MAX_SEEN,
  activeSeconds,
  filterCounts,
  filterSessions,
  formatTimer,
  groupSessions,
  mergeSeen,
  modelLabel,
  parseActivity,
  parseNotice,
  parseSeen,
  reconcileSeen,
  relativeTime,
  reviewLabel,
  sessionFigures,
  settingsParts,
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
    // Rounding up to a whole hour moves to the next unit, never `60m`.
    [now - (59 * 60 + 30), "1h"],
    [now - (59 * 60 + 59), "1h"],
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

test("reconcileSeen keeps old values and unlisted ids, and updates the open session", () => {
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
  assert.deepEqual(seen, { a: 10, b: 25, gone: 5 });
  assert.deepEqual(stored, { a: 10, b: 20, gone: 5 });
  assert.deepEqual([...unreadIds(sessions, seen, "b")], ["a", "new"]);
});

test("switching to another project and back marks nothing unread", () => {
  // Arrange
  const projectA = [session("a1", "Review", "A1", 10), session("a2", "Done", "A2", 20)];
  const projectB = [session("b1", "Review", "B1", 30), session("b2", "Draft", "B2", 40)];
  const seenA = reconcileSeen(null, projectA, null);

  // Act: the terminal switches to B, which this browser never saw, then
  // back to A.
  const seenB = reconcileSeen(seenA, projectB, null);
  const unreadB = unreadIds(projectB, seenB, null);
  const seenBack = reconcileSeen(seenB, projectA, null);

  // Assert
  assert.deepEqual([...unreadB], []);
  assert.deepEqual(seenBack, { a1: 10, a2: 20, b1: 30, b2: 40 });
  assert.deepEqual([...unreadIds(projectA, seenBack, null)], []);
});

test("a stored map that knows none of the listed sessions counts as a first load", () => {
  // Arrange: this browser paired again with another Agentty.
  const stored = { old1: 10, old2: 20 };
  const sessions = [session("x", "Review", "X", 50), session("y", "Question", "Y", 60)];

  // Act
  const seen = reconcileSeen(stored, sessions, null);
  const emptyListing = reconcileSeen(stored, [], null);

  // Assert
  assert.deepEqual([...unreadIds(sessions, seen, null)], []);
  assert.deepEqual(seen, { old1: 10, old2: 20, x: 50, y: 60 });
  assert.deepEqual(emptyListing, stored);
});

test("reconcileSeen keeps listed ids and the most recent others when the map grows too large", () => {
  // Arrange
  const stored = Object.fromEntries(
    Array.from({ length: MAX_SEEN + 10 }, (_, index) => ["old" + index, index]),
  );
  stored.listed = 0;
  const sessions = [session("listed", "Done", "Listed", 0)];

  // Act
  const seen = reconcileSeen(stored, sessions, null);

  // Assert
  assert.equal(Object.keys(seen).length, MAX_SEEN);
  assert.equal(seen.listed, 0);
  assert.equal(Object.hasOwn(seen, "old" + (MAX_SEEN + 9)), true);
  assert.equal(Object.hasOwn(seen, "old10"), false);
});

test("mergeSeen keeps the later time of each session and ignores non-numbers", () => {
  // Arrange
  const first = { a: 10, b: 30, bad: "x" };
  const second = { a: 20, b: 5, c: 1 };

  // Act
  const merged = mergeSeen(first, second);

  // Assert
  assert.deepEqual(merged, { a: 20, b: 30, c: 1 });
  assert.deepEqual(mergeSeen(null, second), { a: 20, b: 5, c: 1 });
  assert.equal(mergeSeen(null, null), null);
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

test("formatTimer counts like the terminal's timer", () => {
  assert.equal(formatTimer(0), "0s");
  assert.equal(formatTimer(-5), "0s");
  assert.equal(formatTimer(12.8), "12s");
  assert.equal(formatTimer(252), "4m 12s");
  assert.equal(formatTimer(3600), "1h 0s");
  assert.equal(formatTimer(3723), "1h 2m 3s");
});

test("activeSeconds adds the open interval to earlier work, only while running", () => {
  const running = { in_progress_started_at: 100, in_progress_total_seconds: 30 };

  assert.equal(activeSeconds(running, 160), 90);
  assert.equal(activeSeconds({ in_progress_started_at: 100 }, 160), 60);
  // A clock behind the host's never counts backwards.
  assert.equal(activeSeconds(running, 90), 30);
  assert.equal(activeSeconds({ in_progress_total_seconds: 30 }, 160), null);
  assert.equal(activeSeconds(null, 160), null);
});

test("sessionFigures reads the listing's figures and hides what an older host omits", () => {
  assert.deepEqual(
    sessionFigures({
      added_lines: 64,
      deleted_lines: 9,
      model: "claude-opus-5-5",
      model_short_name: "opus-5.5",
      review_ready: "ready",
      size: "M",
    }),
    { isPrReady: true, lines: { added: 64, deleted: 9 }, model: "opus-5.5", size: "M" },
  );
  assert.deepEqual(sessionFigures({ id: "old", status: "Review" }), {
    isPrReady: false,
    lines: null,
    model: null,
    size: null,
  });
  assert.equal(sessionFigures(null).size, null);
  // No changes yet, so no counts; the model id stands in for a short name.
  const fresh = sessionFigures({ added_lines: 0, deleted_lines: 0, model: "gpt-5.4", size: "XS" });
  assert.equal(fresh.lines, null);
  assert.equal(fresh.model, "gpt-5.4");
  assert.equal(sessionFigures({ review_ready: "ready_with_bypass" }).isPrReady, true);
  assert.equal(sessionFigures({ review_ready: "blocked" }).isPrReady, false);
});

test("modelLabel joins the agent and the listing's short name for the same model", () => {
  const session = { agent: "claude", model: "claude-opus-5-5" };
  const summary = { model: "claude-opus-5-5", model_short_name: "opus-5.5" };
  const stale = { model: "gpt-5.4", model_short_name: "gpt-5.4" };

  assert.equal(modelLabel(session, summary), "claude/opus-5.5");
  assert.equal(modelLabel(session), "claude-opus-5-5");
  // A listing from before a model change does not rename the session's.
  assert.equal(modelLabel(session, stale), "claude-opus-5-5");
  assert.equal(modelLabel({ model: "claude-opus-5-5" }, summary), "claude-opus-5-5");
  assert.equal(modelLabel({ agent: "claude" }, summary), null);
  assert.equal(modelLabel(null), null);
});

test("settingsParts lists model and reasoning, style and speed, leaving out what is missing", () => {
  const session = {
    agent: "claude",
    model: "claude-opus-5-5",
    reasoning: "high",
    response_style: "balanced",
    speed_mode: "normal",
  };

  assert.deepEqual(settingsParts(session), [
    { label: "claude-opus-5-5", reasoning: "high", type: "model" },
    { label: "balanced", type: "style" },
    { label: "normal speed", type: "speed" },
  ]);
  assert.deepEqual(settingsParts({ model: "gpt-5.4" }), [
    { label: "gpt-5.4", reasoning: null, type: "model" },
  ]);
  assert.deepEqual(settingsParts({ id: "old" }), []);
  assert.deepEqual(settingsParts(null), []);
});

test("reviewLabel names the request, its state and the forge summary", () => {
  assert.equal(
    reviewLabel({ display_id: "#1261", state: "Open", status_summary: "PR ready" }),
    "#1261 open · PR ready",
  );
  assert.equal(reviewLabel({ display_id: "!42", state: "Merged" }), "!42 merged");
  assert.equal(reviewLabel({ display_id: "#7" }), "#7");
  assert.equal(reviewLabel({ state: "Open" }), null);
  assert.equal(reviewLabel(null), null);
});
