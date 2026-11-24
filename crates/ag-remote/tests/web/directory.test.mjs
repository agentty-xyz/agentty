import assert from "node:assert/strict";
import test from "node:test";

import { createDocument } from "./fake_dom.mjs";
import {
  directoryError,
  lastConnectedText,
  loadDirectory,
  parseDirectory,
  renderDirectory,
} from "../../assets/directory.mjs";
import { parsePairingLink } from "../../assets/relay.mjs";

const FIRST = "a".repeat(64);
const SECOND = "b".repeat(64);
const NOW = 1_800_000_000;

// A `fetch` that answers with `status` and `body`, and records its calls.
function fakeFetch(status, body) {
  const calls = [];
  const fetch = async (url, init) => {
    calls.push([String(url), init]);
    if (status instanceof Error) {
      throw status;
    }

    return {
      json: async () => (typeof body === "string" ? JSON.parse(body) : body),
      ok: status >= 200 && status < 300,
      status: status,
    };
  };

  return { calls, fetch };
}

test("parseDirectory keeps the entries it can read and the first of each Agentty", () => {
  // Arrange
  const body = {
    instances: [
      { instance_id: FIRST, last_connected_at: "2027-01-15T08:00:00Z", name: "  laptop  " },
      { instance_id: SECOND, last_connected_at: null, name: "x".repeat(100) },
      { instance_id: FIRST, last_connected_at: null, name: "again" },
      { instance_id: "A".repeat(64), name: "upper" },
      { instance_id: "c".repeat(63), name: "short" },
      { instance_id: "d".repeat(64), name: "   " },
      { instance_id: "e".repeat(64), last_connected_at: "yesterday", name: "server" },
      null,
    ],
  };

  // Act
  const entries = parseDirectory(body);

  // Assert
  assert.deepEqual(entries, [
    { instanceId: FIRST, lastConnectedAt: Date.parse("2027-01-15T08:00:00Z") / 1000, name: "laptop" },
    { instanceId: SECOND, lastConnectedAt: null, name: "x".repeat(80) },
    { instanceId: "e".repeat(64), lastConnectedAt: null, name: "server" },
  ]);
  assert.deepEqual(parseDirectory({ instances: "none" }), []);
  assert.deepEqual(parseDirectory(null), []);
});

test("loadDirectory reads with the page's credentials and tells a sign-out from a failure", async () => {
  // Arrange
  const listed = fakeFetch(200, { instances: [{ instance_id: FIRST, name: "laptop" }] });
  const signedOut = fakeFetch(401, {});
  const redirected = fakeFetch(302, {});
  const failing = fakeFetch(500, {});
  const unreachable = fakeFetch(new TypeError("Failed to fetch"));
  const notJson = fakeFetch(200, "<html>");

  // Act
  const results = [
    await loadDirectory(listed.fetch, "/api/instances"),
    await loadDirectory(signedOut.fetch, "/api/instances"),
    await loadDirectory(redirected.fetch, "/api/instances"),
    await loadDirectory(failing.fetch, "/api/instances"),
    await loadDirectory(unreachable.fetch, "/api/instances"),
    await loadDirectory(notJson.fetch, "/api/instances"),
  ];

  // Assert
  assert.deepEqual(results, [
    { entries: [{ instanceId: FIRST, lastConnectedAt: null, name: "laptop" }], status: "listed" },
    { status: "signed_out" },
    { status: "signed_out" },
    { status: "failed" },
    { status: "failed" },
    { status: "failed" },
  ]);
  assert.deepEqual(listed.calls, [
    [
      "/api/instances",
      {
        cache: "no-store",
        credentials: "same-origin",
        headers: { accept: "application/json" },
        redirect: "manual",
      },
    ],
  ]);
  assert.equal(directoryError("signed_out"), "You are signed out. Sign in again to see your Agenttys.");
  assert.equal(directoryError("failed"), "Could not list your Agenttys.");
  assert.equal(directoryError("listed"), "");
});

test("each unpaired Agentty shows its name as text, when it last connected, and how to pair it", () => {
  // Arrange
  const document = createDocument();
  const list = document.createElement("ul");
  const name = '<img src=x onerror="alert(1)">';

  // Act
  renderDirectory(
    document,
    list,
    [
      { instanceId: FIRST, lastConnectedAt: NOW - 3 * 3600, name: name },
      { instanceId: SECOND, lastConnectedAt: null, name: "server" },
    ],
    NOW,
  );

  // Assert
  assert.deepEqual(
    Array.from(list.children, (item) => [
      item.dataset.instanceId.slice(0, 1),
      Array.from(item.children, (part) => part.textContent),
    ]),
    [
      ["a", [name, "Last connected 3h ago", "To pair, start remote control in its Platform tab and open the pairing link."]],
      ["b", ["server", "Never connected", "To pair, start remote control in its Platform tab and open the pairing link."]],
    ],
  );
  assert.equal(list.querySelectorAll("img").length, 0);
  assert.equal(lastConnectedText(NOW - 5, NOW), "Connected just now");
});

test("a pairing link without a relay names the page's relay", () => {
  // Arrange
  const key = "ab".repeat(32);

  // Act
  const withDefault = parsePairingLink("#key=" + key + "&code=ABCD", "wss://relay.example");
  const withoutDefault = parsePairingLink("#key=" + key + "&code=ABCD");
  const named = parsePairingLink(
    "#relay=" + encodeURIComponent("wss://other.example") + "&key=" + key + "&code=ABCD",
    "wss://relay.example",
  );

  // Assert
  assert.equal(withDefault.relayUrl, "wss://relay.example");
  assert.equal(withoutDefault, null);
  assert.equal(named.relayUrl, "wss://other.example");
});
