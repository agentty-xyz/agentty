// Runs the web client's app.js over the fake DOM in relay mode, against a
// real relay and real servers, for the integration tests in
// crates/ag-remote/tests/server/web_client.rs.
//
// Usage: node relay_app_flow.mjs multi <pairing link of A> <pairing link of B>
//        node relay_app_flow.mjs revoke <pairing link of A> <pairing link of B>
//        node relay_app_flow.mjs migrate <pairing link of A>
//
// In `multi` mode the page opens the link of Agentty A and pairs; after
// `awaiting` the Rust test approves. Then the link of Agentty B opens in
// the same tab, adding B beside A; after `awaitingB` the Rust test
// approves again. The page opens and messages B's sessions, both test
// servers listing the same session ids, then the Rust test stops B after
// `returned`, and the page opens A's session while B is offline and
// finally forgets this device on B.
//
// In `revoke` mode the page pairs A, then opens B's link; after
// `awaitingB` the Rust test revokes this device on A, then approves B
// after `revokedA`. B's pairing card stays as it was meanwhile.
//
// In `migrate` mode the script pairs a device key with A through the
// relay modules and saves the pairing as an earlier version of the page
// did, then loads the page, which moves the pairing over and connects.
// The Rust test approves after `keyAwaiting`.
//
// The script prints one JSON line per step. Any wait that runs out ends
// the script with a failure.

import { readFileSync } from "node:fs";

import { FakeEvent, createDocument, createWindow } from "./fake_dom.mjs";
import { installFakeIndexedDb } from "./fake_indexed_db.mjs";
import { loadDeviceKey } from "../../assets/device_key.mjs";
import { instanceIdOf } from "../../assets/pairings.mjs";
import { openRelayTransport, parsePairingLink } from "../../assets/relay.mjs";

const APP_URL = new URL("../../assets/app.js", import.meta.url);
const PAGE = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8").replace(
  '<meta name="agentty-transport" content="local">',
  '<meta name="agentty-transport" content="relay">',
);
const PAIRINGS_KEY = "agentty.remote.pairings";
const LEGACY_PAIRING_KEY = "agentty.remote.relay";
const WAIT_MS = 10000;

if (typeof WebSocket === "undefined") {
  throw new Error("Node.js 22 or newer is required: this one has no WebSocket");
}

const [mode, firstLink, secondLink] = process.argv.slice(2);
// Instance ids are random, so steps name the Agenttys `A` and `B`.
const labels = new Map();
for (const [link, label] of [
  [firstLink, "A"],
  [secondLink, "B"],
]) {
  if (link) {
    labels.set(await instanceIdOf(parsePairingLink(new URL(link).hash).instanceKey), label);
  }
}
installFakeIndexedDb();
const document = createDocument(PAGE);
const pageUrl = new URL(firstLink);
const window = createWindow(document, mode === "migrate" ? pageUrl.origin + "/" : firstLink);
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
  if (!byId("pair-view").hidden) {
    return "pair-view";
  }

  return ["home-view", "session-view"].find((id) => !byId(id).hidden);
}

// Each Agentty's section of the list, when the page shows several.
function sections() {
  return Array.from(byId("sessions").querySelectorAll(".instance"));
}

// `text` with every instance id, whole or as the eight characters a
// fallback name shows, replaced by its label.
function labelled(text) {
  let result = text;
  for (const [id, label] of labels) {
    result = result.replaceAll(id, label).replaceAll(id.slice(0, 8), label);
  }

  return result;
}

// The rows in `selector`'s matches, as `<label>/<session id>`.
function rows(container = byId("sessions"), selector = ".row") {
  return Array.from(container.querySelectorAll(selector), (row) =>
    labelled(row.dataset.instanceId + "/" + row.dataset.sessionId),
  );
}

function storedIds() {
  return Object.keys(JSON.parse(window.localStorage.getItem(PAIRINGS_KEY) ?? "{}")).map(labelled);
}

function heads() {
  return sections().map((section) => ({
    name: labelled(section.querySelector(".instance-name").textContent),
    state: section.querySelector(".instance-head .status").textContent,
  }));
}

// Pairs through the page with the link in the address and waits for the
// confirmation code.
async function pairThroughPage() {
  await waitFor("the pairing form", () => visibleView() === "pair-view" && !byId("pair-submit").hidden);
  const code = byId("pair-code").value;
  byId("pair-name").value = "Node browser";
  byId("pair-submit").click();
  await waitFor("the confirmation code", () => byId("pair-confirmation").textContent !== "");

  return code;
}

async function multiFlow() {
  const firstCode = await pairThroughPage();
  report({
    step: "awaiting",
    backHidden: byId("pair-back").hidden,
    code: firstCode,
    confirmation: byId("pair-confirmation").textContent,
  });

  await waitFor("A's sessions", () => visibleView() === "home-view" && rows().length > 0);
  report({
    step: "paired",
    connection: byId("connection").textContent,
    hash: window.location.hash,
    // One Agentty keeps the single page's layout: no headings, and its
    // project line above the filter.
    project: byId("instance-slot").querySelector('[data-part="project-name"]').textContent,
    rows: rows(),
    sections: sections().length,
    slotHidden: byId("instance-slot").hidden,
    stored: storedIds(),
  });

  // B's link opens in the same tab.
  window.location.hash = new URL(secondLink).hash;
  await waitFor("B's pairing form", () => visibleView() === "pair-view");
  const backHidden = byId("pair-back").hidden;
  const secondCode = await pairThroughPage();
  report({
    step: "awaitingB",
    backHidden: backHidden,
    code: secondCode,
    confirmation: byId("pair-confirmation").textContent,
  });

  await waitFor(
    "both Agenttys' sessions",
    () => sections().length === 2 && sections().every((section) => rows(section).length > 0),
  );
  await waitFor("both connected", () => byId("connection").textContent === "Connected · relay, encrypted");
  report({
    step: "pairedB",
    forgetHidden: byId("forget").hidden,
    forgetItems: Array.from(byId("forget-each").children, (item) => labelled(item.textContent)),
    heads: heads(),
    rows: sections().map((section) => rows(section)),
    slotHidden: byId("instance-slot").hidden,
    stored: storedIds(),
    view: visibleView(),
  });

  // B's sessions open with routes that name B, and requests reach B.
  const second = sections()[1];
  second.querySelector('[data-session-id="session-1"]').click();
  await waitFor(
    "B's first session",
    () => byId("transcript").children.length > 0 && byId("heading").textContent === "Build it",
  );
  const firstHash = labelled(window.location.hash);
  second.querySelector('[data-session-id="session-2"]').click();
  await waitFor("B's second session", () => byId("heading").textContent === "Plan it" && !byId("composer").hidden);
  const message = byId("message");
  message.value = "Sent to B";
  message.dispatchEvent(new FakeEvent("input"));
  message.dispatchEvent(new FakeEvent("keydown", { ctrlKey: true, key: "Enter" }));
  await waitFor("the composer to clear", () => message.value === "");
  report({
    step: "sentB",
    current: rows(byId("sessions"), '[aria-current="page"]'),
    error: byId("session-error").textContent,
    firstHash: firstHash,
    secondHash: labelled(window.location.hash),
  });

  // The back gesture returns to B's first session.
  window.history.back();
  await waitFor("B's first session again", () => byId("heading").textContent === "Build it");
  report({ step: "returned", hash: labelled(window.location.hash) });

  // The Rust test stops B. A stays connected and usable.
  await waitFor("B to drop", () => heads()[1].state !== "Connected");
  await waitFor("the banner", () => !byId("banner").hidden);
  const banner = labelled(byId("banner-text").textContent).replace(/\d+s$/, "Ns");
  const dropped = heads();
  sections()[0].querySelector('[data-session-id="session-2"]').click();
  await waitFor("A's session", () => byId("heading").textContent === "Plan it" && byId("banner").hidden);
  report({
    step: "bOffline",
    banner: banner,
    connection: byId("connection").textContent,
    hash: labelled(window.location.hash),
    heads: dropped,
    rowsA: rows(sections()[0]),
  });

  // Forgetting this device on B leaves A alone, in the single layout.
  byId("device-more").click();
  byId("forget-each").children[1].click();
  const asked = labelled(byId("forget-target").textContent);
  byId("forget-dialog").querySelector('button[value="confirm"]').click();
  await waitFor("B to go", () => sections().length === 0);
  report({
    step: "forgotB",
    asked: asked,
    connection: byId("connection").textContent,
    forgetHidden: byId("forget").hidden,
    hash: labelled(window.location.hash),
    rows: rows(),
    slotHidden: byId("instance-slot").hidden,
    stored: storedIds(),
    view: visibleView(),
  });
}

// Pairs A, then opens B's link. While B waits for approval the Rust test
// revokes this device on A, which leaves the pairing with B as it was;
// then it approves B.
async function revokeFlow() {
  await pairThroughPage();
  report({ step: "awaiting" });
  await waitFor("A's sessions", () => visibleView() === "home-view" && rows().length > 0);
  window.location.hash = new URL(secondLink).hash;
  await waitFor("B's pairing form", () => visibleView() === "pair-view");
  await pairThroughPage();
  report({ step: "awaitingB" });

  await waitFor("A to go", () => storedIds().length === 0);
  report({
    step: "revokedA",
    cancelHidden: byId("pair-cancel").hidden,
    confirmation: byId("pair-confirmation").textContent !== "",
    error: byId("pair-error").textContent,
    submitHidden: byId("pair-submit").hidden,
    view: visibleView(),
  });

  await waitFor("B's sessions", () => visibleView() === "home-view" && rows().length > 0);
  report({ step: "pairedB", rows: rows(), sections: sections().length, stored: storedIds() });
}

// Pairs a device key with A through the relay modules, as an earlier
// version of the page would have, and saves the pairing its way.
async function pairAsEarlierVersion() {
  const link = parsePairingLink(pageUrl.hash);
  const deviceKey = (await loadDeviceKey()).keyPair;
  await new Promise((resolve, reject) => {
    const transport = openRelayTransport({
      deviceKey: deviceKey,
      instanceKey: link.instanceKey,
      onclose: () => reject(new Error("Stream ended")),
      onmessage: (text) => {
        const frame = JSON.parse(text);
        if (frame.type === "awaiting_approval") {
          report({ step: "keyAwaiting" });
        } else if (frame.type === "paired") {
          transport.close();
          resolve();
        }
      },
      onopen: () =>
        transport.send(
          JSON.stringify({ type: "pair", code: link.code, device_name: "Node browser", protocol_version: 1 }),
        ),
      relayUrl: link.relayUrl,
    });
  });
  window.localStorage.setItem(LEGACY_PAIRING_KEY, link.instanceKey + " " + link.relayUrl);
}

async function migrateFlow() {
  await pairAsEarlierVersion();
  await import(APP_URL.href);
  await waitFor("A's sessions", () => visibleView() === "home-view" && rows().length > 0);
  report({
    step: "migrated",
    connection: byId("connection").textContent,
    legacy: window.localStorage.getItem(LEGACY_PAIRING_KEY),
    rows: rows(),
    sections: sections().length,
    stored: storedIds(),
  });
}

if (mode === "multi") {
  await import(APP_URL.href);
  await multiFlow();
} else if (mode === "revoke") {
  await import(APP_URL.href);
  await revokeFlow();
} else {
  await migrateFlow();
}

// The client keeps its sockets open, so the script ends itself once its
// output has been written.
await new Promise((resolve) => process.stdout.write("", resolve));
process.exit(0);
