// Runs the web client's app.js over the fake DOM against a real server for
// the integration tests in crates/ag-remote/tests/server/web_client.rs.
//
// Usage: node app_flow.mjs <listener address> session <device token>
//        node app_flow.mjs <listener address> route <device token>
//        node app_flow.mjs <listener address> create <device token>
//        node app_flow.mjs <listener address> pair <pairing code>
//
// In `session` mode the test server lists `Build it`, waiting on one
// question, and `Plan it`, running a turn, which counts as updated
// whenever a message is sent or a question answered. After `answered` and
// after `queued` the Rust test bumps the server's revision, so the server
// pushes `changed`: first `Plan it` turns unread, then its queued message
// shows. Answered, `Build it` waits in review on its graph work's
// question, and the page replies in chat with a `/graph` request instead.
//
// In `route` mode the page loads at `#s=session-2` with a draft left in
// this tab, as after a reload, sends it, and opens its graphs and its diff.
//
// In `create` mode the page starts at home, where the test server offers
// new sessions in its `agentty` project, and creates one with a message.
//
// In `pair` mode the page starts unpaired and pairs with the code. After
// `awaiting` the Rust test denies the request.
//
// The script prints one JSON line per step. Any wait that runs out ends
// the script with a failure.

import { readFileSync } from "node:fs";

import { FakeEvent, createDocument, createWindow } from "./fake_dom.mjs";

const APP_URL = new URL("../../assets/app.js", import.meta.url);
const PAGE = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const TOKEN_KEY = "agentty.remote.token";
const ROUTED_DRAFT_KEY = "agentty.remote.draft.session-2";
const WAIT_MS = 10000;

if (typeof WebSocket === "undefined") {
  throw new Error("Node.js 22 or newer is required: this one has no WebSocket");
}

const [address, mode, secret] = process.argv.slice(2);
const document = createDocument(PAGE);
const window = createWindow(
  document,
  "http://" + address + "/" + (mode === "route" ? "#s=session-2" : ""),
);
if (mode !== "pair") {
  window.localStorage.setItem(TOKEN_KEY, secret);
}
if (mode === "route") {
  window.sessionStorage.setItem(ROUTED_DRAFT_KEY, "Resume the plan");
}
globalThis.document = document;
globalThis.window = window;

function byId(id) {
  return document.getElementById(id);
}

// A part of the one Agentty's view, which sits above the filter.
function part(name) {
  return byId("instance-slot").querySelector('[data-part="' + name + '"]');
}

function report(step) {
  process.stdout.write(JSON.stringify(step) + "\n");
}

// Resolves once `condition` holds, checking after every macrotask.
async function waitFor(label, condition) {
  const deadline = Date.now() + WAIT_MS;
  while (!condition()) {
    if (Date.now() > deadline) {
      throw new Error("Timed out waiting for " + label);
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

// The pairing card, or the main area's view inside the app.
function visibleView() {
  if (!byId("pair-view").hidden) {
    return "pair-view";
  }

  return ["home-view", "session-view"].find((id) => !byId(id).hidden);
}

function texts(nodes) {
  return Array.from(nodes, (node) => node.textContent);
}

// The messages on screen. An answer reports its rendered Markdown, not
// its hidden source or the toggle beneath it.
function transcript() {
  return Array.from(byId("transcript").children, (item) => ({
    kind: item.dataset.kind,
    text: (item.querySelector(".md") ?? item).textContent,
  }));
}

function rows() {
  return Array.from(byId("sessions").querySelectorAll(".row"));
}

// Ids of the rows that show the unread dot.
function unreadRows() {
  return rows()
    .filter((row) => row.querySelector(".unread"))
    .map((row) => row.dataset.sessionId);
}

async function sessionFlow() {
  await waitFor("the session list", () => rows().length > 0);
  report({
    step: "listed",
    connection: byId("connection").textContent,
    groups: texts(byId("sessions").querySelectorAll(".group-label")),
    heading: byId("heading").textContent,
    isOnline: byId("connection").classList.contains("is-online"),
    sessions: rows().map((row) => ({
      status: row.querySelector(".status").textContent,
      time: row.querySelector(".time").textContent,
      title: row.querySelector(".row-title").textContent,
    })),
    unread: unreadRows(),
    view: visibleView(),
  });

  rows()[0].click();
  await waitFor("the transcript", () => byId("transcript").children.length > 0);
  report({
    step: "opened",
    answerCount: byId("answer-count").textContent,
    answerSendDisabled: byId("answer-send").disabled,
    cancelHidden: byId("cancel").hidden,
    composerHidden: byId("composer").hidden,
    current: rows()[0].getAttribute("aria-current"),
    hash: window.location.hash,
    heading: byId("heading").textContent,
    // The server lists `merge`, so Merge shows, disabled while a question
    // waits.
    mergeDisabled: byId("merge").disabled,
    mergeHidden: byId("merge").hidden,
    moreHidden: byId("session-more").hidden,
    questions: Array.from(byId("questions").querySelectorAll("[data-question]"), (group) => ({
      eyebrow: group.querySelector(".eyebrow").textContent,
      options: Array.from(group.querySelectorAll(".option"), (option) => option.firstChild.textContent),
      text: group.dataset.question,
    })),
    status: byId("session-status").textContent,
    transcript: transcript(),
    view: visibleView(),
  });

  const option = byId("questions").querySelector(".option");
  option.click();
  const chosen = {
    answerCount: byId("answer-count").textContent,
    answerSendDisabled: byId("answer-send").disabled,
    pressed: option.getAttribute("aria-pressed"),
  };
  byId("answer-send").click();
  // The button stays disabled while the request runs.
  const isSending = byId("answer-send").disabled;
  await waitFor("the answers to go through", () => !byId("answer-send").disabled);
  report({
    step: "answered",
    chosen: chosen,
    error: byId("answer-error").textContent,
    isSending: isSending,
  });

  await waitFor("the unread dot", () => unreadRows().length > 0);
  report({
    step: "refreshed",
    unread: unreadRows(),
    unreadLabel: texts(byId("sessions").querySelectorAll(".unread")),
  });

  // Cancel sits in the session sheet and asks first: keeping the session
  // sends nothing, confirming sends the request, which the test server
  // refuses.
  const sheet = byId("session-sheet");
  const dialog = byId("cancel-dialog");
  byId("session-more").click();
  const sheetOpened = sheet.open;
  byId("cancel").click();
  const askedFirst = dialog.open;
  dialog.querySelector('button[value="keep"]').click();
  byId("session-more").click();
  byId("cancel").click();
  dialog.querySelector('button[value="confirm"]').click();
  await waitFor("the cancel error", () => byId("action-error").textContent !== "");
  report({
    step: "canceled",
    askedFirst: askedFirst,
    error: byId("action-error").textContent,
    isDialogOpen: dialog.open,
    isSheetOpen: sheet.open,
    sheetOpened: sheetOpened,
  });

  // Answered, the session waits in review on its graph work's question,
  // which the question panel shows. Replying in chat instead opens the
  // composer, where `/graph` asks for a graph proposal.
  await waitFor("the graph question", () => byId("questions").querySelector(".questions-source"));
  const graphQuestion = {
    answerChatHidden: byId("answer-chat").hidden,
    composerHidden: byId("composer").hidden,
    source: byId("questions").querySelector(".questions-source").textContent,
    text: byId("questions").querySelector("[data-question]").dataset.question,
  };
  // Answers name the graph work that asked; the panel stays until the
  // host stops reporting the questions.
  byId("questions").querySelector(".option").click();
  byId("answer-send").click();
  await waitFor("the graph answers to go through", () => !byId("answer-send").disabled);
  graphQuestion.answerError = byId("answer-error").textContent;
  byId("answer-chat").click();
  const task = byId("message");
  task.value = "/gr";
  task.dispatchEvent(new FakeEvent("input"));
  const commands = Array.from(byId("message-suggestions").children, (option) => option.dataset.key);
  task.dispatchEvent(new FakeEvent("keydown", { key: "Enter" }));
  const armed = {
    markerHidden: byId("composer-graph").hidden,
    placeholder: task.placeholder,
  };
  task.value = "Split the parser";
  task.dispatchEvent(new FakeEvent("input"));
  task.dispatchEvent(new FakeEvent("keydown", { ctrlKey: true, key: "Enter" }));
  await waitFor("the graph request to go through", () => task.value === "");
  report({
    step: "graph",
    armed: armed,
    commands: commands,
    composerHidden: byId("composer").hidden,
    error: byId("session-error").textContent,
    markerHidden: byId("composer-graph").hidden,
    question: graphQuestion,
    questionsHidden: byId("questions").hidden,
  });

  // The back gesture returns to where the session was opened from.
  window.history.back();
  await waitFor("the home view", () => visibleView() === "home-view");
  report({
    step: "closed",
    current: rows()[0].getAttribute("aria-current"),
    hash: window.location.hash,
    heading: byId("heading").textContent,
    view: visibleView(),
  });

  // A running session queues the message for its next turn. This page has
  // no fine pointer, so Enter adds a line and Ctrl+Enter sends.
  rows()[1].click();
  await waitFor("the running transcript", () =>
    byId("transcript").querySelector('[data-kind="activity"]'),
  );
  const activity = byId("transcript").querySelector("details");
  report({
    step: "running",
    activity: {
      isOpen: activity.open,
      lines: Array.from(activity.querySelectorAll("li"), (line) => [line.className, line.textContent]),
      summary: activity.querySelector("summary").textContent,
    },
    answerBarHidden: byId("answer-bar").hidden,
    composerHidden: byId("composer").hidden,
    hint: byId("composer-hint").textContent,
    // The listing's figures reach the header and the row beside the
    // snapshot's settings and branches.
    header: [...byId("heading-sub").children]
      .filter((node) => !node.classList.contains("timer") && !node.classList.contains("started"))
      .map((node) => node.textContent),
    // The timer and the session's age follow the clock, so only their
    // form is compared.
    isStartedShown: /^Started \d/.test(byId("heading-sub").querySelector(".started")?.textContent ?? ""),
    isTimerShown: /^Timer: \d/.test(byId("heading-sub").querySelector(".timer")?.textContent ?? ""),
    // The server lists `settings` and `host`, so the line is a button.
    meta: byId("composer-settings").hidden ? null : byId("composer-settings").textContent,
    notice: {
      isError: byId("transcript").querySelector(".msg--notice").classList.contains("is-error"),
      label: byId("transcript").querySelector(".notice-label").textContent,
      text: byId("transcript").querySelector(".notice-body").textContent,
    },
    placeholder: byId("message").placeholder,
    sendLabel: byId("send").getAttribute("aria-label"),
    // Without the `stop_turn` capability the page offers no Stop.
    stopHidden: byId("stop").hidden,
    transcript: transcript(),
  });

  const message = byId("message");
  message.value = "Use the main branch";
  message.dispatchEvent(new FakeEvent("input"));
  const enter = new FakeEvent("keydown", { key: "Enter" });
  message.dispatchEvent(enter);
  const keptOnEnter = message.value;
  message.dispatchEvent(new FakeEvent("keydown", { ctrlKey: true, key: "Enter" }));
  await waitFor("the composer to clear", () => message.value === "");
  report({
    step: "queued",
    enterPrevented: enter.defaultPrevented,
    error: byId("session-error").textContent,
    keptOnEnter: keptOnEnter,
  });

  await waitFor("the queued message", () => byId("queued").children.length > 0);
  report({
    step: "shown",
    queued: texts(byId("queued").children),
    queuedBoxHidden: byId("queued-box").hidden,
    // The transcript was already on screen, so the refresh kept its nodes.
    keptActivity: byId("transcript").querySelector("details") === activity,
  });

  // Forgetting the device from an open session drops its route.
  rows()[0].click();
  const routed = window.location.hash;
  byId("device-more").click();
  byId("forget").click();
  byId("forget-dialog").querySelector('button[value="confirm"]').click();
  report({
    step: "forgot",
    hash: window.location.hash,
    isMainInert: byId("main").hasAttribute("inert"),
    routed: routed,
    token: window.localStorage.getItem(TOKEN_KEY),
    view: visibleView(),
  });
}

async function pairFlow() {
  await waitFor("the pairing card", () => visibleView() === "pair-view");
  byId("pair-code").value = secret;
  byId("pair-name").value = "Node browser";
  byId("pair-submit").click();
  // Before the connection opens, Cancel already replaces Pair.
  const connecting = {
    cancelHidden: byId("pair-cancel").hidden,
    label: byId("pair-wait-label").textContent,
    submitHidden: byId("pair-submit").hidden,
  };
  await waitFor("the confirmation code", () => byId("pair-confirmation").textContent !== "");
  report({
    step: "awaiting",
    cancelHidden: byId("pair-cancel").hidden,
    confirmation: byId("pair-confirmation").textContent,
    connecting: connecting,
    label: byId("pair-wait-label").textContent,
    connection: byId("connection").textContent,
    submitHidden: byId("pair-submit").hidden,
    view: visibleView(),
  });

  await waitFor("the denial", () => byId("pair-error").textContent !== "");
  report({
    step: "denied",
    cancelHidden: byId("pair-cancel").hidden,
    confirmHidden: byId("pair-confirm").hidden,
    connection: byId("connection").textContent,
    error: byId("pair-error").textContent,
    submitHidden: byId("pair-submit").hidden,
    view: visibleView(),
  });
}

// The session in the address opens with its draft, and the composer sends
// to it.
async function routeFlow() {
  await waitFor("the routed transcript", () => byId("transcript").children.length > 0);
  report({
    step: "restored",
    current: byId("sessions").querySelector('[aria-current="page"]')?.dataset.sessionId ?? null,
    hash: window.location.hash,
    heading: byId("heading").textContent,
    message: byId("message").value,
    view: visibleView(),
  });

  byId("send").click();
  await waitFor("the composer to clear", () => byId("message").value === "");
  report({
    step: "sent",
    draft: window.sessionStorage.getItem(ROUTED_DRAFT_KEY),
    error: byId("session-error").textContent,
  });

  // The graph summary above the composer opens the graphs in place of the
  // chat, and the back gesture returns to it.
  const summary = texts(byId("graph-summary").children);
  byId("graph-summary").click();
  await waitFor("the graph nodes", () => byId("graph-nodes").children.length > 0);
  const graphs = {
    chatHidden: byId("chat-scroll").hidden,
    nodes: texts(byId("graph-nodes").querySelectorAll(".graph-node-head")),
    output: byId("graph-detail").querySelector(".graph-output-label")?.textContent ?? null,
    position: byId("graph-position").textContent,
    title: byId("graph-title").textContent,
  };
  window.history.back();
  report({
    step: "graphs",
    ...graphs,
    hash: window.location.hash,
    returned: !byId("chat-scroll").hidden && byId("graph-view").hidden,
    summary: summary,
  });

  // The diff replaces the chat, and an opened file loads its patch.
  byId("diff").click();
  await waitFor("the changed files", () => byId("diff-files").children.length > 0);
  const file = byId("diff-files").querySelector("details");
  file.open = true;
  file.dispatchEvent(new FakeEvent("toggle"));
  await waitFor("the patch", () => file.querySelector(".patch"));
  report({
    step: "diff",
    chatHidden: byId("chat-scroll").hidden,
    files: texts(byId("diff-files").querySelectorAll("summary")),
    patch: Array.from(file.querySelectorAll(".patch-line"), (line) => [line.className, line.textContent]),
    summary: byId("diff-summary").textContent,
  });

  // The settings line opens the sheet, which lists the host's models and
  // sends the one change.
  byId("diff").click();
  byId("composer-settings").click();
  await waitFor("the models", () => byId("settings-model").getAttribute("aria-busy") === "false");
  const models = Array.from(byId("settings-model").children, (option) => option.textContent);
  byId("settings-reasoning").querySelector('[data-value="low"]').click();
  byId("settings-apply").click();
  await waitFor("the sheet to close", () => !byId("settings-dialog").open);
  report({
    step: "settings",
    error: byId("settings-error").textContent,
    models: models,
    noteHidden: byId("settings-note").hidden,
  });

  // Sync asks first, and the test host refuses it.
  byId("session-more").click();
  const syncLabel = byId("sheet-sync-label").textContent;
  byId("sheet-sync").click();
  const askedFirst = byId("sync-dialog").open;
  byId("sync-dialog").querySelector('button[value="confirm"]').click();
  await waitFor("the refusal", () => byId("action-error").textContent !== "");
  report({
    step: "sync",
    askedFirst: askedFirst,
    error: byId("action-error").textContent,
    label: syncLabel,
  });

  // The sidebar names the project and its branch. Its Sync starts at once,
  // and the next listing reports the sync.
  const branch = part("project-branch").textContent + " " + part("project-counts").textContent;
  part("project-sync").click();
  await waitFor("the sync to be accepted", () => part("project-sync-label").textContent === "Sync");
  report({
    step: "project",
    branch: branch,
    error: part("project-error").textContent,
  });
  await waitFor("the sync's progress", () => !part("project-status").hidden);
  report({
    step: "syncing",
    status: part("project-status").textContent,
    syncDisabled: part("project-sync").disabled,
  });
}

// Home describes the host's project, creates a session, sends it the
// first message, and opens it.
async function createFlow() {
  await waitFor("the home composer", () => !byId("home-form").hidden);
  const settingsButton = byId("home-composer-settings");
  report({
    step: "home",
    eyebrow: byId("home-eyebrow").textContent,
    hint: byId("home-composer-hint").textContent,
    placeholder: byId("home-message").placeholder,
    settings: (settingsButton.hidden ? byId("home-composer-meta") : settingsButton).textContent,
    settingsButton: !settingsButton.hidden,
    modesHidden: byId("home-modes").hidden,
    newSessionHidden: part("new-session").hidden,
    project: part("project-name").textContent,
    title: byId("home-title").textContent,
  });

  byId("home-message").value = "Add a relay image";
  byId("home-message").dispatchEvent(new FakeEvent("input"));
  byId("home-send").click();
  await waitFor("the new session", () => byId("transcript").children.length > 0);
  report({
    step: "created",
    error: byId("session-error").textContent,
    hash: window.location.hash,
    heading: byId("heading").textContent,
    homeMessage: byId("home-message").value,
    view: visibleView(),
  });
}

await import(APP_URL.href);
const flows = { create: createFlow, pair: pairFlow, route: routeFlow, session: sessionFlow };
await flows[mode]();

// The client keeps its socket open, so the script ends itself once its
// output has been written.
await new Promise((resolve) => process.stdout.write("", resolve));
process.exit(0);
