import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import {
  DRAFT_KEY_PREFIX,
  clockSkew,
  diffKey,
  collectAnswers,
  composerKeyAction,
  composerRows,
  composerState,
  createChat,
  createRefreshGate,
  mergeTarget,
  messageKind,
  publishBranch,
  sessionActions,
  transcriptChanges,
  transcriptEntries,
} from "../../assets/chat.mjs";
import { createDiff } from "../../assets/diff.mjs";
import { createSettings } from "../../assets/settings.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

// Element names `createChat` takes, by the id each has in index.html.
const ELEMENT_IDS = {
  actionError: "action-error",
  actionNote: "action-note",
  answerBar: "answer-bar",
  answerCount: "answer-count",
  answerError: "answer-error",
  answerSend: "answer-send",
  badge: "session-status",
  cancel: "cancel",
  cancelDialog: "cancel-dialog",
  composer: "composer",
  composerHint: "composer-hint",
  composerMeta: "composer-meta",
  composerSettings: "composer-settings",
  diff: "diff",
  dock: "dock",
  gone: "session-gone",
  goneHome: "session-gone-home",
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
  sheetMerge: "sheet-merge",
  sheetMergeLabel: "sheet-merge-label",
  sheetPublish: "sheet-publish",
  sheetPublishNote: "sheet-publish-note",
  sheetSub: "sheet-sub",
  sheetSync: "sheet-sync",
  sheetSyncLabel: "sheet-sync-label",
  sheetTitle: "sheet-title",
  stop: "stop",
  sub: "heading-sub",
  syncDialog: "sync-dialog",
  syncTarget: "sync-target",
  transcript: "transcript",
};

// Element names `createSettings` takes, by the id each has in index.html.
const SETTINGS_IDS = {
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
// request; the requests sent are recorded.
function mountChat({
  capabilities = [],
  isFinePointer = true,
  now = () => 0,
  respond = () => ({ kind: "accepted" }),
  sessionStorage = new MemoryStorage(),
} = {}) {
  const window = createWindow(createDocument(INDEX_HTML), "http://127.0.0.1/", {
    matches: (query) => query === "(pointer: fine)" && isFinePointer,
  });
  window.sessionStorage = sessionStorage;
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const elements = Object.fromEntries(
    Object.entries(ELEMENT_IDS).map(([name, id]) => [name, byId(id)]),
  );
  const requests = [];
  let homes = 0;
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
    now: now,
    onHome: () => {
      homes += 1;
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

  return { byId, chat, document, homes: () => homes, requests, window };
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

test("header actions appear only with their capability and status", () => {
  const none = { diff: false, diffEnabled: false, publish: false, publishEnabled: false, sync: false };
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
  });
  assert.deepEqual(sessionActions("Done", ["merge"]), {
    ...none,
    cancel: false,
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
  assert.equal(sessionActions("InProgress", ["publish"]).publish, true);
  assert.equal(sessionActions("InProgress", ["publish"]).publishEnabled, false);
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
  capable.chat.render(session({ status: "InProgress" }));
  assert.equal(capable.byId("publish").hidden, false);
  assert.equal(capable.byId("publish").disabled, true);
  assert.equal(capable.byId("sheet-publish").disabled, true);
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

test("a failed publish shows the host's error under the header, also after coming back", async () => {
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
  chat.open("s2");
  assert.equal(byId("action-error").textContent, "");
  chat.open("s1");
  assert.equal(byId("action-error").textContent, "Forge authentication failed");
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
  assert.equal(byId("session-status").className, "status status-badge wide-only status--working");
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
  assert.equal(sub.children[3].className, "wide-only");
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
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer 10s");
  time = 105;
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer").textContent, "Timer 15s");

  assert.equal(clockSkew({ updated_at: 50 }, 100), 0);
  assert.equal(clockSkew({ updated_at: 150 }, 100), 50);
  assert.equal(clockSkew(null, 100), 0);
});

test("the header's timer counts while the session runs and goes when it stops", () => {
  let time = 1000;
  const { byId, chat } = mountChat({ now: () => time });
  const running = detailedSummary({
    in_progress_started_at: 900,
    in_progress_total_seconds: 152,
    status: "InProgress",
  });
  chat.render(detailedSession({ status: "InProgress" }), running);
  const timer = byId("heading-sub").querySelector(".timer");
  assert.equal(timer.textContent, "Timer 4m 12s");

  time = 1001;
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer"), timer);
  assert.equal(timer.textContent, "Timer 4m 13s");

  chat.render(detailedSession(), detailedSummary({ in_progress_total_seconds: 253 }));
  assert.equal(byId("heading-sub").querySelector(".timer"), null);
  chat.tick();
  assert.equal(byId("heading-sub").querySelector(".timer"), null);

  // Ticking with no session open does nothing.
  chat.close();
  chat.tick();
  assert.equal(byId("heading-sub").textContent, "");
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
