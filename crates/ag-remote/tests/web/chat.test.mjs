import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import {
  DRAFT_GRAPH_KEY_PREFIX,
  DRAFT_KEY_PREFIX,
  GRAPH_PLACEHOLDER,
  composerKeyAction,
  composerLabels,
  composerRows,
  historyKeyAction,
  promptHistory,
} from "../../assets/composer.mjs";
import {
  GRAPH_IMAGES_NOTE,
  clockSkew,
  diffKey,
  collectAnswers,
  composerState,
  connectionLostError,
  createChat,
  createRefreshGate,
  isConnectionLost,
  mergeTarget,
  messageKind,
  graphQuestionsKey,
  graphQuestionsSource,
  publishBranch,
  questionsKey,
  sessionActions,
  shownQuestions,
  transcriptChanges,
  transcriptEntries,
  wrappedParts,
} from "../../assets/chat.mjs";
import { createDiff } from "../../assets/diff.mjs";
import { createGraph } from "../../assets/graph.mjs";
import { parsePairingLink } from "../../assets/relay.mjs";
import { createSettings } from "../../assets/settings.mjs";
import { NARROW_QUERY, createShell } from "../../assets/shell.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

// Element names `createChat` takes, by the id each has in index.html.
const ELEMENT_IDS = {
  actionError: "action-error",
  actionNote: "action-note",
  answerBar: "answer-bar",
  answerChat: "answer-chat",
  answerCount: "answer-count",
  answerError: "answer-error",
  answerSend: "answer-send",
  badge: "session-status",
  cancel: "cancel",
  cancelDialog: "cancel-dialog",
  composer: "composer",
  composerGraph: "composer-graph",
  composerGraphLeave: "composer-graph-leave",
  composerGraphNote: "composer-graph-note",
  composerHint: "composer-hint",
  composerImages: "composer-images",
  composerMeta: "composer-meta",
  composerSettings: "composer-settings",
  diff: "diff",
  dock: "dock",
  draftPreview: "draft-preview",
  focusedReview: "focused-review",
  gone: "session-gone",
  goneHome: "session-gone-home",
  graphSummary: "graph-summary",
  graphs: "graphs",
  heading: "heading",
  jump: "jump",
  merge: "merge",
  mergeDialog: "merge-dialog",
  mergeLines: "merge-lines",
  mergeTarget: "merge-target",
  message: "message",
  more: "session-more",
  publish: "publish",
  publishBranch: "publish-branch",
  publishDialog: "publish-dialog",
  publishLabel: "publish-label",
  publishReview: "publish-review",
  publishWait: "publish-wait",
  questions: "questions",
  queued: "queued",
  queuedBox: "queued-box",
  readonly: "read-only",
  scroller: "chat-scroll",
  send: "send",
  sessionError: "session-error",
  sheet: "session-sheet",
  sheetDiff: "sheet-diff",
  sheetDiffLabel: "sheet-diff-label",
  sheetGraphs: "sheet-graphs",
  sheetGraphsLabel: "sheet-graphs-label",
  sheetMerge: "sheet-merge",
  sheetMergeLabel: "sheet-merge-label",
  sheetMergeNote: "sheet-merge-note",
  sheetPublish: "sheet-publish",
  sheetPublishNote: "sheet-publish-note",
  sheetDetails: "sheet-details",
  sheetSub: "sheet-sub",
  sheetSync: "sheet-sync",
  sheetSyncLabel: "sheet-sync-label",
  sheetTitle: "sheet-title",
  statusLine: "status-line",
  stop: "stop",
  sub: "heading-sub",
  syncDialog: "sync-dialog",
  syncTarget: "sync-target",
  transcript: "transcript",
};

// Element names `createGraph` takes, by the id each has in index.html.
const GRAPH_IDS = {
  actions: "graph-actions",
  cancelDialog: "graph-cancel-dialog",
  detail: "graph-detail",
  error: "graph-error",
  hints: "graph-hints",
  next: "graph-next",
  nodes: "graph-nodes",
  note: "graph-note",
  position: "graph-position",
  prev: "graph-prev",
  preview: "graph-preview",
  refresh: "graph-refresh",
  title: "graph-title",
  view: "graph-view",
};

// Element names `createSettings` takes, by the id each has in index.html.
const SETTINGS_IDS = {
  agent: "settings-agent",
  agentRow: "settings-agent-row",
  apply: "settings-apply",
  dialog: "settings-dialog",
  error: "settings-error",
  form: "settings-form",
  model: "settings-model",
  note: "settings-note",
  reasoning: "settings-reasoning",
  speed: "settings-speed",
  speedRow: "settings-speed-row",
  style: "settings-style",
};

function session(overrides = {}) {
  return {
    id: "s1",
    messages: [
      { content: "Build it", kind: "user" },
      { content: "Done.", kind: "assistant" },
    ],
    questions: [],
    queued_messages: [],
    status: "Review",
    title: "Serve the page",
    updated_at: 10,
    ...overrides,
  };
}

// Mounts the chat on the client's page. `respond(body)` answers each
// request; the requests sent are recorded. `ResizeObserver`, when given,
// stands in for the browser's.
function mountChat({
  capabilities = [],
  isFinePointer = true,
  isNarrow = false,
  now = () => 0,
  respond = () => ({ kind: "accepted" }),
  sessionStorage = new MemoryStorage(),
  ResizeObserver = undefined,
  url = "http://127.0.0.1/",
} = {}) {
  const window = createWindow(createDocument(INDEX_HTML), url, {
    matches: (query) =>
      (query === "(pointer: fine)" && isFinePointer) || (query === NARROW_QUERY && isNarrow),
  });
  window.sessionStorage = sessionStorage;
  window.ResizeObserver = ResizeObserver;
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const elements = Object.fromEntries(
    Object.entries(ELEMENT_IDS).map(([name, id]) => [name, byId(id)]),
  );
  const requests = [];
  let homes = 0;
  let refreshes = 0;
  const request = async (body) => {
    requests.push(body);

    return respond(body);
  };
  const chat = createChat({
    diff: createDiff({
      document: document,
      elements: {
        error: byId("diff-error"),
        files: byId("diff-files"),
        summary: byId("diff-summary"),
        view: byId("diff-view"),
      },
      request: request,
    }),
    document: document,
    elements: elements,
    graph: createGraph({
      document: document,
      elements: Object.fromEntries(Object.entries(GRAPH_IDS).map(([name, id]) => [name, byId(id)])),
      request: request,
      window: window,
    }),
    now: now,
    onHome: () => {
      homes += 1;
    },
    onRefresh: () => {
      refreshes += 1;
    },
    request: request,
    settings: createSettings({
      document: document,
      elements: Object.fromEntries(Object.entries(SETTINGS_IDS).map(([name, id]) => [name, byId(id)])),
      request: request,
    }),
    window: window,
  });
  chat.setCapabilities(capabilities);
  chat.setConnected(true);
  chat.open("s1");

  return { byId, chat, document, homes: () => homes, refreshes: () => refreshes, requests, window };
}

// Lets pending promise callbacks run.
function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

test("message kinds map to their renderings, and unknown kinds become notices", () => {
  assert.equal(messageKind({ kind: "user" }), "user");
  assert.equal(messageKind({ kind: "assistant" }), "assistant");
  assert.equal(messageKind({ kind: "activity" }), "activity");
  assert.equal(messageKind({ kind: "notice" }), "notice");
  assert.equal(messageKind({ kind: "review_output" }), "notice");
  assert.equal(messageKind(null), "notice");

  const { byId, chat } = mountChat();
  chat.render(
    session({
      messages: [
        { content: "<img src=x onerror=alert(1)>", kind: "user" },
        { content: "line 1\nline 2", kind: "assistant" },
        { content: "From a newer host", kind: "plan" },
      ],
    }),
  );
  const items = byId("transcript").children;
  assert.deepEqual(
    Array.from(items, (item) => [item.className, item.dataset.kind]),
    [
      ["msg msg--user", "user"],
      ["msg msg--assistant", "assistant"],
      ["msg msg--notice", "notice"],
    ],
  );
  // Untrusted text stays text: no element comes out of it.
  assert.equal(items[0].textContent, "<img src=x onerror=alert(1)>");
  assert.equal(items[0].children.length, 0);
  // An answer renders as Markdown, and keeps its source a toggle away.
  const rendered = items[1].querySelector(".md");
  const source = items[1].querySelector(".md-source");
  const toggle = items[1].querySelector(".source-toggle");
  assert.deepEqual(
    Array.from(rendered.children, (child) => child.localName),
    ["p"],
  );
  assert.equal(rendered.querySelectorAll("br").length, 1);
  assert.equal(source.textContent, "line 1\nline 2");
  assert.equal(source.hidden, true);
  toggle.click();
  assert.deepEqual([rendered.hidden, source.hidden, toggle.textContent], [true, false, "View rendered"]);
  assert.equal(toggle.getAttribute("aria-pressed"), "true");
  toggle.click();
  assert.deepEqual([rendered.hidden, source.hidden, toggle.textContent], [false, true, "View source"]);
});

test("an answer keeps showing its source while it grows, and an unchanged one keeps its node", () => {
  const { byId, chat } = mountChat();
  const messages = [
    { content: "Build it", kind: "user" },
    { content: "**Done**", kind: "assistant" },
  ];
  chat.render(session({ messages: messages }));
  const [first, answer] = byId("transcript").children;
  answer.querySelector(".source-toggle").click();

  chat.render(session({ messages: [messages[0], { content: "**Done** and `tested`", kind: "assistant" }] }));

  const [firstAfter, answerAfter] = byId("transcript").children;
  assert.equal(firstAfter, first);
  assert.notEqual(answerAfter, answer);
  assert.equal(answerAfter.querySelector(".md").hidden, true);
  assert.equal(answerAfter.querySelector(".md-source").textContent, "**Done** and `tested`");
  assert.equal(answerAfter.querySelector(".md code").textContent, "tested");

  chat.render(session({ messages: [messages[0], { content: "**Done** and `tested`", kind: "assistant" }] }));

  assert.equal(byId("transcript").children[1], answerAfter);
});

test("notices and question text render inline Markdown, and raw HTML stays text", () => {
  const { byId, chat } = mountChat();
  chat.render(
    session({
      messages: [{ content: "[Review Request] Opened <https://example.com/1> from `wt/3f9a`", kind: "notice" }],
      questions: [{ options: [], text: "Reject `insertAdjacentHTML` too? <img src=x onerror=alert(1)>" }],
      status: "Question",
    }),
  );
  const body = byId("transcript").querySelector(".notice-body");
  // Rendered Markdown sits in `.prose`, which styles its bare code elements.
  assert.ok(body.classList.contains("prose"));
  assert.equal(body.querySelector("code").textContent, "wt/3f9a");
  assert.equal(body.querySelector("a").getAttribute("href"), "https://example.com/1");
  const question = byId("questions").querySelector(".question-text");
  assert.equal(question.querySelector("code").textContent, "insertAdjacentHTML");
  assert.equal(question.querySelector("img"), null);
  assert.ok(question.textContent.endsWith("<img src=x onerror=alert(1)>"));
});

test("a notice's label becomes a tagged prefix, and its tone marks the row", () => {
  const { byId, chat } = mountChat();
  chat.render(
    session({
      messages: [
        { content: "[Commit] Serve the page", kind: "notice" },
        { content: "[Branch Push Error] rejected", kind: "notice" },
        { content: "[Commit Warning] no hook", kind: "notice" },
        { content: "Plain notice", kind: "notice" },
      ],
    }),
  );
  const rows = Array.from(byId("transcript").children, (item) => ({
    classes: item.className,
    label: item.querySelector(".notice-label")?.textContent ?? null,
    text: item.querySelector(".notice-body").textContent,
  }));
  assert.deepEqual(rows, [
    { classes: "msg msg--notice", label: "Commit", text: "Serve the page" },
    { classes: "msg msg--notice is-error", label: "Branch Push Error", text: "rejected" },
    { classes: "msg msg--notice is-warning", label: "Commit Warning", text: "no hook" },
    { classes: "msg msg--notice", label: null, text: "Plain notice" },
  ]);
});

test("activity lines take the tone of their first character, and long boxes start closed", () => {
  const { byId, chat } = mountChat();
  chat.render(
    session({
      messages: [
        { content: "✓ Read a\n✗ Run b\n! Retry c", kind: "activity" },
        { content: "✓ a\n✓ b\n… c\nplain d", kind: "activity" },
      ],
    }),
  );
  const [short, long] = byId("transcript").querySelectorAll("details");
  assert.equal(short.open, true);
  assert.equal(long.open, false);
  assert.equal(short.querySelector("summary").textContent, "Activity3 lines");
  assert.deepEqual(
    Array.from(long.querySelectorAll("li"), (line) => line.className),
    ["tone-success", "tone-success", "tone-info", "tone-muted"],
  );
  assert.deepEqual(
    Array.from(short.querySelectorAll("li"), (line) => line.className),
    ["tone-success", "tone-error", "tone-warning"],
  );
});

test("a turn summary lays out its rows in columns and stays open when short", () => {
  const rows = ["Edited\t✓\tsrc/lib.rs\t+3 −1\t1 edit", "Failed\t✗\tcargo build\texit 1\t"];
  const summary = (body) =>
    ["turn-summary v1", "2 tools · 0s", ...body, "─ Worked 0s · 2 tools · 1 failed (1 nonzero exit)"].join(
      "\n",
    );
  const { byId, chat } = mountChat();
  chat.render(
    session({
      messages: [
        { content: summary([...rows, "Ran\t2 commands"]), kind: "activity" },
        { content: summary(Array(9).fill(rows[0])), kind: "activity" },
      ],
    }),
  );
  const [short, long] = byId("transcript").querySelectorAll("details");
  assert.equal(short.open, true);
  assert.equal(long.open, false);
  assert.equal(short.querySelector("summary").textContent, "Activity2 tools · 0s");
  assert.deepEqual(
    Array.from(short.querySelectorAll("li"), (line) => [
      line.className,
      Array.from(line.children, (cell) => [cell.className, cell.textContent]),
    ]),
    [
      [
        "summary-row",
        [
          ["summary-heading", "Edited"],
          ["tone-success", "✓"],
          ["summary-target", "src/lib.rs"],
          ["summary-result", "+3 −1"],
          ["tone-muted", "1 edit"],
        ],
      ],
      [
        "summary-row",
        [
          ["summary-heading", "Failed"],
          ["tone-error", "✗"],
          ["summary-target", "cargo build"],
          ["summary-result", "exit 1"],
          ["tone-muted", ""],
        ],
      ],
      [
        "summary-row",
        [
          ["summary-heading", "Ran"],
          ["summary-text", "2 commands"],
        ],
      ],
      ["summary-divider", []],
    ],
  );
  assert.equal(
    short.querySelector(".summary-divider").textContent,
    "─ Worked 0s · 2 tools · 1 failed (1 nonzero exit)",
  );
  // Added lines are green and removed ones red, as in the terminal.
  assert.deepEqual(
    Array.from(short.querySelectorAll(".summary-result"), (cell) =>
      Array.from(cell.children, (part) => [part.className, part.textContent]),
    ),
    [
      [
        ["tone-success", "+3"],
        ["tone-error", "−1"],
      ],
      [["tone-error", "exit 1"]],
    ],
  );
});

test("the transcript only appends when it grows, and keeps the nodes on screen", () => {
  const first = transcriptEntries(session().messages);
  const grown = transcriptEntries([...session().messages, { content: "More", kind: "user" }]);
  assert.deepEqual(transcriptChanges(first, grown), { append: [2], remove: 0, replace: [] });
  const edited = transcriptEntries([session().messages[0], { content: "Changed", kind: "assistant" }]);
  assert.deepEqual(transcriptChanges(first, edited), { append: [], remove: 0, replace: [1] });
  assert.deepEqual(transcriptChanges(first, first.slice(0, 1)), {
    append: [],
    remove: 1,
    replace: [],
  });
  // A different kind at the same position is a different message.
  const rekinded = transcriptEntries([session().messages[0], { content: "Done.", kind: "notice" }]);
  assert.deepEqual(transcriptChanges(first, rekinded).replace, [1]);

  const { byId, chat } = mountChat();
  chat.render(session());
  const [user, answer] = byId("transcript").children;
  const composer = byId("message");
  composer.value = "typing";
  composer.focus();
  chat.render(session({ messages: [...session().messages, { content: "Next", kind: "user" }] }));
  const items = byId("transcript").children;
  assert.equal(items.length, 3);
  assert.equal(items[0], user);
  assert.equal(items[1], answer);
  assert.equal(items[2].textContent, "Next");
  // A refresh keeps focus and the text being typed.
  assert.equal(composer.value, "typing");
  assert.equal(composer.ownerDocument.activeElement, composer);
});

test("the transcript ends with what a working session does, and drops it when the turn ends", () => {
  // Arrange
  const { byId, chat } = mountChat();
  const line = byId("status-line");

  // Act
  chat.render(session({ status: "InProgress" }));
  const marker = line.querySelector("i");
  const grown = [...session().messages, { content: "More", kind: "user" }];
  chat.render(session({ messages: grown, status: "InProgress" }));
  const working = { className: line.className, hidden: line.hidden, text: line.textContent };
  const keptMarker = line.querySelector("i") === marker;
  chat.render(session({ status: "Rebasing" }));
  const rebasing = { className: line.className, text: line.textContent };
  chat.render(session({ status: "Review" }));
  const reviewHidden = line.hidden;
  chat.render(session({ status: "Queued" }));
  const queued = { className: line.className, text: line.textContent };
  chat.close();

  // Assert
  assert.deepEqual(working, {
    className: "status status--line status-line status--working status--live",
    hidden: false,
    text: "Working...",
  });
  // A refresh in the same status keeps the node, so the loader animates on.
  assert.equal(keptMarker, true);
  assert.equal(marker.className, "status-dot");
  assert.deepEqual(rebasing, {
    className: "status status--line status-line status--queue status--live",
    text: "Rebasing...",
  });
  assert.equal(reviewHidden, true);
  assert.deepEqual(queued, {
    className: "status status--line status-line status--queue",
    text: "Waiting in merge queue...",
  });
  assert.equal(line.hidden, true);
  assert.equal(line.textContent, "");
});

test("the status line follows the host's progress and keeps its loader while the text changes", () => {
  // Arrange
  const { byId, chat } = mountChat();
  const line = byId("status-line");

  // Act
  chat.render(session({ status: "InProgress" }));
  const marker = line.querySelector("i");
  chat.render(session({ progress: "Working... Reading files", status: "InProgress" }));
  const thinking = line.textContent;
  chat.render(session({ progress: "Resolving 3 review comments...\n  src/app.rs\n", status: "InProgress" }));
  const rows = [...line.querySelectorAll("span span")].map((row) => [row.className, row.textContent]);
  const keptMarker = line.querySelector("i") === marker;
  chat.close();

  // Assert
  assert.equal(thinking, "Working... Reading files");
  assert.deepEqual(rows, [
    ["", "Resolving 3 review comments..."],
    ["status-line-detail", "src/app.rs"],
  ]);
  assert.equal(keptMarker, true);
});

test("an activity box the reader toggled stays that way when it changes", () => {
  const { byId, chat } = mountChat();
  chat.render(session({ messages: [{ content: "✓ a", kind: "activity" }] }));
  const toggled = byId("transcript").querySelector("details");
  // The fake DOM does not toggle on a click; set what the browser would.
  toggled.querySelector("summary").click();
  toggled.open = false;
  chat.render(session({ messages: [{ content: "✓ a\n✓ b", kind: "activity" }] }));
  let details = byId("transcript").querySelector("details");
  assert.equal(details.open, false);
  assert.equal(details.querySelectorAll("li").length, 2);
  // The choice lasts through later changes too.
  chat.render(session({ messages: [{ content: "✓ a\n✓ b\n✓ c", kind: "activity" }] }));
  details = byId("transcript").querySelector("details");
  assert.equal(details.open, false);
});

test("an activity box the reader left alone follows the default as it grows", () => {
  const { byId, chat } = mountChat();
  chat.render(session({ messages: [{ content: "✓ a\n✓ b", kind: "activity" }] }));
  assert.equal(byId("transcript").querySelector("details").open, true);
  chat.render(session({ messages: [{ content: "✓ a\n✓ b\n✓ c\n✓ d\n✓ e", kind: "activity" }] }));
  assert.equal(byId("transcript").querySelector("details").open, false);
});

test("the composer follows the status", () => {
  const cases = [
    ["Draft", "compose", "Describe the task"],
    ["Review", "compose", "Reply to the agent"],
    ["AgentReview", "compose", "Reply to the agent"],
    ["Watching", "compose", "Reply to the agent"],
    ["InProgress", "compose", "Queue a message for the next turn"],
    ["Rebasing", "compose", "Queue a message for the next turn"],
  ];
  for (const [status, mode, placeholder] of cases) {
    const state = composerState(status);
    assert.equal(state.mode, mode, status);
    assert.equal(state.placeholder, placeholder, status);
    assert.equal(state.showStop, false, status);
  }
  assert.deepEqual(composerState("Question", { hasQuestions: true }), { mode: "answer" });
  // A question status without questions keeps the composer.
  assert.equal(composerState("Question").mode, "compose");
  for (const [status, text] of [
    ["Queued", "Queued to merge. Chat is read-only."],
    ["Merging", "Merging. Chat is read-only."],
    ["Merged", "Merged. Chat is read-only."],
    ["Done", "Done. Chat is read-only."],
    ["Canceled", "Canceled. Chat is read-only."],
    ["Archived", "Archived. Chat is read-only."],
  ]) {
    assert.deepEqual(composerState(status), { mode: "readonly", text: text });
  }
  assert.equal(
    composerState("Review", { isConnected: false }).placeholder,
    "Offline. Your draft is kept.",
  );
  assert.equal(composerState("Review").hint, "Enter sends · Shift+Enter adds a line");
  assert.equal(composerState("Review", { isFinePointer: false }).hint, "Enter adds a line");
  assert.equal(
    composerState("InProgress", { capabilities: ["stop_turn"] }).hint,
    "Enter queues the message · Stop returns the session to Review",
  );
});

test("the dock shows the composer, the answer bar, or why chat is read-only", () => {
  const { byId, chat } = mountChat();
  chat.render(session({ status: "Merging" }));
  assert.equal(byId("composer").hidden, true);
  assert.equal(byId("answer-bar").hidden, true);
  assert.equal(byId("read-only").hidden, false);
  assert.equal(byId("read-only").textContent, "Merging. Chat is read-only.");

  chat.render(session({ status: "InProgress" }));
  assert.equal(byId("composer").hidden, false);
  assert.equal(byId("read-only").hidden, true);
  assert.equal(byId("message").placeholder, "Queue a message for the next turn");
  assert.equal(byId("send").getAttribute("aria-label"), "Queue message");
  assert.equal(byId("composer-meta").textContent, "Enter queues the message");

  chat.setConnected(false);
  assert.equal(byId("send").disabled, true);
  assert.equal(byId("message").placeholder, "Offline. Your draft is kept.");
});

test("Enter sends with a fine pointer, adds a line on touch, and never sends mid-composition", () => {
  const fine = { isFinePointer: true };
  const touch = { isFinePointer: false };
  assert.equal(composerKeyAction({ key: "Enter" }, fine), "send");
  assert.equal(composerKeyAction({ key: "Enter", shiftKey: true }, fine), null);
  assert.equal(composerKeyAction({ key: "Enter" }, touch), null);
  assert.equal(composerKeyAction({ ctrlKey: true, key: "Enter" }, touch), "send");
  assert.equal(composerKeyAction({ key: "Enter", metaKey: true }, touch), "send");
  assert.equal(composerKeyAction({ isComposing: true, key: "Enter" }, fine), null);
  assert.equal(composerKeyAction({ key: "Enter", keyCode: 229 }, fine), null);
  assert.equal(composerKeyAction({ ctrlKey: true, isComposing: true, key: "Enter" }, fine), null);
  assert.equal(composerKeyAction({ key: "a" }, fine), null);
});

test("the composer grows from one to eight rows", () => {
  assert.equal(composerRows(""), 1);
  assert.equal(composerRows("a\nb\nc"), 3);
  assert.equal(composerRows("\n".repeat(20)), 8);

  const { byId } = mountChat();
  const message = byId("message");
  message.value = "a\nb";
  message.dispatchEvent(new FakeEvent("input"));
  assert.equal(message.rows, 2);
});

test("typed answers win over chosen options, and an incomplete set cannot be sent", () => {
  const questions = [{ text: "Where?" }, { text: "Which?" }];
  assert.deepEqual(collectAnswers(questions, ["CI", null], ["", ""]), {
    answered: 1,
    answers: [
      { answer: "CI", question: "Where?" },
      { answer: "", question: "Which?" },
    ],
    isComplete: false,
  });
  assert.deepEqual(collectAnswers(questions, ["CI", "Yes"], ["  locally ", ""]).answers, [
    { answer: "locally", question: "Where?" },
    { answer: "Yes", question: "Which?" },
  ]);
  assert.equal(collectAnswers([], [], []).isComplete, false);
});

test("the questions panel collects answers, survives refreshes, and sends the set", async () => {
  const { byId, chat, requests } = mountChat();
  const questions = [
    { options: ["CI", "Locally"], text: "Where should it run?" },
    { options: [], text: "Anything else?" },
  ];
  chat.render(session({ questions: questions, status: "Question" }));
  assert.equal(byId("composer").hidden, true);
  assert.equal(byId("answer-bar").hidden, false);
  assert.deepEqual(
    Array.from(byId("questions").querySelectorAll(".eyebrow"), (node) => node.textContent),
    ["Question 1 of 2", "Question 2 of 2"],
  );
  const options = byId("questions").querySelectorAll(".option");
  assert.equal(options[0].querySelector("small").textContent, "recommended");
  assert.equal(options[1].querySelector("small"), null);
  const inputs = byId("questions").querySelectorAll("input");
  assert.deepEqual(
    Array.from(inputs, (input) => input.placeholder),
    ["Or type an answer", "Your answer"],
  );
  assert.equal(byId("answer-count").textContent, "0 of 2 answered");
  assert.equal(byId("answer-send").disabled, true);

  options[1].click();
  assert.deepEqual(
    Array.from(options, (option) => option.getAttribute("aria-pressed")),
    ["false", "true"],
  );
  assert.equal(byId("answer-count").textContent, "1 of 2 answered");
  // The same set arriving again keeps the choice.
  chat.render(session({ questions: questions, status: "Question" }));
  assert.equal(byId("questions").querySelectorAll(".option")[1], options[1]);
  assert.equal(byId("answer-send").disabled, true);

  // Submitting an incomplete set says why and sends nothing.
  byId("questions").requestSubmit();
  await settle();
  assert.equal(byId("answer-error").textContent, "Answer every question first.");
  assert.deepEqual(requests, []);

  inputs[1].value = "No";
  inputs[1].dispatchEvent(new FakeEvent("input"));
  assert.equal(byId("answer-send").disabled, false);
  byId("answer-send").click();
  await settle();
  assert.deepEqual(requests, [
    {
      answers: [
        { answer: "Locally", question: "Where should it run?" },
        { answer: "No", question: "Anything else?" },
      ],
      method: "answer_questions",
      session_id: "s1",
    },
  ]);
  assert.equal(byId("answer-error").textContent, "");
});

const ANSWER_QUESTIONS = [
  { options: ["CI", "Locally"], text: "Where should it run?" },
  { options: [], text: "Which file?" },
];

// Types `text` into answer field `input`, with the caret at its end.
function typeAnswer(input, text) {
  input.value = text;
  input.dispatchEvent(new FakeEvent("input"));
}

test("@ in a typed answer lists the session's files, and Enter picks one instead of sending", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["files"],
    respond: (body) =>
      body.method === "list_session_files"
        ? {
            files: [
              { is_dir: true, path: "src" },
              { is_dir: false, path: "src/main.rs" },
            ],
            kind: "session_files",
          }
        : { kind: "accepted" },
  });
  chat.render(session({ questions: ANSWER_QUESTIONS, status: "Question" }));
  const inputs = byId("questions").querySelectorAll("input");
  const list = byId("answer-suggestions-1");
  // Each answer field holds its own list, right after it.
  assert.ok(inputs[1].nextSibling.querySelector("#answer-suggestions-1") === list);
  assert.ok(byId("answer-suggestions-0") !== null);

  inputs[0].value = "Locally";
  inputs[0].dispatchEvent(new FakeEvent("input"));
  typeAnswer(inputs[1], "@mai");
  await settle();
  assert.deepEqual(Array.from(list.children, (option) => option.dataset.key), ["src/main.rs"]);
  assert.equal(inputs[1].getAttribute("aria-expanded"), "true");
  assert.equal(byId("answer-count").textContent, "2 of 2 answered");

  const enter = new FakeEvent("keydown", { key: "Enter" });
  inputs[1].dispatchEvent(enter);
  assert.equal(enter.defaultPrevented, true);
  assert.equal(inputs[1].value, "@src/main.rs ");
  assert.equal(inputs[1].selectionStart, inputs[1].value.length);
  assert.equal(inputs[1].getAttribute("aria-expanded"), "false");
  // With the list closed, the keys are the field's again.
  const second = new FakeEvent("keydown", { key: "Enter" });
  inputs[1].dispatchEvent(second);
  assert.equal(second.defaultPrevented, false);

  byId("answer-send").click();
  await settle();
  assert.deepEqual(requests, [
    { method: "list_session_files", session_id: "s1" },
    {
      answers: [
        { answer: "Locally", question: "Where should it run?" },
        { answer: "@src/main.rs", question: "Which file?" },
      ],
      method: "answer_questions",
      session_id: "s1",
    },
  ]);
});

test("the answer file list closes on blur, offline, and with new questions, and needs files", async () => {
  const plain = mountChat();
  plain.chat.render(session({ questions: ANSWER_QUESTIONS, status: "Question" }));
  typeAnswer(plain.byId("questions").querySelectorAll("input")[1], "@src");
  await settle();
  assert.equal(plain.byId("answer-suggestions-1").children.length, 0);
  assert.deepEqual(plain.requests, []);

  let failLoad = false;
  const { byId, chat, requests } = mountChat({
    capabilities: ["files"],
    respond: () => {
      if (failLoad) {
        throw new Error("Worktree is gone");
      }

      return { files: [{ is_dir: false, path: "a.rs" }], kind: "session_files" };
    },
  });
  chat.render(session({ questions: ANSWER_QUESTIONS, status: "Question" }));
  let input = byId("questions").querySelectorAll("input")[1];
  const isOpen = () => input.getAttribute("aria-expanded") === "true";
  typeAnswer(input, "@");
  await settle();
  assert.equal(isOpen(), true);
  input.dispatchEvent(new FakeEvent("blur", { bubbles: false }));
  assert.equal(isOpen(), false);
  input.focus();
  await settle();
  assert.equal(isOpen(), true);

  chat.setConnected(false);
  assert.equal(isOpen(), false);
  chat.setConnected(true);
  input.dispatchEvent(new FakeEvent("keyup", { key: "@" }));
  await settle();
  assert.equal(isOpen(), true);

  // A new set of questions starts with closed lists; a failed load says
  // why under the answers.
  chat.render(session({ questions: [{ options: [], text: "Which test?" }], status: "Question" }));
  input = byId("questions").querySelectorAll("input")[0];
  failLoad = true;
  typeAnswer(input, "@");
  await settle();
  assert.equal(isOpen(), false);
  assert.equal(byId("answer-error").textContent, "Could not list the files: Worktree is gone");
  assert.equal(requests.filter((body) => body.method === "list_session_files").length, 4);
});

test("a failed send keeps the text and the draft, and a later success clears both", async () => {
  let shouldFail = true;
  const storage = new MemoryStorage();
  const { byId, chat, requests } = mountChat({
    respond: () => {
      if (shouldFail) {
        throw new Error("Session is busy");
      }

      return { kind: "accepted" };
    },
    sessionStorage: storage,
  });
  chat.render(session());
  const message = byId("message");
  message.value = "Ship it";
  message.dispatchEvent(new FakeEvent("input"));
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "Ship it");

  const enter = new FakeEvent("keydown", { key: "Enter" });
  message.dispatchEvent(enter);
  assert.equal(enter.defaultPrevented, true);
  // The button stays disabled while the request runs.
  assert.equal(byId("send").disabled, true);
  await settle();
  assert.equal(message.value, "Ship it");
  assert.equal(byId("session-error").textContent, "Session is busy");
  assert.equal(byId("send").disabled, false);
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "Ship it");

  shouldFail = false;
  byId("send").click();
  await settle();
  assert.equal(message.value, "");
  assert.equal(byId("session-error").textContent, "");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), null);
  assert.deepEqual(
    requests.map((body) => body.method + ":" + body.session_id + ":" + body.message),
    ["send_message:s1:Ship it", "send_message:s1:Ship it"],
  );
});

test("each session keeps its own draft across switches and reloads", () => {
  const storage = new MemoryStorage();
  const { byId, chat } = mountChat({ sessionStorage: storage });
  const message = byId("message");
  message.value = "for s1";
  message.dispatchEvent(new FakeEvent("input"));
  chat.open("s2");
  assert.equal(message.value, "");
  chat.open("s1");
  assert.equal(message.value, "for s1");

  // A new page in the same tab reads the stored draft.
  const reloaded = mountChat({ sessionStorage: storage });
  assert.equal(reloaded.byId("message").value, "for s1");

  // Without storage the draft lasts for the page.
  const broken = mountChat({ sessionStorage: new MemoryStorage({ failing: true }) });
  broken.byId("message").value = "kept";
  broken.byId("message").dispatchEvent(new FakeEvent("input"));
  broken.chat.open("s2");
  broken.chat.open("s1");
  assert.equal(broken.byId("message").value, "kept");
});

test("a session opened after a failed first message keeps its text and says why", () => {
  const sessionStorage = new MemoryStorage();
  const { byId, chat } = mountChat({ sessionStorage });
  chat.open("new", { draft: "Fix the relay", error: "Session is busy" });

  assert.equal(byId("message").value, "Fix the relay");
  assert.equal(byId("session-error").textContent, "Session is busy");
  assert.equal(sessionStorage.getItem(DRAFT_KEY_PREFIX + "new"), "Fix the relay");
});

test("a draft kept for a session that is not open shows when it opens", () => {
  const { byId, chat } = mountChat();
  chat.keepDraft("new", "Fix the relay");
  assert.equal(byId("message").value, "");

  chat.open("new");
  assert.equal(byId("message").value, "Fix the relay");
  assert.equal(byId("session-error").textContent, "");
});

test("blank messages and messages while offline are not sent", async () => {
  const { byId, chat, requests } = mountChat();
  chat.render(session());
  byId("message").value = "   ";
  byId("send").click();
  chat.setConnected(false);
  byId("message").value = "Ship it";
  byId("composer").requestSubmit();
  await settle();
  assert.deepEqual(requests, []);
  assert.equal(byId("message").value, "Ship it");
});

test("Diff swaps the transcript and the dock for the session's files and back", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["diff"],
    respond: (body) =>
      body.method === "get_session_diff"
        ? { files: [{ added_lines: 1, deleted_lines: 0, path: "a.rs", status: "added" }], kind: "diff" }
        : { kind: "accepted" },
  });
  chat.render(session({ status: "Draft" }));
  assert.equal(byId("diff").hidden, false);
  assert.equal(byId("diff").disabled, true);
  byId("diff").click();
  assert.deepEqual(requests, []);

  chat.render(session());
  assert.equal(byId("diff").disabled, false);
  byId("diff").click();
  await settle();
  assert.deepEqual(requests, [{ method: "get_session_diff", session_id: "s1" }]);
  assert.equal(byId("diff").getAttribute("aria-pressed"), "true");
  assert.equal(byId("sheet-diff-label").textContent, "Back to chat");
  assert.equal(byId("diff-view").hidden, false);
  assert.equal(byId("chat-scroll").hidden, true);
  assert.equal(byId("dock").hidden, true);
  assert.equal(byId("diff-files").querySelector(".diff-path").textContent, "a.rs");

  // A snapshot of the same session keeps the diff and reloads it only
  // when the session changed.
  chat.render(session());
  await settle();
  assert.equal(requests.length, 1);
  assert.equal(byId("chat-scroll").hidden, true);
  chat.render(session({ updated_at: 11 }));
  await settle();
  assert.equal(requests.length, 2);

  // On a phone the sheet offers the way back.
  byId("session-more").click();
  byId("sheet-diff").click();
  assert.equal(byId("diff-view").hidden, true);
  assert.equal(byId("chat-scroll").hidden, false);
  assert.equal(byId("dock").hidden, false);
  assert.equal(byId("sheet-diff-label").textContent, "Diff");

  // A status without a diff closes it, and so does another session.
  byId("diff").click();
  chat.render(session({ status: "Draft", updated_at: 12 }));
  assert.equal(byId("diff-view").hidden, true);
  chat.render(session({ updated_at: 13 }));
  byId("diff").click();
  chat.open("s2");
  assert.equal(byId("diff-view").hidden, true);
  assert.equal(byId("dock").hidden, false);
});

const GRAPH_SUMMARY = {
  awaiting: [{ node_id: "inspect", text: "inspect awaits your approval; reply in chat to accept it" }],
  done: 1,
  label: "Blocked",
  phase: "approved",
  status: "blocked",
  total: 2,
  version: 3,
};

function graphsAnswer(body) {
  return body.method === "get_session_graphs"
    ? { graphs: [{ id: "graph", revision: 1 }], kind: "graphs" }
    : { kind: "accepted" };
}

test("the graph summary sits above the composer and opens the graphs, which back closes", async () => {
  const plain = mountChat();
  plain.chat.render(session({ graph: GRAPH_SUMMARY }));
  // Without the capability the summary still shows, but opens nothing.
  assert.equal(plain.byId("graph-summary").hidden, false);
  assert.equal(plain.byId("graph-summary").disabled, true);
  assert.equal(plain.byId("graphs").hidden, true);
  assert.equal(plain.byId("sheet-graphs").hidden, true);
  plain.byId("graph-summary").click();
  assert.deepEqual(plain.requests, []);

  const { byId, chat, requests, window } = mountChat({
    capabilities: ["graphs"],
    respond: graphsAnswer,
  });
  chat.render(session());
  assert.equal(byId("graph-summary").hidden, true);
  // Graphs is offered in every status, as the terminal's `w` is.
  assert.equal(byId("graphs").hidden, false);
  assert.equal(byId("sheet-graphs").hidden, false);
  chat.render(session({ graph: GRAPH_SUMMARY }));
  const summary = byId("graph-summary");
  assert.equal(summary.disabled, false);
  assert.equal(summary.querySelector(".graph-summary-title").textContent, "Execution graph · v3");
  assert.equal(summary.querySelector(".graph-summary-count").textContent, "1/2 done");
  assert.equal(summary.querySelector(".graph-summary-status").textContent, "●Blocked");
  assert.equal(
    summary.querySelector(".graph-summary-awaiting").textContent,
    "inspect awaits your approval; reply in chat to accept it",
  );
  const historyLength = window.history.length;

  summary.click();
  await settle();
  assert.deepEqual(requests, [{ method: "get_session_graphs", session_id: "s1" }]);
  assert.equal(byId("graph-view").hidden, false);
  assert.equal(byId("chat-scroll").hidden, true);
  assert.equal(byId("dock").hidden, true);
  assert.equal(byId("graphs").getAttribute("aria-pressed"), "true");
  assert.equal(byId("sheet-graphs-label").textContent, "Back to chat");
  assert.equal(window.history.length, historyLength + 1);
  // Each snapshot reads the open graphs again.
  chat.render(session({ graph: GRAPH_SUMMARY }));
  await settle();
  assert.equal(requests.length, 2);

  // The back gesture returns to the chat.
  window.history.back();
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("chat-scroll").hidden, false);
  assert.equal(byId("dock").hidden, false);
  assert.equal(byId("sheet-graphs-label").textContent, "Graphs");

  // The header button closes them through the same history entry, and on
  // a phone the sheet does.
  byId("graphs").click();
  await settle();
  byId("graphs").click();
  assert.equal(byId("graph-view").hidden, true);
  byId("session-more").click();
  byId("sheet-graphs").click();
  await settle();
  assert.equal(byId("graph-view").hidden, false);
  byId("session-more").click();
  byId("sheet-graphs").click();
  assert.equal(byId("graph-view").hidden, true);

  // Another session or a lost capability closes them.
  byId("graphs").click();
  chat.setCapabilities([]);
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("dock").hidden, false);
  chat.setCapabilities(["graphs"]);
  byId("graphs").click();
  chat.open("s2");
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("graph-summary").hidden, true);
  // A back gesture after that changes nothing more.
  window.history.back();
  assert.equal(byId("graph-view").hidden, true);
});

test("the graphs and the diff replace each other and leave no stale back step", async () => {
  const { byId, chat, window } = mountChat({
    capabilities: ["diff", "graphs"],
    respond: (body) =>
      body.method === "get_session_diff" ? { files: [], kind: "diff" } : graphsAnswer(body),
  });
  chat.render(session());
  byId("graphs").click();
  await settle();
  byId("diff").click();
  await settle();
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("diff-view").hidden, false);
  assert.equal(byId("chat-scroll").hidden, true);
  byId("graphs").click();
  await settle();
  assert.equal(byId("diff-view").hidden, true);
  assert.equal(byId("graph-view").hidden, false);
  assert.equal(byId("dock").hidden, true);

  // After the diff replaced them, the graphs still close on the first try,
  // with their button and with the back gesture, and leave one entry.
  byId("graphs").click();
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("chat-scroll").hidden, false);
  const historyLength = window.history.length;
  for (let round = 0; round < 2; round += 1) {
    byId("graphs").click();
    await settle();
    byId("diff").click();
    await settle();
    byId("graphs").click();
    await settle();
  }
  assert.equal(window.history.length, historyLength);
  window.history.back();
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(byId("diff-view").hidden, true);
  assert.equal(byId("dock").hidden, false);
});

// Mounts the chat on a phone at the route of the open session `s1`, with
// the shell's drawer wired as app.js wires it: a route to the open session
// opens nothing again.
function mountPhone() {
  const page = mountChat({
    capabilities: ["graphs"],
    isNarrow: true,
    respond: graphsAnswer,
    url: "http://127.0.0.1/#s=s1",
  });
  const byId = page.byId;
  const shell = createShell({
    document: page.document,
    elements: {
      app: byId("shell"),
      close: byId("drawer-close"),
      hide: byId("sidebar-hide"),
      main: byId("main"),
      menu: byId("menu"),
      scrim: byId("scrim"),
      sidebar: byId("sidebar"),
    },
    onRoute: (route) => {
      if (route.sessionId !== "s1") {
        page.chat.close();
      }
    },
    parsePairingLink: parsePairingLink,
    window: page.window,
  });
  page.chat.render(session());

  return { ...page, shell };
}

test("on a phone, the open session chosen again in the drawer over the graphs leaves one step to close them", async () => {
  for (const close of ["sheet", "back"]) {
    const { byId, shell, window } = mountPhone();
    byId("session-more").click();
    byId("sheet-graphs").click();
    await settle();
    byId("menu").click();
    assert.equal(shell.isOpen(), true);

    // The drawer's list routes to the session it shows already.
    shell.showSession("s1");
    assert.equal(shell.isOpen(), false);
    assert.equal(byId("graph-view").hidden, false);
    assert.equal(window.history.state?.graphs, true);

    if (close === "sheet") {
      byId("session-more").click();
      byId("sheet-graphs").click();
    } else {
      window.history.back();
    }
    assert.equal(byId("graph-view").hidden, true, close);
    assert.equal(byId("chat-scroll").hidden, false);
    assert.equal(byId("dock").hidden, false);
    assert.equal(window.location.hash, "#s=s1");
    assert.equal(window.history.state, null);
  }
});

test("the graphs close in one step when their entry is not the current one", async () => {
  const { byId, chat, window } = mountChat({ capabilities: ["graphs"], respond: graphsAnswer });
  chat.render(session());
  byId("graphs").click();
  await settle();
  // An entry above theirs: closing goes back over nothing.
  window.history.pushState(null, "", window.location.href);
  byId("graphs").click();
  assert.equal(byId("graph-view").hidden, true);
  assert.equal(window.history.state, null);
  // Their entry, reached later, opens nothing.
  window.history.back();
  assert.equal(window.history.state?.graphs, true);
  assert.equal(byId("graph-view").hidden, true);
});

test("Graphs pressed again before the browser went back asks to go back only once", async () => {
  const { byId, chat, window } = mountChat({ capabilities: ["graphs"], respond: graphsAnswer });
  chat.render(session());
  byId("graphs").click();
  await settle();
  const back = window.history.back;
  let backs = 0;
  window.history.back = () => {
    backs += 1;
  };

  byId("graphs").click();
  byId("graphs").click();
  byId("graphs").click();

  assert.equal(backs, 1);
  // Once the step lands, the chat shows and one press opens them again.
  window.history.back = back;
  back();
  assert.equal(byId("graph-view").hidden, true);
  byId("graphs").click();
  await settle();
  assert.equal(byId("graph-view").hidden, false);
  assert.equal(window.history.state?.graphs, true);
});

test("the graph summary keeps a reader at the end there as it appears or grows", () => {
  const { byId, chat } = mountChat();
  const scroller = byId("chat-scroll");
  layout(scroller, { client: 500, height: 1000, top: 0 });
  chat.render(session());
  assert.equal(scroller.scrollTop, 1000);

  // A reader near the end follows it as the summary appears and grows.
  // The fake DOM has no layout, so the height stays put.
  scroller.scrollTop = 460;
  chat.render(session({ graph: GRAPH_SUMMARY }));
  assert.equal(scroller.scrollTop, 1000);
  scroller.scrollTop = 470;
  chat.render(session({ graph: { ...GRAPH_SUMMARY, done: 2 } }));
  assert.equal(scroller.scrollTop, 1000);
  // An unchanged summary moves nothing.
  scroller.scrollTop = 480;
  chat.render(session({ graph: { ...GRAPH_SUMMARY, done: 2 } }));
  assert.equal(scroller.scrollTop, 480);

  // A reader further up keeps their place, and the summary is no news for
  // the jump button: it shows beside the transcript.
  scroller.scrollTop = 100;
  chat.render(session({ graph: { ...GRAPH_SUMMARY, done: 1 } }));
  assert.equal(scroller.scrollTop, 100);
  assert.equal(byId("jump").hidden, true);
});

test("header actions appear only with their capability and status", () => {
  const none = {
    diff: false,
    diffEnabled: false,
    publish: false,
    publishEnabled: false,
    publishWaits: false,
    sync: false,
  };
  assert.deepEqual(sessionActions("Review"), {
    ...none,
    cancel: true,
    merge: false,
    mergeEnabled: false,
  });
  assert.deepEqual(sessionActions("Review", ["merge"]), {
    ...none,
    cancel: true,
    merge: true,
    mergeEnabled: true,
  });
  assert.deepEqual(sessionActions("InProgress", ["merge"]), {
    ...none,
    cancel: true,
    merge: true,
    mergeEnabled: false,
    publishWaits: true,
  });
  assert.deepEqual(sessionActions("Done", ["merge"]), {
    ...none,
    cancel: false,
    merge: true,
    mergeEnabled: false,
  });
  // A session with a review request merges on the forge.
  assert.deepEqual(sessionActions("Review", ["merge"], true), {
    ...none,
    cancel: true,
    merge: true,
    mergeEnabled: false,
  });
  // The diff follows `allows_diff_view`: not while a session is a draft.
  assert.equal(sessionActions("Done", ["diff"]).diffEnabled, true);
  assert.equal(sessionActions("Draft", ["diff"]).diffEnabled, false);
  assert.equal(sessionActions("Draft", ["diff"]).diff, true);

  const plain = mountChat();
  plain.chat.render(session({ status: "InProgress" }));
  assert.equal(plain.byId("merge").hidden, true);
  assert.equal(plain.byId("sheet-merge").hidden, true);
  assert.equal(plain.byId("stop").hidden, true);
  assert.equal(plain.byId("cancel").hidden, false);
  assert.equal(plain.byId("session-more").hidden, false);
  plain.chat.render(session({ status: "Done" }));
  // Nothing to offer: the sheet button goes too.
  assert.equal(plain.byId("session-more").hidden, true);

  const capable = mountChat({ capabilities: ["merge", "stop_turn"] });
  capable.chat.render(session({ status: "InProgress" }));
  assert.equal(capable.byId("merge").hidden, false);
  assert.equal(capable.byId("merge").disabled, true);
  assert.equal(capable.byId("stop").hidden, false);
  capable.chat.render(session({ status: "Review" }));
  assert.equal(capable.byId("merge").disabled, false);
  assert.equal(capable.byId("sheet-merge").disabled, false);
  assert.equal(capable.byId("stop").hidden, true);
  // With only Merge, which the wide header already shows, the sheet
  // button is for phones.
  capable.chat.render(session({ status: "Merged" }));
  assert.equal(capable.byId("session-more").hidden, false);
  assert.equal(capable.byId("session-more").classList.contains("narrow-only"), true);
  // A capability the host stops listing takes the action away.
  capable.chat.setCapabilities([]);
  assert.equal(capable.byId("merge").hidden, true);
});

test("Stop sends stop_turn for the open session and shows a failure beside it", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["stop_turn"],
    respond: () => {
      throw new Error("No turn is running");
    },
  });
  chat.render(session({ status: "InProgress" }));
  byId("stop").click();
  assert.equal(byId("stop").disabled, true);
  await settle();
  assert.deepEqual(requests, [{ method: "stop_turn", session_id: "s1" }]);
  assert.equal(byId("session-error").textContent, "No turn is running");
  assert.equal(byId("stop").disabled, false);
});

test("Merge asks first, names the base branch, and sends merge_session", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["merge"] });
  chat.render(session());
  const dialog = byId("merge-dialog");
  assert.equal(byId("merge-target").textContent, "its base branch");
  assert.equal(byId("sheet-merge-label").textContent, "Merge");

  byId("merge").click();
  assert.equal(dialog.open, true);
  dialog.querySelector('button[value="keep"]').click();
  await settle();
  assert.deepEqual(requests, []);

  chat.render(session({ base_branch: "main" }));
  assert.equal(byId("merge-target").textContent, "main");
  assert.equal(byId("sheet-merge-label").textContent, "Merge into main");
  // On a phone the sheet offers Merge and opens the same confirmation.
  byId("session-more").click();
  assert.equal(byId("session-sheet").open, true);
  byId("sheet-merge").click();
  assert.equal(byId("session-sheet").open, false);
  assert.equal(dialog.open, true);
  dialog.querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, [{ method: "merge_session", session_id: "s1" }]);
  assert.equal(byId("action-error").textContent, "");
});

test("a confirmation never acts on a session other than the one it asked about", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["merge"] });
  chat.render(session());
  const cancelDialog = byId("cancel-dialog");
  const mergeDialog = byId("merge-dialog");

  // Moving to another session, as the back gesture does, closes every
  // open question without acting.
  byId("session-more").click();
  byId("cancel").click();
  assert.equal(cancelDialog.open, true);
  chat.open("s2");
  assert.equal(cancelDialog.open, false);
  chat.render(session({ id: "s2" }));
  byId("merge").click();
  chat.open("s3");
  assert.equal(mergeDialog.open, false);
  byId("session-more").click();
  chat.open("s4");
  assert.equal(byId("session-sheet").open, false);
  await settle();
  assert.deepEqual(requests, []);
  assert.equal(byId("action-error").textContent, "");

  // A dialog still open for another session sends nothing on confirm.
  chat.render(session({ id: "s4" }));
  byId("merge").click();
  mergeDialog.dataset.sessionId = "s3";
  mergeDialog.querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, []);

  // Confirming for the open session sends that session's id.
  byId("merge").click();
  mergeDialog.querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, [{ method: "merge_session", session_id: "s4" }]);
});

test("a refresh failure shows for its own session until a snapshot arrives", () => {
  const { byId, chat } = mountChat();
  chat.render(session());
  // A failure for a session no longer open stays off this one.
  chat.showLoadError("s0", "Request failed");
  assert.equal(byId("action-error").textContent, "");
  chat.showLoadError("s1", "Disconnected");
  assert.equal(byId("action-error").textContent, "Disconnected");
  chat.render(session());
  assert.equal(byId("action-error").textContent, "");
});

test("text typed after sending survives the reply, even in another session", async () => {
  const storage = new MemoryStorage();
  let accept = () => {};
  const { byId, chat } = mountChat({
    respond: () =>
      new Promise((resolve) => {
        accept = () => resolve({ kind: "accepted" });
      }),
    sessionStorage: storage,
  });
  chat.render(session());
  const message = byId("message");
  message.value = "Ship it";
  message.dispatchEvent(new FakeEvent("input"));
  byId("send").click();
  // More text, then another session, before the host answers.
  message.value = "Ship it and tag it";
  message.dispatchEvent(new FakeEvent("input"));
  chat.open("s2");
  accept();
  await settle();
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "Ship it and tag it");
  chat.open("s1");
  assert.equal(message.value, "Ship it and tag it");
});

test("mergeTarget reads the base branch only when the host sends one", () => {
  assert.equal(mergeTarget({ base_branch: "main" }), "main");
  assert.equal(mergeTarget({ base_branch: "" }), null);
  assert.equal(mergeTarget({}), null);
  assert.equal(mergeTarget(null), null);
});

// A request answer the test settles by hand: `respond` returns `promise`
// until `resolve` or `reject` runs.
function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });

  return { promise, reject, resolve };
}

const REVIEW_REQUEST = {
  display_id: "#7",
  forge: "GitHub",
  source_branch: "wt/s1",
  state: "Open",
  target_branch: "main",
  title: "Build it",
  web_url: "https://github.com/example/project/pull/7",
};

test("Publish and Sync appear only with their capability and a status that allows them", () => {
  assert.equal(sessionActions("Review", ["publish"]).publishEnabled, true);
  assert.equal(sessionActions("Review", ["publish"]).publishWaits, false);
  // As `p` in the terminal, a running turn or sync queues the publish.
  for (const status of ["InProgress", "Rebasing"]) {
    assert.equal(sessionActions(status, ["publish"]).publish, true, status);
    assert.equal(sessionActions(status, ["publish"]).publishEnabled, true, status);
    assert.equal(sessionActions(status, ["publish"]).publishWaits, true, status);
    assert.equal(sessionActions(status).publishEnabled, false, status);
  }
  for (const status of ["Draft", "Question", "Queued", "Merging", "Merged", "Done", "Canceled"]) {
    assert.equal(sessionActions(status, ["publish"]).publishEnabled, false, status);
  }
  assert.equal(sessionActions("Review").publish, false);
  // Sync follows `allows_rebase_action`, and is left out where it does
  // not apply.
  assert.equal(sessionActions("InProgress", ["sync"]).sync, true);
  assert.equal(sessionActions("Review", ["sync"]).sync, true);
  assert.equal(sessionActions("Question", ["sync"]).sync, false);
  assert.equal(sessionActions("Done", ["sync"]).sync, false);
  assert.equal(sessionActions("Review").sync, false);

  const plain = mountChat();
  plain.chat.render(session());
  assert.equal(plain.byId("publish").hidden, true);
  assert.equal(plain.byId("sheet-publish").hidden, true);
  assert.equal(plain.byId("sheet-sync").hidden, true);

  const capable = mountChat({ capabilities: ["publish", "sync"] });
  capable.chat.render(session({ status: "Question" }));
  assert.equal(capable.byId("publish").hidden, false);
  assert.equal(capable.byId("publish").disabled, true);
  assert.equal(capable.byId("sheet-publish").disabled, true);
  capable.chat.render(session({ status: "InProgress" }));
  assert.equal(capable.byId("publish").disabled, false);
  assert.equal(capable.byId("sheet-sync").hidden, false);
  assert.equal(capable.byId("sheet-sync-label").textContent, "Sync with base branch");
  capable.chat.render(session({ base_branch: "main" }));
  assert.equal(capable.byId("publish").disabled, false);
  assert.equal(capable.byId("sheet-publish").disabled, false);
  assert.equal(capable.byId("sheet-sync-label").textContent, "Sync with main");
  // Sync lives in the sheet on every screen, so the sheet button does too.
  assert.equal(capable.byId("session-more").classList.contains("narrow-only"), false);
  capable.chat.render(session({ status: "Merged" }));
  assert.equal(capable.byId("sheet-sync").hidden, true);
  assert.equal(capable.byId("session-more").classList.contains("narrow-only"), true);
  capable.chat.setConnected(false);
  capable.chat.render(session());
  assert.equal(capable.byId("publish").disabled, true);
  assert.equal(capable.byId("sheet-sync").disabled, true);
});

test("publishBranch names the branch an earlier publish used, or the session branch", () => {
  assert.equal(publishBranch({ branch: "wt/s1", published_upstream_ref: "origin/feature/x" }), "feature/x");
  assert.equal(publishBranch({ published_upstream_ref: "feature" }), "feature");
  assert.equal(publishBranch({ branch: "wt/s1", published_upstream_ref: null }), "wt/s1");
  assert.equal(publishBranch({}), null);
  assert.equal(publishBranch(null), null);
});

test("Publish asks first, names the remote branch, and says whether it opens or refreshes", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["publish"],
    respond: () => ({ kind: "review_request", review_request: REVIEW_REQUEST }),
  });
  chat.render(session());
  const dialog = byId("publish-dialog");
  assert.equal(byId("publish-branch").textContent, "the session branch");
  assert.equal(byId("publish-review").textContent, "opens a pull request for it");
  assert.equal(byId("sheet-publish-note").textContent, "opens a pull request");

  byId("publish").click();
  assert.equal(dialog.open, true);
  dialog.querySelector('button[value="keep"]').click();
  await settle();
  assert.deepEqual(requests, []);

  chat.render(session({ branch: "wt/s1", published_upstream_ref: "origin/feature/x", review_request: REVIEW_REQUEST }));
  assert.equal(byId("publish-branch").textContent, "feature/x");
  assert.equal(byId("publish-review").textContent, "refreshes its pull request #7");
  assert.equal(byId("sheet-publish-note").textContent, "refreshes #7");
  // On a phone the sheet offers Publish and opens the same confirmation.
  byId("session-more").click();
  byId("sheet-publish").click();
  assert.equal(byId("session-sheet").open, false);
  assert.equal(dialog.open, true);
  dialog.querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, [{ method: "publish_session", session_id: "s1" }]);
  const link = byId("action-note").querySelector("a");
  assert.equal(byId("action-note").textContent, "Published · #7 open");
  assert.equal(link.getAttribute("href"), "https://github.com/example/project/pull/7");
  assert.equal(link.getAttribute("rel"), "noopener noreferrer");
});

test("a publish stays pending across session switches and is never sent twice", async () => {
  const answer = deferred();
  const { byId, chat, requests } = mountChat({
    capabilities: ["publish"],
    respond: (body) => (body.method === "publish_session" ? answer.promise : { kind: "accepted" }),
  });
  chat.render(session());
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("publish").disabled, true);
  assert.equal(byId("publish").getAttribute("aria-busy"), "true");
  assert.equal(byId("publish-label").textContent, "Publishing…");
  assert.equal(byId("sheet-publish-note").textContent, "publishing…");
  // A second confirmation, such as one left open from before, adds nothing.
  byId("publish-dialog").dataset.sessionId = "s1";
  byId("publish-dialog").showModal();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();

  // Another session can publish meanwhile; coming back shows the wait.
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  assert.equal(byId("publish").disabled, false);
  assert.equal(byId("publish-label").textContent, "Publish");
  chat.open("s1");
  chat.render(session());
  assert.equal(byId("publish").disabled, true);
  assert.equal(byId("publish-label").textContent, "Publishing…");
  assert.deepEqual(requests, [{ method: "publish_session", session_id: "s1" }]);

  // The answer arriving while another session is open waits for this one.
  chat.open("s2");
  answer.resolve({ kind: "review_request", review_request: REVIEW_REQUEST });
  await settle();
  assert.equal(byId("action-note").textContent, "");
  chat.open("s1");
  chat.render(session());
  assert.equal(byId("action-note").textContent, "Published · #7 open");
  assert.equal(byId("publish").disabled, false);
  assert.equal(byId("publish-label").textContent, "Publish");
});

test("Publish during a turn says it waits, stays queued until the turn ends, then publishes", async () => {
  const answer = deferred();
  const { byId, chat, requests } = mountChat({
    capabilities: ["publish"],
    respond: (body) => (body.method === "publish_session" ? answer.promise : { kind: "accepted" }),
  });
  chat.render(session());
  assert.equal(byId("publish-wait").hidden, true);
  assert.equal(byId("publish").title, "");

  chat.render(session({ status: "InProgress" }));
  assert.equal(byId("publish").disabled, false);
  assert.equal(byId("publish-label").textContent, "Publish");
  assert.equal(byId("publish").title, "Publishes after the current turn");
  assert.equal(byId("sheet-publish-note").textContent, "after this turn");
  assert.equal(byId("publish-wait").hidden, false);
  assert.equal(
    byId("publish-wait").textContent,
    "The session is working, so this waits and publishes after the current turn.",
  );
  chat.render(session({ review_request: REVIEW_REQUEST, status: "Rebasing" }));
  assert.equal(byId("sheet-publish-note").textContent, "after this turn");
  assert.equal(byId("publish-review").textContent, "refreshes its pull request #7");

  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, [{ method: "publish_session", session_id: "s1" }]);
  // The host answers only once the publish ran, so the page says it is
  // queued while the turn runs, through every refresh.
  chat.render(session({ status: "InProgress" }));
  assert.equal(byId("publish").disabled, true);
  assert.equal(byId("publish").getAttribute("aria-busy"), "true");
  assert.equal(byId("publish").title, "");
  assert.equal(byId("publish-label").textContent, "Publish queued");
  assert.equal(byId("sheet-publish-note").textContent, "queued");
  // Messages, Stop and the other actions stay free meanwhile.
  assert.equal(byId("send").disabled, false);

  // The turn ends and the publish runs.
  chat.render(session());
  assert.equal(byId("publish-label").textContent, "Publishing…");
  assert.equal(byId("sheet-publish-note").textContent, "publishing…");
  assert.equal(byId("publish-wait").hidden, true);
  answer.resolve({ kind: "review_request", review_request: REVIEW_REQUEST });
  await settle();
  assert.equal(byId("action-note").textContent, "Published · #7 open");
  assert.equal(byId("publish-label").textContent, "Publish");
  assert.equal(requests.length, 1);
});

test("a publish queued behind a turn stays queued when the connection drops", async () => {
  const answer = deferred();
  const { byId, chat, requests } = mountChat({
    capabilities: ["publish"],
    respond: (body) => (body.method === "publish_session" ? answer.promise : { kind: "accepted" }),
  });
  const confirmPublish = async () => {
    byId("publish").click();
    byId("publish-dialog").querySelector('button[value="confirm"]').click();
    await settle();
  };
  chat.render(session({ status: "InProgress" }));
  await confirmPublish();
  // The connection drops; Agentty still runs the publish after the turn.
  answer.reject(connectionLostError("Disconnected"));
  chat.setConnected(false);
  await settle();
  chat.setConnected(true);
  chat.render(session({ status: "InProgress" }));

  assert.ok(isConnectionLost(connectionLostError("Disconnected")));
  assert.equal(isConnectionLost(new Error("Disconnected")), false);
  assert.equal(byId("publish-label").textContent, "Publish queued");
  assert.equal(byId("sheet-publish-note").textContent, "queued");
  assert.equal(byId("publish").disabled, true);
  assert.equal(byId("action-error").textContent, "");
  // It is not offered again, even through a confirmation left open.
  byId("publish-dialog").dataset.sessionId = "s1";
  byId("publish-dialog").showModal();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(requests.length, 1);
  // Another session meanwhile does not end it.
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  chat.open("s1");
  chat.render(session({ status: "Rebasing" }));
  assert.equal(byId("publish-label").textContent, "Publish queued");

  // The turn is over, so the publish ran: Publish is offered again, with
  // no error.
  chat.render(session({ review_request: REVIEW_REQUEST }));
  assert.equal(byId("publish-label").textContent, "Publish");
  assert.equal(byId("publish").disabled, false);
  assert.equal(byId("action-error").textContent, "");
  assert.equal(byId("action-note").textContent, "");
});

test("a publish during a turn still shows Agentty's refusal, and one in review a dropped connection", async () => {
  let failure = new Error("Session is busy");
  const { byId, chat } = mountChat({
    capabilities: ["publish"],
    respond: (body) => {
      if (body.method === "publish_session") {
        throw failure;
      }

      return { kind: "accepted" };
    },
  });
  chat.render(session({ status: "InProgress" }));
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("action-error").textContent, "Session is busy");
  assert.equal(byId("publish-label").textContent, "Publish");
  assert.equal(byId("publish").disabled, false);

  // In review the publish runs at once, so a dropped connection says so.
  failure = connectionLostError("Disconnected");
  chat.render(session());
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("action-error").textContent, "Disconnected");
  assert.equal(byId("publish-label").textContent, "Publish");
});

test("a failed publish shows the host's error once, until the publication changes", async () => {
  const { byId, chat } = mountChat({
    capabilities: ["publish"],
    respond: () => {
      throw new Error("Forge authentication failed");
    },
  });
  chat.render(session());
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("action-error").textContent, "Forge authentication failed");
  assert.equal(byId("action-note").textContent, "");
  assert.equal(byId("publish").disabled, false);
  // Refreshes that change nothing about the publication keep it.
  chat.render(session({ updated_at: 11 }));
  assert.equal(byId("action-error").textContent, "Forge authentication failed");
  // A publish from the terminal makes it stale.
  chat.render(session({ published_upstream_ref: "origin/wt/s1", review_request: REVIEW_REQUEST }));
  assert.equal(byId("action-error").textContent, "");

  // A failure that arrives while another session is open shows on return,
  // and only then.
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  chat.open("s2");
  await settle();
  assert.equal(byId("action-error").textContent, "");
  chat.open("s1");
  assert.equal(byId("action-error").textContent, "Forge authentication failed");
  chat.render(session());
  assert.equal(byId("action-error").textContent, "Forge authentication failed");
  chat.open("s2");
  chat.open("s1");
  assert.equal(byId("action-error").textContent, "");
});

test("a publish note goes once the review request is no longer the one published", async () => {
  const { byId, chat } = mountChat({
    capabilities: ["publish"],
    respond: () => ({ kind: "review_request", review_request: REVIEW_REQUEST }),
  });
  chat.render(session());
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("action-note").textContent, "Published · #7 open");
  // The snapshot catching up with the publish keeps the note.
  chat.render(session({ review_request: REVIEW_REQUEST }));
  assert.equal(byId("action-note").textContent, "Published · #7 open");
  // Once the pull request merges, the note is out of date.
  chat.render(session({ review_request: { ...REVIEW_REQUEST, state: "Merged" } }));
  assert.equal(byId("action-note").textContent, "");
  // Coming back does not announce it again.
  chat.open("s2");
  chat.open("s1");
  assert.equal(byId("action-note").textContent, "");
});

test("actions run per session: one session's action neither blocks nor answers for another", async () => {
  const answers = [];
  const { byId, chat, requests } = mountChat({
    capabilities: ["sync", "merge"],
    respond: () => {
      const answer = deferred();
      answers.push(answer);

      return answer.promise;
    },
  });
  const confirmSync = () => {
    byId("session-more").click();
    byId("sheet-sync").click();
    byId("sync-dialog").querySelector('button[value="confirm"]').click();
  };
  chat.render(session());
  confirmSync();
  await settle();
  assert.equal(byId("sheet-sync").disabled, true);

  // Another session can sync and merge while the first sync runs.
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  assert.equal(byId("sheet-sync").disabled, false);
  assert.equal(byId("merge").disabled, false);
  confirmSync();
  await settle();
  assert.deepEqual(requests, [
    { method: "sync_session", session_id: "s1" },
    { method: "sync_session", session_id: "s2" },
  ]);
  // The second session's refusal stays when the first one's sync succeeds.
  answers[1].reject(new Error("Session is not ready to sync"));
  await settle();
  assert.equal(byId("action-error").textContent, "Session is not ready to sync");
  answers[0].resolve({ kind: "accepted" });
  await settle();
  assert.equal(byId("action-error").textContent, "Session is not ready to sync");

  // A merge in flight for this session disables only its own Merge.
  byId("merge").click();
  byId("merge-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.equal(byId("merge").disabled, true);
  chat.open("s1");
  chat.render(session());
  assert.equal(byId("merge").disabled, false);
  assert.equal(byId("sheet-sync").disabled, false);
});

test("a review request link from a publish goes through the link filter", async () => {
  const { byId, chat } = mountChat({
    capabilities: ["publish"],
    respond: () => ({ kind: "review_request", review_request: { ...REVIEW_REQUEST, web_url: "javascript:alert(1)" } }),
  });
  chat.render(session());
  byId("publish").click();
  byId("publish-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  // An address the filter refuses leaves the label as text.
  assert.equal(byId("action-note").querySelector("a"), null);
  assert.equal(byId("action-note").textContent, "Published · #7 open");
});

test("Sync asks first, names the base branch, and shows the host's refusal", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["sync"],
    respond: () => {
      throw new Error("Session cannot sync in status `Done`");
    },
  });
  chat.render(session({ base_branch: "main" }));
  const dialog = byId("sync-dialog");
  assert.equal(byId("sync-target").textContent, "main");
  byId("session-more").click();
  byId("sheet-sync").click();
  assert.equal(dialog.open, true);
  dialog.querySelector('button[value="keep"]').click();
  await settle();
  assert.deepEqual(requests, []);

  byId("session-more").click();
  byId("sheet-sync").click();
  dialog.querySelector('button[value="confirm"]').click();
  assert.equal(byId("sheet-sync").disabled, true);
  await settle();
  assert.deepEqual(requests, [{ method: "sync_session", session_id: "s1" }]);
  assert.equal(byId("action-error").textContent, "Session cannot sync in status `Done`");
  assert.equal(byId("sheet-sync").disabled, false);

  // A confirmation left from another session does nothing.
  byId("session-more").click();
  byId("sheet-sync").click();
  chat.open("s2");
  assert.equal(dialog.open, false);
  await settle();
  assert.equal(requests.length, 1);
});

const HOST = {
  base_branch: "main",
  default_model: "Opus 5.5",
  kind: "host",
  models: [
    { agent: "claude", description: "", model: "claude-opus-5-5", name: "Opus 5.5", supports_fast: true },
    { agent: "claude", description: "", model: "claude-sonnet-5", name: "Sonnet 5", supports_fast: false },
    { agent: "codex", description: "", model: "gpt-6.1", name: "GPT 6.1", supports_fast: false },
  ],
  project: "agentty",
  session_modes: ["regular"],
};

const SETTINGS_SESSION = {
  agent: "claude",
  model: "Opus 5.5",
  model_id: "claude-opus-5-5",
  reasoning: "high",
  response_style: "balanced",
  speed_mode: "normal",
};

test("the settings line opens the settings sheet only when the host offers settings and host", async () => {
  for (const capabilities of [[], ["settings"], ["host"]]) {
    const plain = mountChat({ capabilities: capabilities });
    plain.chat.render(session(SETTINGS_SESSION));
    assert.equal(plain.byId("composer-settings").hidden, true);
    assert.equal(plain.byId("composer-meta").hidden, false);
    assert.match(plain.byId("composer-meta").textContent, /balanced/);
  }

  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings"],
    respond: (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }),
  });
  chat.render(session(SETTINGS_SESSION));
  const line = byId("composer-settings");
  assert.equal(line.hidden, false);
  assert.equal(byId("composer-meta").hidden, true);
  assert.equal(line.textContent, "Opus 5.5 [high]·balanced·normal speed");
  line.click();
  await settle();
  assert.equal(byId("settings-dialog").open, true);
  assert.equal(byId("settings-note").hidden, true);
  assert.deepEqual(requests, [{ method: "get_host" }]);
  // Only the session's agent's models are offered, by name.
  assert.deepEqual(
    Array.from(byId("settings-model").children, (option) => [option.value, option.textContent]),
    [
      ["claude-opus-5-5", "Opus 5.5"],
      ["claude-sonnet-5", "Sonnet 5"],
    ],
  );

  // Moving to another session closes the sheet without sending.
  chat.open("s2");
  assert.equal(byId("settings-dialog").open, false);
  assert.equal(requests.length, 1);
});

test("the settings sheet says that changes during a running turn apply after it", async () => {
  const { byId, chat } = mountChat({
    capabilities: ["host", "settings"],
    respond: () => HOST,
  });
  chat.render(session({ ...SETTINGS_SESSION, status: "InProgress" }));
  byId("composer-settings").click();
  await settle();
  assert.equal(byId("settings-note").hidden, false);
  // The turn ends while the sheet is open.
  chat.render(session(SETTINGS_SESSION));
  assert.equal(byId("settings-note").hidden, true);
  assert.equal(byId("settings-dialog").open, true);
  // Offline, the line cannot open the sheet.
  byId("settings-dialog").close();
  chat.setConnected(false);
  assert.equal(byId("composer-settings").disabled, true);
});

// Types `text` into the composer as the user would.
function typeMessage(byId, text) {
  const message = byId("message");
  message.value = text;
  message.dispatchEvent(new FakeEvent("input"));
}

function pressKey(byId, key) {
  const event = new FakeEvent("keydown", { key: key });
  byId("message").dispatchEvent(event);

  return event;
}

function suggested(byId) {
  return Array.from(byId("message-suggestions").children, (option) => option.dataset.key);
}

test("/ in the composer opens the slash menu, and Enter picks there instead of sending", async () => {
  const storage = new MemoryStorage();
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings"],
    respond: (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }),
    sessionStorage: storage,
  });
  chat.render(session(SETTINGS_SESSION));
  const message = byId("message");
  // The popup belongs to the composer, right after the message box.
  assert.equal(message.nextSibling.querySelector("#message-suggestions"), byId("message-suggestions"));
  assert.equal(message.getAttribute("aria-expanded"), "false");

  typeMessage(byId, "/");
  await settle();
  assert.equal(message.getAttribute("aria-expanded"), "true");
  assert.deepEqual(suggested(byId), ["/model", "/reasoning", "/style", "/fast"]);
  assert.deepEqual(
    Array.from(byId("message-suggestions").children, (option) => option.textContent)[1],
    "/reasoningOverride the reasoning level for this session.",
  );
  typeMessage(byId, "/re");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "/re");
  assert.equal(pressKey(byId, "Enter").defaultPrevented, true);
  assert.deepEqual(suggested(byId), ["low", "medium", "high", "xhigh", "max"]);
  assert.equal(message.value, "/reasoning");
  // The key's release keeps the value list.
  message.dispatchEvent(new FakeEvent("keyup", { key: "Enter" }));
  pressKey(byId, "ArrowDown");
  pressKey(byId, "Enter");
  await settle();

  assert.equal(message.getAttribute("aria-expanded"), "false");
  assert.equal(message.value, "");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), null);
  assert.deepEqual(requests, [
    { method: "get_host" },
    { method: "set_session_settings", reasoning: "medium", session_id: "s1" },
  ]);
});

test("a tapped slash command picks it, and the host's refusal shows under the composer", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings"],
    respond: (body) => {
      if (body.method === "get_host") {
        return HOST;
      }
      throw new Error("Session is busy");
    },
  });
  chat.render(session(SETTINGS_SESSION));
  typeMessage(byId, "/f");
  await settle();
  byId("message-suggestions").children[0].click();
  await settle();

  assert.deepEqual(requests.slice(1), [{ method: "set_session_settings", session_id: "s1", speed_mode: "fast" }]);
  assert.equal(byId("session-error").textContent, "Session is busy");
  assert.equal(byId("message").value, "");
});

test("a sent / message that names no command leaves no menu behind", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings"],
    respond: (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }),
  });
  chat.render(session(SETTINGS_SESSION));
  const message = byId("message");

  typeMessage(byId, "/tmp/notes");
  await settle();
  assert.equal(message.getAttribute("aria-expanded"), "false");
  pressKey(byId, "Enter");
  await settle();
  chat.render(session(SETTINGS_SESSION));

  assert.deepEqual(requests.slice(1), [{ method: "send_message", message: "/tmp/notes", session_id: "s1" }]);
  assert.equal(message.value, "");
  assert.equal(message.getAttribute("aria-expanded"), "false");
  assert.deepEqual(suggested(byId), []);

  // Nor does sending with the button while a value list shows.
  typeMessage(byId, "/st");
  pressKey(byId, "Enter");
  assert.deepEqual(suggested(byId), ["concise", "balanced", "detailed"]);
  byId("composer").dispatchEvent(new FakeEvent("submit"));
  await settle();
  chat.render(session(SETTINGS_SESSION));
  assert.equal(message.getAttribute("aria-expanded"), "false");
});

test("without settings and host, / is plain text and Enter sends it", async () => {
  for (const capabilities of [[], ["settings"], ["host"]]) {
    const { byId, chat, requests } = mountChat({ capabilities: capabilities });
    chat.render(session(SETTINGS_SESSION));
    typeMessage(byId, "/model");
    assert.equal(byId("message").getAttribute("aria-expanded"), "false");
    pressKey(byId, "Enter");
    await settle();
    assert.deepEqual(requests, [{ method: "send_message", message: "/model", session_id: "s1" }]);
  }
});

// Mounts the chat with the `graph_proposal` capability on `s1` in
// review, answering each request with `respond`.
function mountGraphChat(respond = () => ({ kind: "accepted" })) {
  const mounted = mountChat({ capabilities: ["graph_proposal"], respond: respond });
  mounted.chat.render(session());

  return mounted;
}

// Picks `/graph` from the slash menu of the composer.
function armGraph(byId) {
  typeMessage(byId, "/gr");
  pressKey(byId, "Enter");
}

test("/graph arms the composer, which then asks for a graph proposal and disarms", async () => {
  const { byId, chat, requests } = mountGraphChat();
  typeMessage(byId, "/");
  assert.deepEqual(suggested(byId), ["/graph"]);
  assert.equal(
    byId("message-suggestions").children[0].textContent,
    "/graphDiscuss a task with its execution graph beside the chat.",
  );
  assert.equal(byId("composer-graph").hidden, true);

  armGraph(byId);
  assert.equal(byId("message").value, "");
  assert.equal(byId("composer-graph").hidden, false);
  assert.equal(byId("message").placeholder, "Describe the task for the execution graph");
  assert.equal(byId("send").getAttribute("aria-label"), "Ask for a graph proposal");
  assert.equal(byId("composer-meta").textContent, "Enter asks for a graph proposal · Esc returns to chat");
  // The task is taken literally, so `/` opens no menu now.
  typeMessage(byId, "/split the parser");
  assert.deepEqual(suggested(byId), []);
  // A refresh keeps the armed composer.
  chat.render(session());
  assert.equal(byId("composer-graph").hidden, false);
  pressKey(byId, "Enter");
  await settle();

  assert.deepEqual(requests, [
    { message: "/split the parser", method: "generate_graph_proposal", session_id: "s1" },
  ]);
  assert.equal(byId("message").value, "");
  assert.equal(byId("composer-graph").hidden, true);
  assert.equal(byId("message").placeholder, "Reply to the agent");
  // The next message is chat again.
  typeMessage(byId, "Thanks");
  pressKey(byId, "Enter");
  await settle();
  assert.equal(requests[1].method, "send_message");
});

test("a refused graph request keeps the task and graph mode, and Escape or the marker leaves it", async () => {
  const { byId, requests } = mountGraphChat(() => {
    throw new Error("Graph generation requires a prepared idle session");
  });
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  pressKey(byId, "Enter");
  await settle();

  assert.equal(requests.length, 1);
  assert.equal(byId("session-error").textContent, "Graph generation requires a prepared idle session");
  assert.equal(byId("message").value, "Split the parser");
  assert.equal(byId("composer-graph").hidden, false);
  // Escape only leaves graph mode, keeping the text.
  assert.equal(pressKey(byId, "Escape").defaultPrevented, true);
  assert.equal(byId("composer-graph").hidden, true);
  assert.equal(byId("message").value, "Split the parser");
  assert.equal(byId("message").placeholder, "Reply to the agent");
  assert.equal(pressKey(byId, "Escape").defaultPrevented, false);
  // The marker's button leaves it too.
  typeMessage(byId, "/gr");
  pressKey(byId, "Enter");
  byId("composer-graph-leave").click();
  assert.equal(byId("composer-graph").hidden, true);
});

test("composerLabels asks for the graph task while graph mode is armed", () => {
  const dock = { hint: "Enter sends", placeholder: "Reply to the agent", sendLabel: "Send" };

  assert.deepEqual(composerLabels(dock, { isFinePointer: true, isGraphArmed: false }), dock);
  assert.deepEqual(composerLabels(dock, { isFinePointer: false, isGraphArmed: true }), {
    hint: "Sending asks for a graph proposal",
    placeholder: GRAPH_PLACEHOLDER,
    sendLabel: "Ask for a graph proposal",
  });
  assert.equal(GRAPH_PLACEHOLDER, "Describe the task for the execution graph");
  assert.deepEqual(composerLabels(dock, { isFinePointer: false, isGraphArmed: true, isGraphBlocked: true }), {
    hint: "Graph requests unavailable now · × returns to chat",
    placeholder: GRAPH_PLACEHOLDER,
    sendLabel: "Graph requests unavailable now",
  });
  // Blocked means nothing without graph mode.
  assert.deepEqual(composerLabels(dock, { isFinePointer: true, isGraphArmed: false, isGraphBlocked: true }), dock);
});

test("/graph is offered only with its capability, in sessions the terminal lists it for", () => {
  for (const status of ["InProgress", "Rebasing"]) {
    const { byId, chat } = mountGraphChat();
    chat.render(session({ status: status }));
    typeMessage(byId, "/");
    assert.deepEqual(suggested(byId), [], status);
  }
  const { byId, chat } = mountChat({ capabilities: ["graphs", "graph_actions"] });
  chat.render(session());
  typeMessage(byId, "/");
  assert.deepEqual(suggested(byId), []);
});

test("graph mode stays with its draft across a session switch, and Esc forgets it", async () => {
  const { byId, chat, requests } = mountGraphChat();
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  assert.equal(byId("composer-graph").hidden, true);
  assert.equal(byId("message").value, "");

  chat.open("s1");
  chat.render(session());
  assert.equal(byId("composer-graph").hidden, false);
  assert.equal(byId("message").value, "Split the parser");
  assert.equal(byId("message").placeholder, "Describe the task for the execution graph");
  // Leaving graph mode is kept with the draft too.
  pressKey(byId, "Escape");
  chat.open("s2");
  chat.open("s1");
  chat.render(session());
  assert.equal(byId("composer-graph").hidden, true);
  assert.equal(byId("message").value, "Split the parser");
  pressKey(byId, "Enter");
  await settle();
  assert.deepEqual(requests, [{ message: "Split the parser", method: "send_message", session_id: "s1" }]);
});

test("a reloaded tab brings a graph task back in graph mode, and sending it asks for a proposal", async () => {
  const storage = new MemoryStorage();
  const first = mountChat({ capabilities: ["graph_proposal"], sessionStorage: storage });
  first.chat.render(session());
  armGraph(first.byId);
  typeMessage(first.byId, "Split the parser");

  const reloaded = mountChat({ capabilities: ["graph_proposal"], sessionStorage: storage });
  reloaded.chat.render(session());
  assert.equal(reloaded.byId("composer-graph").hidden, false);
  assert.equal(reloaded.byId("message").value, "Split the parser");
  pressKey(reloaded.byId, "Enter");
  await settle();

  assert.deepEqual(reloaded.requests, [
    { message: "Split the parser", method: "generate_graph_proposal", session_id: "s1" },
  ]);
  assert.equal(reloaded.byId("composer-graph").hidden, true);
  assert.equal(storage.getItem(DRAFT_GRAPH_KEY_PREFIX + "s1"), null);
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), null);
});

test("a graph task survives its session going missing and coming back", async () => {
  const { byId, chat, requests } = mountGraphChat();
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  // The host stops listing the session, as when the terminal switches
  // projects, and lists it again.
  chat.render(null);
  chat.render(session());

  assert.equal(byId("composer-graph").hidden, false);
  assert.equal(byId("message").value, "Split the parser");
  pressKey(byId, "Enter");
  await settle();
  assert.deepEqual(requests, [
    { message: "Split the parser", method: "generate_graph_proposal", session_id: "s1" },
  ]);
});

test("an earlier prompt sent in graph mode leaves the graph task it replaced armed", async () => {
  const { byId, requests } = mountGraphChat();
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  pressArrow(byId, "ArrowUp");
  assert.equal(byId("message").value, "Build it");
  pressKey(byId, "Enter");
  await settle();

  assert.equal(byId("message").value, "Split the parser");
  assert.equal(byId("composer-graph").hidden, false);
  pressKey(byId, "Enter");
  await settle();
  assert.deepEqual(
    requests.map((body) => [body.method, body.message]),
    [
      ["generate_graph_proposal", "Build it"],
      ["generate_graph_proposal", "Split the parser"],
    ],
  );
});

test("a graph request that went out ends graph mode of its emptied draft in another session", async () => {
  let release;
  const { byId, chat } = mountGraphChat(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  pressKey(byId, "Enter");
  // The box is emptied while the request runs, then the reader leaves.
  typeMessage(byId, "");
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  release({ kind: "accepted" });
  await settle();

  chat.open("s1");
  chat.render(session());
  assert.equal(byId("composer-graph").hidden, true);
  assert.equal(byId("message").placeholder, "Reply to the agent");
});

test("an armed draft loaded before the first snapshot shows that it cannot go yet", () => {
  const storage = new MemoryStorage();
  storage.setItem(DRAFT_KEY_PREFIX + "s1", "Split the parser");
  storage.setItem(DRAFT_GRAPH_KEY_PREFIX + "s1", "1");

  const { byId } = mountChat({ capabilities: ["graph_proposal"], sessionStorage: storage });

  assert.equal(byId("message").value, "Split the parser");
  assert.equal(byId("composer-graph").hidden, false);
  assert.equal(byId("composer-graph").classList.contains("is-blocked"), true);
  assert.equal(byId("composer-graph-note").textContent, "Graph requests unavailable now");
  assert.equal(byId("send").disabled, true);
});

test("a graph task the host or the session cannot take now stays one, and sends nothing", async () => {
  const { byId, chat, requests } = mountGraphChat();
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  chat.setCapabilities([]);
  chat.render(session());

  assert.equal(byId("composer-graph").hidden, false);
  assert.equal(byId("composer-graph").classList.contains("is-blocked"), true);
  assert.equal(byId("composer-graph-note").textContent, "Graph requests unavailable now");
  assert.equal(byId("composer-meta").textContent, "Graph requests unavailable now · Esc returns to chat");
  assert.equal(byId("send").disabled, true);
  pressKey(byId, "Enter");
  byId("composer").dispatchEvent(new FakeEvent("submit"));
  await settle();
  assert.deepEqual(requests, []);

  // A busy session cannot take it either; once idle with the host
  // offering it again, the task can go.
  chat.setCapabilities(["graph_proposal"]);
  chat.render(session({ status: "InProgress" }));
  assert.equal(byId("send").disabled, true);
  chat.render(session());
  assert.equal(byId("composer-graph").classList.contains("is-blocked"), false);
  assert.equal(byId("composer-graph-note").textContent, "Sending asks for a graph proposal");
  assert.equal(byId("send").disabled, false);

  // Leaving graph mode makes it an ordinary message, sent as chat.
  chat.setCapabilities([]);
  chat.render(session());
  byId("composer-graph-leave").click();
  assert.equal(byId("send").disabled, false);
  pressKey(byId, "Enter");
  await settle();
  assert.deepEqual(requests, [{ message: "Split the parser", method: "send_message", session_id: "s1" }]);
});

test("a graph request whose session closed meanwhile leaves the open composer alone", async () => {
  let release;
  const { byId, chat } = mountGraphChat(
    () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  );
  armGraph(byId);
  typeMessage(byId, "Split the parser");
  pressKey(byId, "Enter");
  chat.open("s2");
  chat.render(session({ id: "s2" }));
  armGraph(byId);
  release({ kind: "accepted" });
  await settle();

  assert.equal(byId("composer-graph").hidden, false);
});

test("the graph work's questions open the question panel in any status, and Reply in chat closes them", async () => {
  const { byId, chat, refreshes, requests } = mountChat();
  const graphQuestions = [{ options: ["Local", "Remote"], text: "Where should it run?" }];
  chat.render(session({ graph_question_target: "proposal-v1", graph_questions: graphQuestions }));

  assert.equal(byId("questions").hidden, false);
  assert.equal(
    byId("questions").querySelector(".questions-source").textContent,
    "The execution graph asks. Answer here, or reply in chat.",
  );
  assert.equal(byId("answer-bar").hidden, false);
  assert.equal(byId("answer-chat").hidden, false);
  assert.equal(byId("composer").hidden, true);
  byId("questions").querySelectorAll(".option")[0].click();
  byId("answer-send").click();
  await settle();
  // The answers name the graph work whose questions the panel showed.
  assert.deepEqual(requests, [
    {
      answers: [{ answer: "Local", question: "Where should it run?" }],
      graph_question_target: "proposal-v1",
      method: "answer_questions",
      session_id: "s1",
    },
  ]);
  assert.equal(refreshes(), 0);

  byId("answer-chat").click();
  assert.equal(byId("questions").hidden, true);
  assert.equal(byId("answer-bar").hidden, true);
  assert.equal(byId("composer").hidden, false);
  // The same questions stay closed through refreshes; new ones open.
  chat.render(session({ graph_question_target: "proposal-v1", graph_questions: graphQuestions }));
  assert.equal(byId("questions").hidden, true);
  const next = [{ options: [], text: "Which file?" }];
  chat.render(session({ graph_questions: next }));
  assert.equal(byId("questions").hidden, false);
  // Opening the session again shows dismissed questions again.
  byId("answer-chat").click();
  chat.open("s1");
  chat.render(session({ graph_questions: next }));
  assert.equal(byId("questions").hidden, false);
});

test("closed graph questions open again when other graph work asks the same ones", () => {
  const { byId, chat } = mountChat();
  const graphQuestions = [{ options: [], text: "Where should it run?" }];
  const asked = (target) => session({ graph_question_target: target, graph_questions: graphQuestions });
  chat.render(asked("proposal-v1"));
  byId("answer-chat").click();
  chat.render(asked("proposal-v1"));
  assert.equal(byId("questions").hidden, true);

  chat.render(asked("proposal-v2"));

  assert.equal(byId("questions").hidden, false);
  assert.equal(byId("answer-bar").hidden, false);
});

test("new graph work asking the same questions starts with empty answers", () => {
  const { byId, chat } = mountChat();
  const graphQuestions = [{ options: ["Local", "Remote"], text: "Where should it run?" }];
  const asked = (target) => session({ graph_question_target: target, graph_questions: graphQuestions });
  chat.render(asked("node-a"));
  byId("questions").querySelectorAll(".option")[1].click();
  typeAnswer(byId("questions").querySelector("input"), "Both");
  chat.render(asked("node-a"));
  assert.equal(byId("questions").querySelector("input").value, "Both");

  chat.render(asked("node-b"));

  assert.equal(byId("questions").querySelector("input").value, "");
  assert.deepEqual(
    Array.from(byId("questions").querySelectorAll(".option"), (option) => option.getAttribute("aria-pressed")),
    ["false", "false"],
  );
  assert.equal(byId("answer-count").textContent, "0 of 1 answered");
});

test("refused graph answers say why, keep the answers, and read the session again", async () => {
  const { byId, chat, refreshes } = mountChat({
    respond: () => {
      throw new Error("These questions were already answered or are no longer pending.");
    },
  });
  chat.render(
    session({
      graph_question_target: "node-a",
      graph_questions: [{ options: ["Local"], text: "Where should it run?" }],
    }),
  );
  byId("questions").querySelector(".option").click();
  byId("answer-send").click();
  await settle();

  assert.equal(
    byId("answer-error").textContent,
    "These questions were already answered or are no longer pending.",
  );
  assert.equal(byId("answer-count").textContent, "1 of 1 answered");
  assert.equal(refreshes(), 1);
});

test("a refusal stays over the questions other graph work asks in their place", async () => {
  const refusal = "These questions were already answered or are no longer pending.";
  const { byId, chat } = mountChat({
    respond: () => {
      throw new Error(refusal);
    },
  });
  const graphQuestions = [{ options: ["Local"], text: "Where should it run?" }];
  const asked = (target) => session({ graph_question_target: target, graph_questions: graphQuestions });
  chat.render(asked("node-a"));
  byId("questions").querySelector(".option").click();
  byId("answer-send").click();
  await settle();

  // The refresh shows other graph work asking the same questions: the
  // answers start empty, and the reason they went away stays.
  chat.render(asked("node-b"));
  assert.equal(byId("answer-count").textContent, "0 of 1 answered");
  assert.equal(byId("answer-error").textContent, refusal);

  // It is said once: the next set of questions starts without it.
  chat.render(asked("node-c"));
  assert.equal(byId("answer-error").textContent, "");
});

test("a refusal does not outlive the questions it was about", async () => {
  const { byId, chat } = mountChat({
    respond: () => {
      throw new Error("These questions were already answered or are no longer pending.");
    },
  });
  const graphQuestions = [{ options: ["Local"], text: "Where should it run?" }];
  chat.render(session({ graph_question_target: "node-a", graph_questions: graphQuestions }));
  byId("questions").querySelector(".option").click();
  byId("answer-send").click();
  await settle();

  // Nothing asks any more, and later other work does.
  chat.render(session());
  assert.equal(byId("answer-error").textContent, "");
  chat.render(session({ graph_question_target: "node-b", graph_questions: graphQuestions }));
  assert.equal(byId("answer-error").textContent, "");
});

test("Reply in chat shows only where the status has a composer to reply with", () => {
  const graphQuestions = [{ options: [], text: "Where should it run?" }];
  for (const [status, isShown] of [
    ["Review", true],
    ["InProgress", true],
    ["Queued", false],
    ["Merging", false],
    ["Merged", false],
  ]) {
    const { byId, chat } = mountChat();
    chat.render(session({ graph_questions: graphQuestions, status: status }));
    assert.equal(byId("answer-bar").hidden, false, status);
    assert.equal(byId("answer-chat").hidden, !isShown, status);
    assert.equal(
      byId("questions").querySelector(".questions-source").textContent,
      isShown
        ? "The execution graph asks. Answer here, or reply in chat."
        : "The execution graph asks. Answer here.",
      status,
    );
  }
  // The line follows the status while the same questions stay.
  const { byId, chat } = mountChat();
  chat.render(session({ graph_questions: graphQuestions }));
  chat.render(session({ graph_questions: graphQuestions, status: "Merging" }));
  assert.equal(
    byId("questions").querySelector(".questions-source").textContent,
    "The execution graph asks. Answer here.",
  );
  assert.equal(graphQuestionsSource(null), "The execution graph asks. Answer here.");
});

test("the turn's own questions win over the graph work's, and keep no Reply in chat", () => {
  const { byId, chat } = mountChat();
  chat.render(
    session({
      graph_questions: [{ options: [], text: "Where should it run?" }],
      questions: [{ options: [], text: "Which branch?" }],
      status: "Question",
    }),
  );

  assert.equal(byId("questions").querySelector(".question").dataset.question, "Which branch?");
  assert.equal(byId("questions").querySelector(".questions-source"), null);
  assert.equal(byId("answer-chat").hidden, true);
  // Without its turn's questions, the session shows no answer bar.
  chat.render(session());
  assert.equal(byId("answer-bar").hidden, true);
  assert.equal(byId("questions").hidden, true);
});

test("shownQuestions prefers the turn's questions and skips a dismissed graph set", () => {
  const graph = [{ options: [], text: "Where?" }];
  const own = [{ options: [], text: "Which?" }];

  const none = { isGraph: false, questions: [], target: null };
  assert.deepEqual(shownQuestions(session({ graph_questions: graph, questions: own })), {
    isGraph: false,
    questions: own,
    target: null,
  });
  assert.deepEqual(shownQuestions(session({ graph_questions: graph })), {
    isGraph: true,
    questions: graph,
    target: null,
  });
  const asked = session({ graph_question_target: "v1", graph_questions: graph });
  assert.deepEqual(shownQuestions(asked, graphQuestionsKey(asked)), none);
  // Other graph work asking the same questions is not dismissed.
  const again = session({ graph_question_target: "v2", graph_questions: graph });
  assert.deepEqual(shownQuestions(again, graphQuestionsKey(asked)), {
    isGraph: true,
    questions: graph,
    target: "v2",
  });
  assert.equal(graphQuestionsKey(asked), "v1:" + questionsKey(graph));
  assert.equal(graphQuestionsKey({}), ":[]");
  assert.deepEqual(shownQuestions({}), none);
  assert.deepEqual(composerState("Review", { hasQuestions: true, isGraphQuestions: true }), { mode: "answer" });
  assert.equal(composerState("Review", { hasQuestions: true }).mode, "compose");
});

test("the slash menu closes on blur, offline, in a read-only session, and on a session switch", async () => {
  const { byId, chat } = mountChat({
    capabilities: ["host", "settings"],
    respond: (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }),
  });
  chat.render(session(SETTINGS_SESSION));
  const message = byId("message");
  const isOpen = () => message.getAttribute("aria-expanded") === "true";
  typeMessage(byId, "/");
  await settle();

  message.dispatchEvent(new FakeEvent("blur", { bubbles: false }));
  assert.equal(isOpen(), false);
  message.focus();
  assert.equal(isOpen(), true);
  // Moving the caret back closes it, and a click at the end opens it again.
  message.value = "/mo";
  message.setSelectionRange(1);
  message.dispatchEvent(new FakeEvent("keyup", { key: "ArrowLeft" }));
  assert.equal(isOpen(), false);
  message.setSelectionRange(3);
  message.click();
  assert.equal(isOpen(), true);

  chat.setConnected(false);
  assert.equal(isOpen(), false);
  chat.setConnected(true);
  typeMessage(byId, "/");
  chat.render(session({ ...SETTINGS_SESSION, status: "Done" }));
  assert.equal(isOpen(), false);

  chat.render(session(SETTINGS_SESSION));
  typeMessage(byId, "/");
  assert.equal(isOpen(), true);
  chat.open("s2");
  assert.equal(isOpen(), false);
});

test("@ in the composer lists the session's files, and Enter inserts one instead of sending", async () => {
  const storage = new MemoryStorage();
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings", "files"],
    respond: (body) =>
      body.method === "list_session_files"
        ? {
            files: [
              { is_dir: true, path: "src" },
              { is_dir: false, path: "src/main.rs" },
            ],
            kind: "session_files",
          }
        : { kind: "accepted" },
    sessionStorage: storage,
  });
  chat.render(session(SETTINGS_SESSION));
  const message = byId("message");

  typeMessage(byId, "Fix @mai");
  await settle();
  assert.deepEqual(suggested(byId), ["src/main.rs"]);
  assert.equal(pressKey(byId, "Enter").defaultPrevented, true);

  assert.equal(message.value, "Fix @src/main.rs ");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "Fix @src/main.rs ");
  assert.equal(message.getAttribute("aria-expanded"), "false");
  // With the list closed, Enter sends the message with the file in it.
  pressKey(byId, "Enter");
  await settle();
  assert.deepEqual(requests, [
    { method: "list_session_files", session_id: "s1" },
    { method: "send_message", message: "Fix @src/main.rs ", session_id: "s1" },
  ]);
});

test("slash commands take the composer first, and @ closes on blur and on a session switch", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings", "files"],
    respond: (body) => {
      if (body.method === "get_host") {
        return HOST;
      }

      return { files: [{ is_dir: false, path: "a.rs" }], kind: "session_files" };
    },
  });
  chat.render(session(SETTINGS_SESSION));
  const message = byId("message");
  const isOpen = () => message.getAttribute("aria-expanded") === "true";

  typeMessage(byId, "/");
  await settle();
  assert.deepEqual(suggested(byId), ["/model", "/reasoning", "/style", "/fast"]);
  // Leaving the command for a file closes the commands and lists files.
  typeMessage(byId, "/model @");
  await settle();
  assert.deepEqual(suggested(byId), ["a.rs"]);
  assert.deepEqual(
    requests.map((body) => body.method),
    ["get_host", "list_session_files"],
  );

  message.dispatchEvent(new FakeEvent("blur", { bubbles: false }));
  assert.equal(isOpen(), false);
  message.focus();
  assert.equal(isOpen(), true);
  chat.render(session(SETTINGS_SESSION));
  assert.equal(isOpen(), true);
  // Going back from files to a command keeps the commands through a
  // refresh.
  typeMessage(byId, "/");
  chat.render(session(SETTINGS_SESSION));
  assert.deepEqual(suggested(byId), ["/model", "/reasoning", "/style", "/fast"]);
  chat.open("s2");
  assert.equal(isOpen(), false);
});

test("without the files capability, @ is plain text", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["host", "settings"] });
  chat.render(session(SETTINGS_SESSION));
  typeMessage(byId, "@src");

  assert.equal(byId("message").getAttribute("aria-expanded"), "false");
  assert.deepEqual(requests, []);
});

test("historyKeyAction browses from the first or last line, without modifiers", () => {
  const key = (name, init = {}) => ({ key: name, ...init });

  assert.equal(historyKeyAction(key("ArrowUp"), "", 0), "back");
  assert.equal(historyKeyAction(key("ArrowUp"), "one line", 3), "back");
  assert.equal(historyKeyAction(key("ArrowUp"), "first\nsecond", 3), "back");
  assert.equal(historyKeyAction(key("ArrowUp"), "first\nsecond", 8), null);
  assert.equal(historyKeyAction(key("ArrowDown"), "first\nsecond", 8), "forward");
  assert.equal(historyKeyAction(key("ArrowDown"), "first\nsecond", 3), null);
  // A selection counts from its start going up and its end going down.
  assert.equal(historyKeyAction(key("ArrowUp"), "first\nsecond", 3, 8), "back");
  assert.equal(historyKeyAction(key("ArrowDown"), "first\nsecond", 3, 8), "forward");
  for (const modifier of ["altKey", "ctrlKey", "metaKey", "shiftKey", "isComposing"]) {
    assert.equal(historyKeyAction(key("ArrowUp", { [modifier]: true }), "", 0), null, modifier);
  }
  assert.equal(historyKeyAction(key("ArrowUp", { keyCode: 229 }), "", 0), null);
  assert.equal(historyKeyAction(key("Enter"), "", 0), null);
});

test("promptHistory lists the session's own prompts, oldest first", () => {
  assert.deepEqual(
    promptHistory(
      session({
        messages: [
          { content: "Build it", kind: "user" },
          { content: "Done.", kind: "assistant" },
          { content: "Merged", kind: "notice" },
          { content: "Add tests\nand docs", kind: "user" },
          { kind: "user" },
        ],
      }),
    ),
    ["Build it", "Add tests\nand docs"],
  );
  assert.deepEqual(promptHistory(session({ messages: [] })), []);
  assert.deepEqual(promptHistory({ id: "s1" }), []);
  assert.deepEqual(promptHistory(null), []);
});

const HISTORY_MESSAGES = [
  { content: "Build it", kind: "user" },
  { content: "Done.", kind: "assistant" },
  { content: "Add tests\nand docs", kind: "user" },
  { content: "Added.", kind: "assistant" },
];

function pressArrow(byId, key, init = {}) {
  const event = new FakeEvent("keydown", { key: key, ...init });
  byId("message").dispatchEvent(event);
  byId("message").dispatchEvent(new FakeEvent("keyup", { key: key }));

  return event.defaultPrevented;
}

test("Up and Down browse the session's prompts and bring the unsent draft back", () => {
  const storage = new MemoryStorage();
  const { byId, chat } = mountChat({ sessionStorage: storage });
  chat.render(session({ messages: HISTORY_MESSAGES }));
  const message = byId("message");
  typeMessage(byId, "half typed");

  // Newest first; the caret ends up at the end of the prompt.
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message.value, "Add tests\nand docs");
  assert.equal(message.selectionStart, message.value.length);
  assert.equal(message.rows, 2);
  // The draft is kept while a prompt shows.
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "half typed");
  // On a later line of a prompt, Up moves the caret, as the browser does.
  assert.equal(pressArrow(byId, "ArrowUp"), false);
  assert.equal(message.value, "Add tests\nand docs");
  message.setSelectionRange(3);
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message.value, "Build it");
  // The oldest prompt is as far back as it goes.
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message.value, "Build it");
  // Shift+Up selects instead.
  assert.equal(pressArrow(byId, "ArrowUp", { shiftKey: true }), false);

  assert.equal(pressArrow(byId, "ArrowDown"), true);
  assert.equal(message.value, "Add tests\nand docs");
  // On an earlier line, Down moves the caret.
  message.setSelectionRange(3);
  assert.equal(pressArrow(byId, "ArrowDown"), false);
  message.setSelectionRange(message.value.length);
  assert.equal(pressArrow(byId, "ArrowDown"), true);
  assert.equal(message.value, "half typed");
  assert.equal(message.rows, 1);
  // Back at the draft, Down is the browser's again.
  assert.equal(pressArrow(byId, "ArrowDown"), false);
  assert.equal(message.value, "half typed");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "half typed");
});

test("prompt history: none to browse, an edited prompt, and a session switch", () => {
  const storage = new MemoryStorage();
  const { byId, chat } = mountChat({ sessionStorage: storage });
  const message = byId("message");
  // A session without prompts of its own leaves the keys to the browser.
  chat.render(session({ messages: [{ content: "Merged", kind: "notice" }] }));
  assert.equal(pressArrow(byId, "ArrowUp"), false);
  assert.equal(pressArrow(byId, "ArrowDown"), false);
  assert.equal(message.value, "");

  // Editing a prompt ends browsing and makes it the draft, as in the
  // terminal.
  chat.render(session({ messages: HISTORY_MESSAGES }));
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  typeMessage(byId, "Add tests\nand docs, please");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "Add tests\nand docs, please");
  assert.equal(pressArrow(byId, "ArrowDown"), false);
  assert.equal(message.value, "Add tests\nand docs, please");

  // Switching sessions while a prompt shows comes back to the draft.
  typeMessage(byId, "my draft");
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  chat.open("s2");
  assert.equal(message.value, "");
  assert.equal(pressArrow(byId, "ArrowDown"), false);
  chat.open("s1");
  chat.render(session({ messages: HISTORY_MESSAGES }));
  assert.equal(message.value, "my draft");
  // Browsing starts afresh there, from the newest prompt.
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message.value, "Add tests\nand docs");
});

test("a sent earlier prompt gives way to the draft, and the draft stays saved", async () => {
  const storage = new MemoryStorage();
  const { byId, chat, requests } = mountChat({ sessionStorage: storage });
  chat.render(session({ messages: HISTORY_MESSAGES }));
  typeMessage(byId, "half typed");
  pressArrow(byId, "ArrowUp");
  message(byId).setSelectionRange(0);
  pressArrow(byId, "ArrowUp");
  assert.equal(pressKey(byId, "Enter").defaultPrevented, true);
  await settle();

  assert.deepEqual(requests, [{ method: "send_message", message: "Build it", session_id: "s1" }]);
  assert.equal(message(byId).value, "half typed");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "half typed");
  // Browsing ended with the send.
  assert.equal(pressArrow(byId, "ArrowDown"), false);
});

test("text sent while browsing starts is not brought back as the draft", async () => {
  const storage = new MemoryStorage();
  const reply = deferred();
  const { byId, chat, requests } = mountChat({
    respond: (body) => (body.method === "send_message" ? reply.promise : { kind: "accepted" }),
    sessionStorage: storage,
  });
  chat.render(session({ messages: HISTORY_MESSAGES }));
  typeMessage(byId, "fresh text");
  pressKey(byId, "Enter");
  // Before Agentty answers, Up starts browsing from the text in flight.
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message(byId).value, "Add tests\nand docs");
  reply.resolve({ kind: "accepted" });
  await settle();

  assert.deepEqual(requests, [{ method: "send_message", message: "fresh text", session_id: "s1" }]);
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), null);
  assert.equal(pressArrow(byId, "ArrowDown"), true);
  assert.equal(message(byId).value, "");
});

function message(byId) {
  return byId("message");
}

test("open suggestions take the arrow keys, and a shown prompt opens none", async () => {
  const { byId, chat, requests } = mountChat({
    capabilities: ["host", "settings", "files"],
    respond: (body) => {
      if (body.method === "get_host") {
        return HOST;
      }

      return { files: [{ is_dir: false, path: "README.md" }], kind: "session_files" };
    },
  });
  chat.render(
    session({
      ...SETTINGS_SESSION,
      messages: [{ content: "/tmp/notes", kind: "user" }, { content: "Read @REA", kind: "user" }],
    }),
  );
  const isOpen = () => message(byId).getAttribute("aria-expanded") === "true";

  // With the slash menu open, Up moves its highlight.
  typeMessage(byId, "/");
  await settle();
  assert.equal(isOpen(), true);
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message(byId).value, "/");
  assert.equal(isOpen(), true);
  // Once the menu closes, Up browses.
  message(byId).dispatchEvent(new FakeEvent("blur", { bubbles: false }));
  assert.equal(isOpen(), false);
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message(byId).value, "Read @REA");
  // The prompt ends in an `@` token, yet no file list opens over it, so
  // Up keeps browsing.
  assert.equal(isOpen(), false);
  assert.deepEqual(
    requests.map((body) => body.method),
    ["get_host"],
  );
  assert.equal(pressArrow(byId, "ArrowUp"), true);
  assert.equal(message(byId).value, "/tmp/notes");
  assert.equal(isOpen(), false);
  // Back at the draft, the suggestions follow the text again.
  pressArrow(byId, "ArrowDown");
  pressArrow(byId, "ArrowDown");
  assert.equal(message(byId).value, "/");
  assert.equal(isOpen(), true);
});

test("Cancel session asks first and shows the host's refusal under the header", async () => {
  const { byId, chat, requests } = mountChat({
    respond: () => {
      throw new Error("Session not found");
    },
  });
  chat.render(session());
  byId("session-more").click();
  // A tap on the backdrop closes the sheet without an action.
  const sheet = byId("session-sheet");
  sheet.dispatchEvent(new FakeEvent("click"));
  assert.equal(sheet.open, false);
  assert.equal(byId("cancel-dialog").open, false);

  byId("session-more").click();
  byId("cancel").click();
  byId("cancel-dialog").querySelector('button[value="confirm"]').click();
  await settle();
  assert.deepEqual(requests, [{ method: "cancel_session", session_id: "s1" }]);
  assert.equal(byId("action-error").textContent, "Session not found");
});

test("new content follows the reader near the end, and otherwise offers Jump to latest", () => {
  const { byId, chat } = mountChat();
  const scroller = byId("chat-scroll");
  scroller.clientHeight = 500;
  scroller.scrollHeight = 1000;
  chat.render(session());
  // The first snapshot opens at the end.
  assert.equal(scroller.scrollTop, 1000);

  // Reading near the end: the next message scrolls into view. The fake
  // DOM has no layout, so the height stays put.
  scroller.scrollTop = 460;
  chat.render(session({ messages: [...session().messages, { content: "a", kind: "user" }] }));
  assert.equal(scroller.scrollTop, 1000);
  assert.equal(byId("jump").hidden, true);

  // Scrolled up: the position stays and the pill appears.
  scroller.scrollTop = 100;
  scroller.scrollHeight = 1400;
  const grown = [...session().messages, { content: "a", kind: "user" }, { content: "b", kind: "user" }];
  chat.render(session({ messages: grown }));
  assert.equal(scroller.scrollTop, 100);
  assert.equal(byId("jump").hidden, false);
  // A refresh without new content leaves it alone.
  chat.render(session({ messages: grown }));
  assert.equal(scroller.scrollTop, 100);

  byId("jump").click();
  assert.equal(scroller.scrollTop, 1400);
  assert.equal(byId("jump").hidden, true);

  // Scrolling back to the end by hand also hides it.
  scroller.scrollTop = 0;
  chat.render(session({ messages: [...grown, { content: "c", kind: "user" }] }));
  assert.equal(byId("jump").hidden, false);
  scroller.scrollTop = 1400 - 500;
  scroller.dispatchEvent(new FakeEvent("scroll"));
  assert.equal(byId("jump").hidden, true);
});

// Sets what a scroller reports: a hidden one reports zeros.
function layout(scroller, { client, height, top }) {
  scroller.clientHeight = client;
  scroller.scrollHeight = height;
  scroller.scrollTop = top;
}

function diffAnswer(body) {
  return body.method === "get_session_diff"
    ? { files: [{ added_lines: 1, deleted_lines: 0, path: "a.rs", status: "added" }], kind: "diff" }
    : { kind: "accepted" };
}

test("back from the diff, the transcript returns to where the reader was", () => {
  const { byId, chat } = mountChat({ capabilities: ["diff"], respond: diffAnswer });
  const scroller = byId("chat-scroll");
  layout(scroller, { client: 500, height: 2000, top: 0 });
  chat.render(session());
  // Reading in the middle, then the diff; the hidden scroller reports
  // zeros, as a browser's does.
  scroller.scrollTop = 600;
  byId("diff").click();
  layout(scroller, { client: 0, height: 0, top: 0 });
  const grown = [...session().messages, { content: "more", kind: "assistant" }];
  chat.render(session({ messages: grown, updated_at: 11 }));
  assert.equal(byId("jump").hidden, true);

  layout(scroller, { client: 500, height: 2400, top: 0 });
  byId("diff").click();
  assert.equal(scroller.scrollTop, 600);
  // A message arrived meanwhile, so the pill offers it.
  assert.equal(byId("jump").hidden, false);
});

test("back from the diff, a reader who was at the end sees the end", () => {
  const { byId, chat } = mountChat({ capabilities: ["diff"], respond: diffAnswer });
  const scroller = byId("chat-scroll");
  layout(scroller, { client: 500, height: 2000, top: 0 });
  chat.render(session());
  assert.equal(scroller.scrollTop, 2000);
  byId("diff").click();
  layout(scroller, { client: 0, height: 0, top: 0 });
  chat.render(session({ updated_at: 11 }));

  layout(scroller, { client: 500, height: 2400, top: 0 });
  byId("diff").click();
  assert.equal(scroller.scrollTop, 2400);
  assert.equal(byId("jump").hidden, true);
  // Nothing new arrived for a reader in the middle either: no pill.
  scroller.scrollTop = 100;
  byId("diff").click();
  byId("diff").click();
  assert.equal(scroller.scrollTop, 100);
  assert.equal(byId("jump").hidden, true);
});

test("the open diff is read again only when the session's changes may differ", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["diff"], respond: diffAnswer });
  const diffReads = () => requests.filter((body) => body.method === "get_session_diff").length;
  chat.render(detailedSession(), detailedSummary());
  byId("diff").click();
  await settle();
  assert.equal(diffReads(), 1);

  // A running session updates often; the same line counts read nothing.
  chat.render(detailedSession({ updated_at: 11 }), detailedSummary({ updated_at: 11 }));
  await settle();
  assert.equal(diffReads(), 1);
  chat.render(detailedSession({ updated_at: 12 }), detailedSummary({ added_lines: 65 }));
  await settle();
  assert.equal(diffReads(), 2);

  // Without counts in the listing, an update stands in for them.
  assert.equal(diffKey(session(), null), "updated:10");
  assert.equal(diffKey(session(), detailedSummary()), "lines:64:9");
});

test("queued messages show one row each with the full text in the title", () => {
  const { byId, chat } = mountChat();
  chat.render(session({ queued_messages: ["first", "second"], status: "InProgress" }));
  assert.equal(byId("queued-box").hidden, false);
  const [first] = byId("queued").children;
  assert.deepEqual(
    Array.from(byId("queued").children, (item) => [item.title, item.textContent]),
    [
      ["first", "first"],
      ["second", "second"],
    ],
  );
  chat.render(session({ queued_messages: ["first"], status: "InProgress" }));
  assert.equal(byId("queued").children.length, 1);
  assert.equal(byId("queued").children[0], first);
  chat.render(session({ queued_messages: [], status: "InProgress" }));
  assert.equal(byId("queued-box").hidden, true);
});

test("the header shows the title and status, and a missing session leads home", () => {
  const { byId, chat, homes } = mountChat();
  chat.render(session({ status: "InProgress", title: null }));
  assert.equal(byId("heading").textContent, "No title");
  assert.equal(byId("session-status").textContent, "In progress");
  // The header badge holds the status dot and label in the status's tone.
  assert.equal(byId("session-status").className, "badge wide-only");
  assert.equal(byId("session-status").querySelector(".status").className, "status status--working");
  assert.equal(byId("session-status").querySelector("i").className, "status-dot");
  assert.equal(byId("heading-sub").textContent, "In progress");

  chat.render(null);
  assert.equal(byId("session-gone").hidden, false);
  assert.equal(byId("chat-scroll").hidden, true);
  assert.equal(byId("composer").hidden, true);
  assert.equal(byId("session-more").hidden, true);
  byId("session-gone-home").click();
  assert.equal(homes(), 1);

  chat.close();
  assert.equal(byId("session-status").hidden, true);
  assert.equal(byId("transcript").children.length, 0);
  assert.equal(byId("heading-sub").textContent, "");
});

// A snapshot with every detail a current host sends, and its entry in the
// listing.
function detailedSession(overrides = {}) {
  return session({
    agent: "claude",
    base_branch: "main",
    branch: "wt/3f9a1c2e",
    model: "claude-opus-5-5",
    reasoning: "high",
    response_style: "balanced",
    review_request: {
      display_id: "#1261",
      source_branch: "wt/3f9a1c2e",
      state: "Open",
      status_summary: "PR ready",
      target_branch: "main",
      title: "Serve the page",
      web_url: "https://github.com/example/project/pull/1261",
    },
    speed_mode: "normal",
    ...overrides,
  });
}

function detailedSummary(overrides = {}) {
  return {
    added_lines: 64,
    deleted_lines: 9,
    id: "s1",
    model: "claude-opus-5-5",
    model_short_name: "opus-5.5",
    review_ready: "ready",
    size: "M",
    status: "Review",
    updated_at: 10,
    ...overrides,
  };
}

// The text of each part of the header's sub-line, after the status.
function subParts(byId) {
  return Array.from(byId("heading-sub").children, (node) => node.textContent).slice(1);
}

test("Merge is off for a session with a review request, and says why", () => {
  const { byId, chat } = mountChat({ capabilities: ["merge"] });

  chat.render(detailedSession({ review_request: undefined }), detailedSummary());
  assert.equal(byId("merge").disabled, false);
  assert.equal(byId("merge").title, "");
  assert.equal(byId("sheet-merge-note").textContent, "asks first");

  chat.render(detailedSession(), detailedSummary());
  assert.equal(byId("merge").hidden, false);
  assert.equal(byId("merge").disabled, true);
  assert.equal(byId("sheet-merge").disabled, true);
  assert.equal(byId("merge").title, "Merge is off: merge the session's pull request instead");
  // The sheet's note stays short enough for a phone.
  assert.equal(byId("sheet-merge-note").textContent, "has a pull request");
});

test("the header names a merge conflict with the base branch", () => {
  const { byId, chat } = mountChat();

  chat.render(detailedSession(), detailedSummary({ has_merge_conflict: true }));
  assert.deepEqual(subParts(byId).slice(0, 3), ["[M]", "+64 -9", "Merge conflict with main"]);
  assert.equal(byId("heading-sub").querySelector(".conflict").textContent, "Merge conflict with main");
  // A phone shows the conflict without the branch it is with.
  assert.equal(byId("heading-sub").querySelector(".conflict .wide-only").textContent, " with main");

  // An older host names no base branch.
  chat.render(session(), detailedSummary({ has_merge_conflict: true }));
  assert.equal(byId("heading-sub").querySelector(".conflict").textContent, "Merge conflict");

  chat.render(detailedSession(), detailedSummary());
  assert.equal(byId("heading-sub").querySelector(".conflict"), null);
});

test("the header shows size, lines, branches, model and the review request", () => {
  const { byId, chat } = mountChat();
  chat.render(detailedSession(), detailedSummary());

  assert.deepEqual(subParts(byId), [
    "[M]",
    "+64 -9",
    "wt/3f9a1c2e → main",
    "claude/opus-5.5 [high]",
    "#1261 open · PR ready",
  ]);
  const sub = byId("heading-sub");
  assert.equal(sub.querySelector(".add").textContent, "+64");
  assert.equal(sub.querySelector(".del").textContent, "-9");
  // The branch and model give way on a phone; the status takes their place.
  assert.equal(sub.children[3].className, "branch wide-only");
  assert.equal(sub.querySelector(".model").classList.contains("wide-only"), true);
  const link = sub.querySelector("a");
  assert.equal(link.getAttribute("href"), "https://github.com/example/project/pull/1261");
  assert.equal(link.getAttribute("rel"), "noopener noreferrer");
  assert.equal(link.getAttribute("target"), "_blank");
  assert.equal(link.title, "Serve the page");
});

test("the header hides whatever an older host omits", () => {
  const { byId, chat } = mountChat();
  chat.render(session(), null);
  assert.deepEqual(subParts(byId), []);
  assert.equal(byId("heading-sub").textContent, "Review");

  // Without a listing entry the model shows by its id, and the summary of
  // another session is not used.
  chat.render(
    detailedSession({ base_branch: undefined, review_request: undefined }),
    detailedSummary({ id: "s2" }),
  );
  assert.deepEqual(subParts(byId), ["wt/3f9a1c2e", "claude-opus-5-5 [high]"]);

  chat.render(detailedSession({ reasoning: undefined }), { id: "s1", status: "Review" });
  assert.deepEqual(subParts(byId), [
    "wt/3f9a1c2e → main",
    "claude-opus-5-5",
    "#1261 open · PR ready",
  ]);
});

test("the header shows how far the branch is from its base and from its upstream", () => {
  const { byId, chat } = mountChat();
  const published = detailedSession({ published_upstream_ref: "origin/wt/3f9a1c2e" });
  const distances = { base_ahead: 3, base_behind: 1, upstream_ahead: 0, upstream_behind: 0 };

  chat.render(published, detailedSummary(distances));
  // The upstream named after the session branch shows by its remote. Each
  // branch and its counts are one part, so a full line drops them together.
  assert.deepEqual(subParts(byId).slice(2, 4), ["wt/3f9a1c2e → main ↓1 ↑3", "⇡ origin ✓"]);
  const sub = byId("heading-sub");
  const [base, upstream] = sub.querySelectorAll(".branch-counts");
  assert.equal(base.title, "Behind and ahead of main");
  assert.equal(upstream.title, "Even with origin/wt/3f9a1c2e");
  assert.equal(base.parentElement, sub.querySelector(".branch"));
  assert.equal(upstream.parentElement, sub.querySelector(".upstream"));
  assert.equal(sub.querySelector(".branch-name").title, "wt/3f9a1c2e → main");
  assert.equal(sub.querySelector(".upstream").title, "Published to origin/wt/3f9a1c2e");
  assert.equal(sub.querySelector(".upstream").classList.contains("wide-only"), true);

  // Before the host compares the branches, only the names show, and an
  // upstream under another name shows whole; an unpublished branch has
  // no upstream.
  chat.render(
    detailedSession({ published_upstream_ref: "origin/review/pager" }),
    detailedSummary(),
  );
  assert.deepEqual(subParts(byId).slice(2, 4), ["wt/3f9a1c2e → main", "⇡ origin/review/pager"]);
  chat.render(detailedSession(), detailedSummary(distances));
  assert.equal(byId("heading-sub").querySelector(".upstream"), null);
  // Without a base branch there is nothing to count against.
  chat.render(detailedSession({ base_branch: undefined }), detailedSummary(distances));
  assert.equal(subParts(byId)[2], "wt/3f9a1c2e");
});

test("wrappedParts finds the header parts below its one line", () => {
  // A 20px line at 100: parts on it may sit a little lower when centred.
  assert.deepEqual(wrappedParts([100, 102, 120, 140], 100, 20), [false, false, true, true]);
  // A part with no box measures at 0, above the line.
  assert.deepEqual(wrappedParts([0, 100], 100, 20), [false, false]);
  // Without layout every box is empty, and nothing counts as wrapped.
  assert.deepEqual(wrappedParts([0, 0], 0, 0), [false, false]);
});

test("header parts that wrap off the line are inert until they fit again", () => {
  const observers = [];
  class FakeResizeObserver {
    constructor(callback) {
      this.callback = callback;
      observers.push(this);
    }

    observe(target) {
      this.target = target;
    }
  }
  const { byId, chat, document } = mountChat({ ResizeObserver: FakeResizeObserver });
  // A line that holds everything up to the review link; the rest wraps.
  let wrapFrom = ["timer", "branch", "model", "upstream", "started"];
  document.layout = (node) => {
    if (node === byId("heading-sub")) {
      return { height: 20, top: 100 };
    }
    const isWrapped = wrapFrom.some((part) => node.classList.contains(part));

    return { height: 20, top: isWrapped ? 120 : 100 };
  };
  const inert = () =>
    Array.from(byId("heading-sub").children)
      .filter((node) => node.hasAttribute("inert"))
      .map((node) => node.className.split(" ")[0]);

  chat.render(detailedSession(), detailedSummary({ in_progress_total_seconds: 60 }));
  assert.deepEqual(inert(), ["branch", "model", "timer"]);
  const link = byId("heading-sub").querySelector("a");
  link.focus();
  assert.equal(link.hasAttribute("inert"), false);

  // A wider line, as when the sidebar hides, frees what fits again; a
  // refresh keeps the focused link in place and never inert.
  assert.equal(observers[0].target, byId("heading-sub"));
  wrapFrom = ["model"];
  observers[0].callback();
  assert.deepEqual(inert(), ["model"]);
  chat.render(detailedSession({ updated_at: 11 }), detailedSummary({ in_progress_total_seconds: 60 }));
  assert.equal(byId("heading-sub").querySelector("a"), link);
  assert.equal(document.activeElement, link);
  assert.deepEqual(inert(), ["model"]);
});

// The sheet's detail rows as `[label, value]`.
function sheetRows(byId) {
  return Array.from(byId("sheet-details").children, (row) => [
    row.querySelector("dt").textContent,
    row.querySelector("dd").textContent,
  ]);
}

test("the sheet lists the session's details in full, and an older host's none", () => {
  let time = 20000;
  const { byId, chat } = mountChat({ now: () => time });
  const full = detailedSession({
    created_at: 20000 - 11100,
    published_upstream_ref: "origin/review/a-very-long-branch-name-for-the-pager",
  });
  const summary = detailedSummary({
    base_ahead: 3,
    base_behind: 1,
    in_progress_started_at: 19900,
    in_progress_total_seconds: 20,
    status: "InProgress",
    upstream_ahead: 0,
    upstream_behind: 2,
  });

  chat.render(full, summary);
  assert.equal(byId("sheet-details").hidden, false);
  assert.deepEqual(sheetRows(byId), [
    ["Branch", "wt/3f9a1c2e → main ↓1 ↑3"],
    ["Upstream", "origin/review/a-very-long-branch-name-for-the-pager ↓2 ↑0"],
    ["Model", "claude/opus-5.5 [high]"],
    ["Timer", "2m 0s"],
    ["Started", "3h 5m ago"],
    ["Pull request", "#1261 open · PR ready"],
  ]);
  const link = byId("sheet-details").querySelector("a");
  assert.equal(link.getAttribute("href"), "https://github.com/example/project/pull/1261");
  assert.equal(link.getAttribute("rel"), "noopener noreferrer");

  // The timer row ticks with the header, in place, and a refresh keeps
  // the rows, so a focused link keeps its focus.
  const timer = byId("sheet-details").querySelector(".sheet-timer");
  time = 20001;
  chat.tick();
  assert.equal(timer.textContent, "2m 1s");
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer: 2m 1s");
  chat.render(full, summary);
  assert.equal(byId("sheet-details").querySelector("a"), link);
  assert.equal(byId("sheet-details").querySelector(".sheet-timer"), timer);

  // An older host sends none of the optional fields: no rows, no list.
  chat.render(session(), null);
  assert.deepEqual(sheetRows(byId), []);
  assert.equal(byId("sheet-details").hidden, true);
  // A host that sends some of them gets only those rows.
  chat.render(detailedSession({ review_request: undefined }), null);
  assert.deepEqual(
    sheetRows(byId).map(([label]) => label),
    ["Branch", "Model"],
  );
});

test("the sheet links a review request only through the Markdown link filter", () => {
  const { byId, chat } = mountChat();
  chat.render(
    detailedSession({
      review_request: { display_id: "#9", state: "Open", title: "", web_url: "javascript:alert(1)" },
    }),
  );

  assert.equal(byId("sheet-details").querySelector("a"), null);
  assert.deepEqual(sheetRows(byId).at(-1), ["Pull request", "#9 open"]);
});

test("the sheet button shows for the details on wide screens too", () => {
  // Details alone open the sheet everywhere.
  const details = mountChat();
  details.chat.render(detailedSession({ status: "Done" }), null);
  assert.equal(details.byId("session-more").hidden, false);
  assert.equal(details.byId("session-more").classList.contains("narrow-only"), false);

  // Only Merge, which the wide header shows, and no details: phones only.
  const merge = mountChat({ capabilities: ["merge"] });
  merge.chat.render(session({ status: "Merged" }), null);
  assert.equal(merge.byId("session-more").hidden, false);
  assert.equal(merge.byId("session-more").classList.contains("narrow-only"), true);

  // No details and nothing to offer: no button.
  const none = mountChat();
  none.chat.render(session({ status: "Done" }), null);
  assert.equal(none.byId("session-more").hidden, true);
  // Leaving the session clears the details and the button.
  details.chat.close();
  assert.equal(details.byId("sheet-details").hidden, true);
  assert.equal(details.byId("session-more").hidden, true);
});

test("queued actions follow the queued messages, marked as actions", () => {
  const { byId, chat } = mountChat();
  chat.render(
    session({
      queued_actions: ["sync — rebase onto the base branch after this turn"],
      queued_messages: ["first"],
      status: "InProgress",
    }),
  );
  const queued = () => Array.from(byId("queued").children);

  assert.equal(byId("queued-box").hidden, false);
  assert.deepEqual(
    queued().map((item) => [item.className, item.textContent]),
    [
      ["", "first"],
      ["queued-action", "Actionsync — rebase onto the base branch after this turn"],
    ],
  );
  assert.equal(queued()[1].querySelector(".queued-tag").textContent, "Action");
  assert.equal(queued()[1].title, "sync — rebase onto the base branch after this turn");

  // A message with the text of an action is still a message.
  const action = queued()[1];
  chat.render(session({ queued_actions: ["first"], queued_messages: [], status: "InProgress" }));
  assert.deepEqual(
    queued().map((item) => item.className),
    ["queued-action"],
  );
  assert.notEqual(queued()[0], action);
  // Actions alone show the box; an older host sends none.
  chat.render(session({ queued_actions: undefined, status: "InProgress" }));
  assert.equal(byId("queued-box").hidden, true);
});

test("a draft session previews its staged messages as text, and a started one does not", () => {
  const { byId, chat } = mountChat();
  const draft = { is_draft: true, messages: [], status: "Draft" };
  const hostile = "<img src=x onerror=alert(1)>\nsecond line";

  chat.render(session({ ...draft, draft_prompt: hostile }));
  const preview = byId("draft-preview");
  assert.equal(preview.hidden, false);
  assert.deepEqual(
    Array.from(preview.children, (node) => [node.className, node.textContent]),
    [
      ["eyebrow", "Draft session"],
      ["draft-note", "Staged on the host. Your next message starts the session with it."],
      ["msg msg--user", hostile],
    ],
  );
  assert.equal(preview.querySelector("img"), null);

  // Nothing staged yet, or an older host that does not send it.
  chat.render(session(draft));
  assert.deepEqual(
    Array.from(preview.children, (node) => node.textContent),
    ["Draft session", "Your first message starts the session."],
  );
  // A started draft and a regular session show no preview.
  chat.render(session({ ...draft, draft_prompt: hostile, status: "InProgress" }));
  assert.equal(preview.hidden, true);
  assert.equal(preview.children.length, 0);
  chat.render(session({ draft_prompt: hostile, status: "Draft" }));
  assert.equal(preview.hidden, true);
  // Leaving the session clears it.
  chat.render(session({ ...draft, draft_prompt: hostile }));
  chat.close();
  assert.equal(preview.hidden, true);
});

test("a finished focused review renders under the transcript as an answer does", () => {
  const { byId, chat } = mountChat();
  const text = "### Suggestions\n- Add a `test` <img src=x onerror=alert(1)>";

  chat.render(session({ focused_review: { state: "ready", text: text } }));

  const block = byId("focused-review");
  assert.equal(block.hidden, false);
  assert.equal(block.className, "focused-review");
  const column = Array.from(byId("chat-scroll").querySelector(".chat-column").children);
  assert.deepEqual(
    column.slice(column.indexOf(byId("transcript")), column.indexOf(byId("status-line")) + 1),
    [byId("transcript"), block, byId("status-line")],
  );
  assert.equal(block.querySelector(".eyebrow").textContent, "Focused review");
  const body = block.querySelector(".focused-review-body");
  assert.equal(body.querySelector(".md h3").textContent, "Suggestions");
  assert.equal(body.querySelector(".md code").textContent, "test");
  assert.equal(body.querySelector("img"), null);
  assert.equal(body.querySelector(".md-source").textContent, text);
  assert.equal(body.querySelector(".md-source").hidden, true);
  const toggle = body.querySelector(".source-toggle");
  toggle.click();
  assert.deepEqual(
    [body.querySelector(".md").hidden, body.querySelector(".md-source").hidden, toggle.textContent],
    [true, false, "View rendered"],
  );
});

test("a focused review keeps its node and source view until it changes, then hides", () => {
  const { byId, chat } = mountChat();
  chat.render(session({ focused_review: { state: "ready", text: "**First**" } }));
  const block = byId("focused-review");
  const body = block.querySelector(".focused-review-body");
  body.querySelector(".source-toggle").click();

  chat.render(session({ focused_review: { state: "ready", text: "**First**" }, updated_at: 11 }));
  assert.equal(block.querySelector(".focused-review-body"), body);

  chat.render(session({ focused_review: { state: "ready", text: "**Second**" } }));
  const changed = block.querySelector(".focused-review-body");
  assert.notEqual(changed, body);
  assert.equal(changed.querySelector(".md-source").hidden, false);
  assert.equal(changed.querySelector(".md-source").textContent, "**Second**");

  // A cleared review, an older host, and an unknown state show nothing.
  for (const focusedReview of [undefined, null, { state: "running", text: "x" }]) {
    chat.render(session({ focused_review: { state: "ready", text: "**Third**" } }));
    chat.render(session({ focused_review: focusedReview }));
    assert.equal(block.hidden, true);
    assert.equal(block.children.length, 0);
  }
  // Leaving the session clears it.
  chat.render(session({ focused_review: { state: "ready", text: "**Third**" } }));
  chat.close();
  assert.equal(block.hidden, true);
  assert.equal(block.children.length, 0);
});

test("a failed focused review shows its message as plain text", () => {
  const { byId, chat } = mountChat();
  const message = "Review assist unavailable: **timeout** <img src=x>";

  chat.render(session({ focused_review: { state: "failed", text: message } }));

  const block = byId("focused-review");
  assert.equal(block.hidden, false);
  assert.equal(block.className, "focused-review is-failed");
  assert.equal(block.querySelector(".eyebrow").textContent, "Focused review");
  const failure = block.querySelector(".focused-review-failure");
  assert.equal(failure.textContent, message);
  assert.equal(failure.children.length, 0);
  assert.equal(block.querySelector(".source-toggle"), null);

  chat.render(session({ focused_review: { state: "ready", text: "Fixed" } }));
  assert.equal(block.className, "focused-review");
});

test("a new focused review follows a reader at the end and offers the jump otherwise", () => {
  const { byId, chat } = mountChat();
  const scroller = byId("chat-scroll");
  chat.render(session());
  scroller.scrollHeight = 2000;
  scroller.clientHeight = 500;
  scroller.scrollTop = 1500;

  chat.render(session({ focused_review: { state: "ready", text: "Looks good" } }));
  assert.equal(scroller.scrollTop, 2000);
  assert.equal(byId("jump").hidden, true);

  scroller.scrollTop = 100;
  chat.render(session({ focused_review: { state: "ready", text: "Looks better" } }));
  assert.equal(scroller.scrollTop, 100);
  assert.equal(byId("jump").hidden, false);
});

test("a review request links only through the Markdown link filter", () => {
  const { byId, chat } = mountChat();
  for (const webUrl of ["javascript:alert(1)", "data:text/html,x", "/relative", ""]) {
    chat.render(
      detailedSession({
        review_request: { display_id: "#9", state: "Open", title: "", web_url: webUrl },
      }),
    );
    const sub = byId("heading-sub");
    assert.equal(sub.querySelector("a"), null, webUrl);
    assert.equal(sub.querySelector(".review").textContent, "#9 open", webUrl);
    assert.equal(sub.querySelector(".review").title, "", webUrl);
  }
});

test("a refresh keeps the header parts that did not change, and the focus on the link", () => {
  const { byId, chat, document } = mountChat();
  chat.render(detailedSession(), detailedSummary());
  const link = byId("heading-sub").querySelector("a");
  const size = byId("heading-sub").querySelector(".size");
  link.focus();

  chat.render(detailedSession({ updated_at: 11 }), detailedSummary({ added_lines: 70 }));
  assert.equal(byId("heading-sub").querySelector("a"), link);
  assert.equal(byId("heading-sub").querySelector(".size"), size);
  assert.equal(document.activeElement, link);
  assert.equal(byId("heading-sub").querySelector(".add").textContent, "+70");

  // Parts that go away are removed, and the timer stays last.
  chat.render(
    detailedSession({ review_request: undefined, status: "InProgress" }),
    detailedSummary({ in_progress_started_at: 0, status: "InProgress" }),
  );
  const sub = byId("heading-sub");
  assert.equal(sub.querySelector("a"), null);
  assert.equal(sub.children[sub.children.length - 1].className, "timer");
  chat.render(detailedSession({ status: "InProgress" }), detailedSummary({ in_progress_started_at: 0 }));
  assert.equal(sub.children[sub.children.length - 1].className, "timer");
  assert.equal(sub.querySelector("a").textContent, "#1261 open · PR ready");
});

test("the timer allows for a device clock behind the host's", () => {
  let time = 100;
  const { byId, chat } = mountChat({ now: () => time });
  // The host started the turn at 200 by its clock, so this device is at
  // least 100 seconds behind.
  chat.render(
    detailedSession({ status: "InProgress" }),
    detailedSummary({ in_progress_started_at: 200, in_progress_total_seconds: 10, updated_at: 190 }),
  );
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer: 10s");
  time = 105;
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer: 15s");

  assert.equal(clockSkew({ updated_at: 50 }, 100), 0);
  assert.equal(clockSkew({ updated_at: 150 }, 100), 50);
  assert.equal(clockSkew(null, 100), 0);
});

test("the header's timer counts while the session runs and stands still between turns", () => {
  let time = 1000;
  const { byId, chat } = mountChat({ now: () => time });
  const running = detailedSummary({
    in_progress_started_at: 900,
    in_progress_total_seconds: 152,
    status: "InProgress",
  });
  chat.render(detailedSession({ status: "InProgress" }), running);
  const timer = byId("heading-sub").querySelector(".timer");
  assert.equal(timer.textContent, "Timer: 4m 12s");

  time = 1001;
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer"), timer);
  assert.equal(timer.textContent, "Timer: 4m 13s");

  // The turn ended: the total the host counted stays, still.
  chat.render(detailedSession(), detailedSummary({ in_progress_total_seconds: 253 }));
  const stopped = byId("heading-sub").querySelector(".timer");
  assert.equal(stopped.textContent, "Timer: 4m 13s");
  time = 1100;
  chat.tick();
  assert.equal(stopped.textContent, "Timer: 4m 13s");
  // So does a finished session's.
  chat.render(detailedSession({ status: "Done" }), detailedSummary({ in_progress_total_seconds: 253 }));
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer: 4m 13s");

  // A session that never worked, or a host that lists no total, shows none.
  chat.render(detailedSession({ status: "Draft" }), detailedSummary({ in_progress_total_seconds: 0 }));
  assert.equal(byId("heading-sub").querySelector(".timer"), null);
  chat.render(detailedSession(), detailedSummary());
  assert.equal(byId("heading-sub").querySelector(".timer"), null);
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer"), null);

  // Ticking with no session open does nothing.
  chat.close();
  chat.tick();
  assert.equal(byId("heading-sub").textContent, "");
});

test("the header says how long ago the session started, after the timer", () => {
  let time = 20_000;
  const { byId, chat } = mountChat({ now: () => time });
  const sub = byId("heading-sub");
  const tail = () => Array.from(sub.children, (node) => node.className).slice(-2);
  chat.render(
    detailedSession({ created_at: 20_000 - 3 * 3600 - 5 * 60 }),
    detailedSummary({ in_progress_total_seconds: 252 }),
  );
  assert.deepEqual(tail(), ["timer", "started"]);
  const started = sub.querySelector(".started");
  assert.equal(started.textContent, "Started 3h 5m ago");
  assert.equal(sub.querySelector(".timer").textContent, "Timer: 4m 12s");

  // The age moves on with the clock, in place.
  time += 60;
  chat.tick();
  assert.equal(sub.querySelector(".started"), started);
  assert.equal(started.textContent, "Started 3h 6m ago");

  // A refresh keeps both last, and a session that never worked shows the
  // age alone.
  chat.render(detailedSession({ created_at: time - 30, status: "Draft" }), detailedSummary());
  assert.deepEqual(tail().slice(-1), ["started"]);
  assert.equal(sub.querySelector(".timer"), null);
  assert.equal(sub.querySelector(".started").textContent, "Started just now");
  // The timer comes back before the age once the session works.
  chat.render(
    detailedSession({ created_at: time - 30, status: "InProgress" }),
    detailedSummary({ in_progress_started_at: time - 5, status: "InProgress" }),
  );
  assert.deepEqual(tail(), ["timer", "started"]);
  assert.equal(sub.querySelector(".timer").textContent, "Timer: 5s");

  // An older host sends no creation time, and the page leaves the age out.
  chat.render(detailedSession(), detailedSummary({ in_progress_total_seconds: 252 }));
  assert.equal(sub.querySelector(".started"), null);
  assert.deepEqual(tail().slice(-1), ["timer"]);
});

test("the composer line shows the settings, and the key hint moves below", () => {
  const { byId, chat } = mountChat();
  chat.render(detailedSession(), detailedSummary());
  assert.equal(
    byId("composer-meta").textContent,
    "claude/opus-5.5 [high]·balanced·normal speed",
  );
  assert.equal(byId("composer-meta").querySelector("b").textContent, "claude/opus-5.5");
  const separators = byId("composer-meta").querySelectorAll(".sep");
  assert.equal(separators.length, 2);
  assert.equal(separators[0].getAttribute("aria-hidden"), "true");
  assert.equal(byId("composer-hint").textContent, "Enter sends · Shift+Enter adds a line");

  // An agent without a speed control sends no speed.
  chat.render(detailedSession({ speed_mode: undefined }), detailedSummary());
  assert.equal(byId("composer-meta").textContent, "claude/opus-5.5 [high]·balanced");

  // An older host names no settings, so the hint stays in the line.
  chat.render(session());
  assert.equal(byId("composer-meta").textContent, "Enter sends · Shift+Enter adds a line");
  assert.equal(byId("composer-hint").textContent, "");

  chat.render(detailedSession());
  chat.close();
  assert.equal(byId("composer-hint").textContent, "");
});

test("the merge confirmation names the base branch and the line counts", () => {
  const { byId, chat } = mountChat({ capabilities: ["merge"] });
  chat.render(detailedSession(), detailedSummary());
  assert.equal(byId("merge-target").textContent, "main");
  assert.equal(byId("merge-lines").textContent, " (+64 -9)");

  chat.render(detailedSession(), detailedSummary({ added_lines: 0, deleted_lines: 0 }));
  assert.equal(byId("merge-lines").textContent, "");
  chat.render(session(), null);
  assert.equal(byId("merge-lines").textContent, "");
});

// A window whose timers run only when the test says so, and a clock the
// test moves.
function manualTimers() {
  const timers = [];
  let time = 0;

  return {
    advance: (milliseconds) => {
      time += milliseconds;
      for (const timer of timers.filter((entry) => entry.at <= time)) {
        timers.splice(timers.indexOf(timer), 1);
        timer.callback();
      }
    },
    now: () => time,
    pending: () => timers.length,
    window: {
      clearTimeout: (timer) => {
        const index = timers.indexOf(timer);
        if (index >= 0) {
          timers.splice(index, 1);
        }
      },
      setTimeout: (callback, wait) => {
        const timer = { at: time + wait, callback: callback };
        timers.push(timer);

        return timer;
      },
    },
  };
}

test("refreshes run at most once a second and coalesce while one runs", async () => {
  const document = createDocument(INDEX_HTML);
  const clock = manualTimers();
  let runs = 0;
  let finish = () => {};
  const gate = createRefreshGate({
    document: document,
    now: clock.now,
    run: () =>
      new Promise((resolve) => {
        runs += 1;
        finish = resolve;
      }),
    window: clock.window,
  });

  gate.request();
  await settle();
  assert.equal(runs, 1);
  // Three changes while the first refresh runs make one more.
  gate.request();
  gate.request();
  gate.request();
  finish();
  await settle();
  assert.equal(runs, 1);
  assert.equal(clock.pending(), 1);
  clock.advance(999);
  await settle();
  assert.equal(runs, 1);
  clock.advance(1);
  await settle();
  assert.equal(runs, 2);
  finish();
  await settle();

  // An urgent request, as when a session opens, skips the wait.
  clock.advance(100);
  gate.request();
  assert.equal(clock.pending(), 1);
  gate.request(true);
  await settle();
  assert.equal(runs, 3);
  assert.equal(clock.pending(), 0);
  finish();
  await settle();
});

test("a hidden page fetches nothing until it shows again", async () => {
  const document = createDocument(INDEX_HTML);
  const clock = manualTimers();
  let runs = 0;
  const gate = createRefreshGate({
    document: document,
    now: clock.now,
    run: async () => {
      runs += 1;
    },
    window: clock.window,
  });

  document.visibilityState = "hidden";
  gate.request();
  gate.request(true);
  await settle();
  assert.equal(runs, 0);
  document.dispatchEvent(new FakeEvent("visibilitychange"));
  await settle();
  assert.equal(runs, 0);

  document.visibilityState = "visible";
  document.dispatchEvent(new FakeEvent("visibilitychange"));
  await settle();
  assert.equal(runs, 1);
  // Showing again with nothing pending fetches nothing.
  clock.advance(5000);
  document.dispatchEvent(new FakeEvent("visibilitychange"));
  await settle();
  assert.equal(runs, 1);
});

// A pasted file of `type` with `bytes`.
function imageFile(bytes, type = "image/png") {
  return new File([Uint8Array.from(bytes)], "pasted", { type: type });
}

// Pastes `files`, and `text` with them, into the composer and returns the
// paste event.
function pasteFiles(byId, files, text = "") {
  const clipboardData = { files: files, getData: (type) => (type === "text/plain" ? text : "") };
  const event = new FakeEvent("paste", { clipboardData: clipboardData });
  byId("message").dispatchEvent(event);

  return event;
}

function imageChips(byId) {
  return Array.from(byId("composer-images").children, (chip) => [
    chip.querySelector(".composer-image-label").textContent,
    chip.querySelector(".composer-image-size").textContent,
  ]);
}

test("pasted images go with the message as renumbered placeholders and base64 files", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["images"] });
  chat.render(session());
  typeMessage(byId, "Compare  with");
  byId("message").setSelectionRange(8, 8);

  const pasted = pasteFiles(byId, [imageFile([1, 2, 3]), imageFile([4, 5], "image/jpeg")]);
  assert.equal(pasted.defaultPrevented, true);
  assert.equal(byId("message").value, "Compare [Image #1] [Image #2] with");
  assert.equal(byId("message").selectionStart, 29);
  assert.equal(byId("composer-images").hidden, false);
  assert.deepEqual(imageChips(byId), [
    ["Image #1", "1 KB"],
    ["Image #2", "1 KB"],
  ]);
  // Removing the first image takes its placeholder out; the second keeps
  // its number until it goes out.
  byId("composer-images").children[0].querySelector("button").click();
  assert.equal(byId("message").value, "Compare [Image #2] with");
  assert.deepEqual(imageChips(byId), [["Image #2", "1 KB"]]);
  pressKey(byId, "Enter");
  await settle();

  assert.deepEqual(requests, [
    {
      images: [{ data: "BAU=" }],
      message: "Compare [Image #1] with",
      method: "send_message",
      session_id: "s1",
    },
  ]);
  assert.equal(byId("message").value, "");
  assert.equal(byId("composer-images").hidden, true);
});

test("removing an image while an earlier prompt shows takes it out of the draft only", () => {
  const storage = new MemoryStorage();
  const { byId, chat } = mountChat({ capabilities: ["images"], sessionStorage: storage });
  chat.render(session({ messages: HISTORY_MESSAGES }));
  typeMessage(byId, "See ");
  pasteFiles(byId, [imageFile([1]), imageFile([2])]);
  assert.equal(pressArrow(byId, "ArrowUp"), true);

  byId("composer-images").children[0].querySelector("button").click();

  assert.equal(byId("message").value, "Add tests\nand docs");
  assert.equal(storage.getItem(DRAFT_KEY_PREFIX + "s1"), "See [Image #2]");
  assert.deepEqual(imageChips(byId), [["Image #2", "1 KB"]]);
  assert.equal(pressArrow(byId, "ArrowDown"), true);
  assert.equal(byId("message").value, "See [Image #2]");
});

test("an image goes out without text, and a refused message keeps its images", async () => {
  let isRefusing = true;
  const { byId, chat, requests } = mountChat({
    capabilities: ["images"],
    respond: () => {
      if (isRefusing) {
        throw new Error("Session is busy");
      }

      return { kind: "accepted" };
    },
  });
  chat.render(session());
  pasteFiles(byId, [imageFile([1])]);
  typeMessage(byId, "");

  pressKey(byId, "Enter");
  await settle();
  assert.equal(byId("session-error").textContent, "Session is busy");
  assert.deepEqual(imageChips(byId), [["Image #1", "1 KB"]]);
  isRefusing = false;
  pressKey(byId, "Enter");
  await settle();

  assert.deepEqual(
    requests.map((body) => [body.message, body.images]),
    [
      ["", [{ data: "AQ==" }]],
      ["", [{ data: "AQ==" }]],
    ],
  );
  assert.deepEqual(imageChips(byId), []);
});

test("an image pasted while a message goes out keeps the only placeholder left", async () => {
  let release;
  const { byId, chat, requests } = mountChat({
    capabilities: ["images"],
    respond: () =>
      new Promise((resolve) => {
        release = resolve;
      }),
  });
  chat.render(session());
  typeMessage(byId, "Look ");
  pasteFiles(byId, [imageFile([1])]);
  pressKey(byId, "Enter");
  await settle();
  // The next image is pasted at the end while the first is on its way.
  byId("message").setSelectionRange(byId("message").value.length, byId("message").value.length);
  pasteFiles(byId, [imageFile([2])]);
  assert.equal(byId("message").value, "Look [Image #1][Image #2]");
  release({ kind: "accepted" });
  await settle();

  assert.equal(byId("message").value, "Look [Image #2]");
  assert.deepEqual(imageChips(byId), [["Image #2", "1 KB"]]);
  pressKey(byId, "Enter");
  await settle();
  release({ kind: "accepted" });
  await settle();
  assert.deepEqual(
    requests.map((body) => [body.message, body.images]),
    [
      ["Look [Image #1]", [{ data: "AQ==" }]],
      ["Look [Image #1]", [{ data: "Ag==" }]],
    ],
  );
  assert.equal(byId("message").value, "");
});

test("pastes the host cannot take, or without images, are left alone or refused", () => {
  const plain = mountChat();
  plain.chat.render(session());
  assert.equal(pasteFiles(plain.byId, [imageFile([1])]).defaultPrevented, false);
  assert.deepEqual(imageChips(plain.byId), []);

  const { byId, chat } = mountChat({ capabilities: ["images"] });
  chat.render(session());
  // Text pastes as text.
  assert.equal(pasteFiles(byId, [new File(["notes"], "notes.txt", { type: "text/plain" })]).defaultPrevented, false);
  // Text copied with an image of it, as from a spreadsheet, pastes as text.
  assert.equal(pasteFiles(byId, [imageFile([1])], "A1\tB1").defaultPrevented, false);
  assert.equal(pasteFiles(byId, [imageFile([1], "image/tiff")], "A1\tB1").defaultPrevented, false);
  assert.equal(byId("session-error").textContent, "");
  const gif = pasteFiles(byId, [imageFile([1], "image/gif")]);
  assert.equal(gif.defaultPrevented, true);
  assert.equal(byId("session-error").textContent, "Only PNG and JPEG images can be attached");
  pasteFiles(byId, Array.from({ length: 9 }, () => imageFile([1])));
  assert.equal(byId("session-error").textContent, "A message carries at most 8 images");
  pasteFiles(byId, [imageFile(new Uint8Array(6 * 1024 * 1024 + 1))]);
  assert.equal(byId("session-error").textContent, "A message's images are at most 6 MB together");
  assert.deepEqual(imageChips(byId), []);
  assert.equal(byId("message").value, "");
});

test("images stay with their draft across sessions, and a graph request refuses them", async () => {
  const { byId, chat, requests } = mountChat({ capabilities: ["images", "graph_proposal"] });
  chat.render(session());
  pasteFiles(byId, [imageFile([1])]);

  chat.open("s2");
  assert.deepEqual(imageChips(byId), []);
  chat.open("s1");
  assert.deepEqual(imageChips(byId), [["Image #1", "1 KB"]]);
  chat.render(session());
  typeMessage(byId, "/gr");
  pressKey(byId, "Enter");
  typeMessage(byId, "Split it");
  pressKey(byId, "Enter");
  await settle();

  assert.deepEqual(requests, []);
  assert.equal(byId("session-error").textContent, GRAPH_IMAGES_NOTE);
  assert.deepEqual(imageChips(byId), [["Image #1", "1 KB"]]);
});

test("a first message kept for a new session keeps its images", () => {
  const { byId, chat } = mountChat({ capabilities: ["images"] });
  const image = { file: imageFile([1, 2]), number: 1 };

  chat.open("s3", { draft: "Look at [Image #1]", error: "Session is busy", images: [image] });

  assert.equal(byId("message").value, "Look at [Image #1]");
  assert.deepEqual(imageChips(byId), [["Image #1", "1 KB"]]);
  chat.keepDraft("s4", "Other");
  chat.open("s4");
  assert.deepEqual(imageChips(byId), []);
});
