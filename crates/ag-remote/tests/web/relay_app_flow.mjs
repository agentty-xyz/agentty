// Runs the web client's app.js over the fake DOM in relay mode, against a
// real relay and real servers, for the integration tests in
// crates/ag-remote/tests/server/web_client.rs.
//
// Usage: node relay_app_flow.mjs multi <pairing link of A> <pairing link of B>
//        node relay_app_flow.mjs revoke <pairing link of A> <pairing link of B>
//        node relay_app_flow.mjs migrate <pairing link of A>
//        node relay_app_flow.mjs embed <pairing link of A> <pairing link of B> <page address>
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
// In `embed` mode the page is the one a web application serves at the
// page address: the script first loads its files as a browser would, then
// pastes A's link with a wrong code and then its real one; after
// `awaiting` the Rust test approves. B's link opens at the page's address
// without its relay; after `awaitingB` the Rust test signs the page out
// of the directory and approves B.
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

const [mode, firstLink, secondLink, embedderPage] = process.argv.slice(2);
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

// The fragment of `link` without its relay, as a link to a page that
// names its relay may leave it out.
function withoutRelay(link) {
  const parameters = new URLSearchParams(new URL(link).hash.slice(1));
  parameters.delete("relay");

  return "#" + parameters.toString();
}

// In `embed` mode the page is the one another web application serves, at
// its own address with no link in it.
async function startUrl() {
  if (mode === "embed") {
    return [await (await fetch(embedderPage)).text(), embedderPage];
  }

  return [PAGE, mode === "migrate" ? new URL(firstLink).origin + "/" : firstLink];
}

installFakeIndexedDb();
const [html, url] = await startUrl();
const document = createDocument(html);
const pageUrl = new URL(firstLink);
const window = createWindow(document, url);
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

// The device menu's items that forget this device on one Agentty each.
function forgetItems() {
  return byId("device-menu").querySelectorAll("[data-forget-on]");
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
    forgetItems: Array.from(forgetItems(), (item) => labelled(item.textContent)),
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
  forgetItems()[1].click();
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

// The texts of `selector`'s matches under `root`.
function texts(root, selector) {
  return Array.from(root.querySelectorAll(selector), (node) => node.textContent);
}

// Puts `text` in the pairing card's link field, as a paste does.
function paste(text) {
  byId("pair-link").value = text;
  byId("pair-link").dispatchEvent(new FakeEvent("input"));
}

// `link` with `parameter` set to `value` in its fragment.
function withParameter(link, parameter, value) {
  const url = new URL(link);
  const parameters = new URLSearchParams(url.hash.slice(1));
  parameters.set(parameter, value);
  url.hash = parameters.toString();

  return url.href;
}

// Fetches the page and every file it and its scripts name, as a browser
// would, and reports the policy's `connect-src` and what did not load
// with the page's headers.
async function servedFiles() {
  const page = await fetch(embedderPage);
  const policy = page.headers.get("content-security-policy") ?? "";
  const queue = [...(await page.text()).matchAll(/(?:src|href)="([a-z][^"/:]*\.(?:css|js|mjs))"/g)].map(
    (match) => match[1],
  );
  const fetched = new Set();
  const failures = [];
  while (queue.length > 0) {
    const path = queue.shift();
    if (fetched.has(path)) {
      continue;
    }
    fetched.add(path);
    const response = await fetch(new URL(path, embedderPage));
    const body = await response.text();
    if (!response.ok || response.headers.get("content-security-policy") !== policy) {
      failures.push(path);
    }
    queue.push(...Array.from(body.matchAll(/from "\.\/([^"]+)"/g), (match) => match[1]));
  }

  return {
    connect: policy.split("; ").find((directive) => directive.startsWith("connect-src")),
    failures: failures,
    fetched: fetched.size,
  };
}

// The page another web application serves on its own origin: the relay is
// another origin, the page takes pasted links, and the directory names the
// account's Agenttys. After `awaiting` and `awaitingB` the Rust test
// approves; after `pairedB` it signs the page out of the directory.
async function embedFlow() {
  report({ step: "served", ...(await servedFiles()) });

  // A link whose code does not work leaves the card ready for another.
  await waitFor("the directory on the pairing card", () => !byId("pair-directory").hidden);
  const firstCard = {
    hintHidden: byId("pair-hint-paste").hidden,
    linkHidden: byId("pair-link-field").hidden,
    submitHidden: byId("pair-submit").hidden,
  };
  paste(withParameter(firstLink, "code", "WRONG-CODE"));
  byId("pair-submit").click();
  await waitFor("the refusal", () => byId("pair-error").textContent !== "" && byId("pair-cancel").hidden);
  report({
    step: "wrongCode",
    card: firstCard,
    error: byId("pair-error").textContent,
    linkHidden: byId("pair-link-field").hidden,
  });

  paste(firstLink);
  // The code is filled in, so the device name comes next.
  const focused = document.activeElement?.id ?? null;
  await pairThroughPage();
  const card = byId("pair-view");
  report({
    step: "awaiting",
    focused: focused,
    badge: card.querySelector(".brand .badge").textContent,
    directory: texts(byId("pair-directory-list"), ".directory-name"),
    sections: texts(card, ".sections a"),
  });

  await waitFor("A's sessions", () => visibleView() === "home-view" && rows().length > 0);
  await waitFor("the sidebar's directory", () => byId("directory-list").children.length === 1);
  const sidebar = byId("sidebar");
  const account = sidebar.querySelector(".account");
  report({
    step: "paired",
    account: [account.getAttribute("href"), texts(account, "span")],
    badge: sidebar.querySelector(".brand .badge").textContent,
    directory: Array.from(byId("directory-list").children, (item) =>
      texts(item, "span").map((text) => text.replace(/\d+[smhd] ago$/, "N ago")),
    ),
    rows: rows(),
    sections: Array.from(sidebar.querySelectorAll(".sections a"), (link) => [
      link.textContent,
      link.getAttribute("href"),
      link.getAttribute("aria-current"),
    ]),
    stored: storedIds(),
  });

  // The device menu opens the pairing card, which takes a link pasted from
  // Agentty's Remote tab. Text that is not a link, and a link that names
  // another relay, are refused: this page pairs only through its own.
  byId("device-more").click();
  byId("pair-another").click();
  const pairCard = {
    hintHidden: byId("pair-hint-paste").hidden,
    linkHidden: byId("pair-link-field").hidden,
    view: visibleView(),
  };
  paste("ABCD-EFGH");
  const notLink = byId("pair-error").textContent;
  paste(withParameter(secondLink, "relay", "ws://127.0.0.1:9"));
  report({
    step: "otherRelay",
    card: pairCard,
    error: byId("pair-error").textContent,
    fieldsHidden: byId("pair-fields").hidden,
    linkHidden: byId("pair-link-field").hidden,
    notLink: notLink,
    submitHidden: byId("pair-submit").hidden,
  });

  // A link to this page itself may leave the relay out.
  window.location.hash = withoutRelay(secondLink);
  await pairThroughPage();
  report({ step: "awaitingB", linkHidden: byId("pair-link-field").hidden, pasted: byId("pair-link").value });

  await waitFor("both Agenttys", () => sections().length === 2 && heads().every((head) => head.state === "Connected"));
  await waitFor("the directory to empty", () => byId("directory").hidden);
  report({
    step: "pairedB",
    directoryHidden: byId("directory").hidden,
    forgetItems: Array.from(forgetItems(), (item) => item.textContent),
    heads: heads(),
  });

  // The page shows again a while later, and the directory says the page
  // is signed out; the Agenttys stay connected.
  const now = Date.now;
  Date.now = () => now() + 120000;
  document.dispatchEvent(new FakeEvent("visibilitychange"));
  await waitFor("the sign-out", () => byId("directory-error").textContent !== "");
  report({
    step: "signedOut",
    directoryHidden: byId("directory").hidden,
    error: byId("directory-error").textContent,
    heads: heads(),
    listed: byId("directory-list").children.length,
  });
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
} else if (mode === "embed") {
  await import(APP_URL.href);
  await embedFlow();
} else {
  await migrateFlow();
}

// The client keeps its sockets open, so the script ends itself once its
// output has been written.
await new Promise((resolve) => process.stdout.write("", resolve));
process.exit(0);
