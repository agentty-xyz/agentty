// Runs the web client's app.js over the fake DOM against a real server for
// the integration tests in crates/ag-remote/tests/server/web_client.rs.
//
// Usage: node app_flow.mjs <listener address> <device token>
//
// The test server lists one session, `Build it`, waiting on one question.
// The script prints one JSON line per step. Between `sent` and `refreshed`
// the Rust test bumps the server's revision, so the server pushes
// `changed`. Any wait that runs out ends the script with a failure.

import { readFileSync } from "node:fs";

import { createDocument, createWindow } from "./fake_dom.mjs";

const APP_URL = new URL("../../assets/app.js", import.meta.url);
const PAGE = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const TOKEN_KEY = "agentty.remote.token";
const WAIT_MS = 10000;

if (typeof WebSocket === "undefined") {
  throw new Error("Node.js 22 or newer is required: this one has no WebSocket");
}

const [address, token] = process.argv.slice(2);
const document = createDocument(PAGE);
const window = createWindow(document, "http://" + address + "/");
window.localStorage.setItem(TOKEN_KEY, token);
globalThis.document = document;
globalThis.window = window;

function byId(id) {
  return document.getElementById(id);
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

function visibleView() {
  return ["pair-view", "list-view", "session-view"].find((id) => !byId(id).hidden);
}

function texts(nodes) {
  return nodes.map((node) => node.textContent);
}

await import(APP_URL.href);

await waitFor("the session list", () => byId("sessions").children.length > 0);
report({
  step: "listed",
  connection: byId("connection").textContent,
  isOnline: byId("connection").classList.contains("online"),
  sessions: byId("sessions").children.map((item) => ({
    detail: item.querySelector(".detail").textContent,
    title: item.querySelector(".title").textContent,
  })),
  view: visibleView(),
});

byId("sessions").querySelector("button").click();
await waitFor("the transcript", () => byId("transcript").children.length > 0);
report({
  step: "opened",
  cancelHidden: byId("cancel").hidden,
  heading: byId("heading").textContent,
  questions: byId("questions")
    .querySelectorAll("[data-question]")
    .map((group) => ({
      options: texts(group.querySelectorAll("button")),
      text: group.dataset.question,
    })),
  status: byId("session-status").textContent,
  transcript: byId("transcript").children.map((item) => ({
    kind: item.className,
    text: item.textContent,
  })),
  view: visibleView(),
});

byId("message").value = "Use the main branch";
byId("composer").querySelector("button").click();
await waitFor("the composer to clear", () => byId("message").value === "");
report({ step: "sent", error: byId("session-error").textContent });

await waitFor("the queued message", () => byId("queued").children.length > 0);
report({ step: "refreshed", queued: texts(byId("queued").children) });

const option = byId("questions").querySelector(".options button");
option.click();
const chosen = option.getAttribute("aria-pressed");
byId("questions").querySelector('button[type="submit"]').click();
// Cancel asks first: keeping the session sends nothing, confirming sends
// the request, which the test server refuses.
const dialog = byId("cancel-dialog");
byId("cancel").click();
const askedFirst = dialog.open;
dialog.querySelector('button[value="keep"]').click();
byId("cancel").click();
dialog.querySelector('button[value="confirm"]').click();
await waitFor("the cancel error", () => byId("session-error").textContent !== "");
report({
  step: "canceled",
  askedFirst: askedFirst,
  chosen: chosen,
  error: byId("session-error").textContent,
  isDialogOpen: dialog.open,
});

byId("back").click();
await waitFor("the list view", () => visibleView() === "list-view");
report({ step: "closed", heading: byId("heading").textContent, view: visibleView() });

// The client keeps its socket open, so the script ends itself once its
// output has been written.
await new Promise((resolve) => process.stdout.write("", resolve));
process.exit(0);
