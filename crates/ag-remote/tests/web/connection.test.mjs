import assert from "node:assert/strict";
import test from "node:test";

import { MAX_BACKOFF_MS, createConnection } from "../../assets/connection.mjs";

// A transport the test drives: `open()`, `receive(frame)` and `drop()` act
// as the far side, and `sent` holds the frames sent.
function fakeTransports() {
  const opened = [];
  const openTransport = (handlers) => {
    const transport = {
      ...handlers,
      isClosed: false,
      sent: [],
      close: () => {
        transport.isClosed = true;
      },
      drop: () => handlers.onclose(),
      open: () => handlers.onopen(),
      receive: (frame) => handlers.onmessage(JSON.stringify(frame)),
      send: (text) => {
        if (transport.isClosed) {
          return false;
        }
        transport.sent.push(JSON.parse(text));

        return true;
      },
    };
    opened.push(transport);

    return transport;
  };

  return { openTransport, opened };
}

// A window whose timers the test runs by hand.
function fakeWindow() {
  const timers = [];

  return {
    clearTimeout: (id) => {
      if (id !== null && id !== undefined) {
        timers[id] = null;
      }
    },
    runTimers: () => {
      const due = timers.splice(0).filter(Boolean);
      for (const timer of due) {
        timer.run();
      }

      return due.map((timer) => timer.delay);
    },
    setTimeout: (run, delay) => timers.push({ delay, run }) - 1,
  };
}

function connect({ canReconnect = () => true } = {}) {
  const transports = fakeTransports();
  const window = fakeWindow();
  const events = [];
  const connection = createConnection({
    canReconnect: canReconnect,
    helloFrame: () => ({ type: "hello", protocol_version: 1 }),
    on: {
      authenticated: (frame) => events.push(["authenticated", frame.type]),
      awaiting: (confirmation) => events.push(["awaiting", confirmation]),
      changed: () => events.push(["changed"]),
      pairingLost: () => events.push(["pairingLost"]),
      rejected: (reason, wasPairing) => events.push(["rejected", reason, wasPairing]),
      state: (kind) => events.push(["state", kind]),
    },
    openTransport: transports.openTransport,
    window: window,
  });

  return { connection, events, transports, window };
}

test("a welcome authenticates with the capabilities, and requests resolve with their answers", async () => {
  // Arrange
  const { connection, events, transports } = connect();

  // Act
  connection.connect();
  const transport = transports.opened[0];
  transport.open();
  transport.receive({ type: "welcome", capabilities: ["merge"] });
  const answered = connection.request({ method: "list_sessions" });
  const failed = connection.request({ method: "get_session", session_id: "x" });
  transport.receive({ type: "response", id: 1, result: { ok: { kind: "sessions" } } });
  transport.receive({ type: "response", id: 2, result: { error: "Session not found" } });
  transport.receive({ type: "response", id: 9, result: { ok: {} } });
  transport.receive({ type: "changed", revision: 2 });
  transport.onmessage("not json");

  // Assert
  assert.deepEqual(transport.sent, [
    { type: "hello", protocol_version: 1 },
    { type: "request", id: 1, request: { method: "list_sessions" } },
    { type: "request", id: 2, request: { method: "get_session", session_id: "x" } },
  ]);
  assert.deepEqual(await answered, { kind: "sessions" });
  await assert.rejects(failed, /Session not found/);
  assert.deepEqual(connection.capabilities(), ["merge"]);
  assert.equal(connection.isAuthenticated(), true);
  assert.equal(connection.kind(), "connected");
  assert.deepEqual(events, [
    ["state", "connecting"],
    ["state", "connected"],
    ["authenticated", "welcome"],
    ["changed"],
  ]);
});

test("a request before authentication fails, and a dropped connection fails the ones in flight", async () => {
  // Arrange
  const { connection, transports } = connect();
  connection.connect();
  const early = connection.request({ method: "list_sessions" });
  transports.opened[0].open();
  transports.opened[0].receive({ type: "welcome" });
  const pending = connection.request({ method: "list_sessions" });

  // Act
  transports.opened[0].drop();

  // Assert
  await assert.rejects(early, /Not connected/);
  await assert.rejects(pending, /Disconnected/);
  assert.equal(connection.isAuthenticated(), false);
  assert.equal(connection.kind(), "reconnecting");
});

test("a dropped connection reconnects with waits that double up to the cap", () => {
  // Arrange
  const { connection, transports, window } = connect();
  connection.connect();

  // Act
  const delays = [];
  for (let attempt = 0; attempt < 7; attempt += 1) {
    transports.opened.at(-1).drop();
    delays.push(...window.runTimers());
  }
  transports.opened.at(-1).open();
  transports.opened.at(-1).receive({ type: "welcome" });
  transports.opened.at(-1).drop();
  const afterWelcome = window.runTimers();

  // Assert
  assert.deepEqual(delays, [1000, 2000, 4000, 8000, 16000, MAX_BACKOFF_MS, MAX_BACKOFF_MS]);
  assert.deepEqual(afterWelcome, [1000]);
  assert.deepEqual(transports.opened.at(-1).sent, []);
  assert.ok(connection.reconnectAt() > 0);
});

test("without a credential or after disconnect a dropped connection stays offline", () => {
  // Arrange
  const unpaired = connect({ canReconnect: () => false });
  const disconnected = connect();

  // Act
  unpaired.connection.connect();
  unpaired.transports.opened[0].drop();
  disconnected.connection.connect();
  const transport = disconnected.transports.opened[0];
  disconnected.connection.disconnect();
  transport.drop();

  // Assert
  assert.equal(unpaired.connection.kind(), "offline");
  assert.deepEqual(unpaired.window.runTimers(), []);
  assert.equal(transport.isClosed, true);
  assert.equal(disconnected.connection.kind(), "offline");
  assert.deepEqual(disconnected.window.runTimers(), []);
});

test("pairing reports the confirmation, then approval or the reason it was refused", () => {
  // Arrange
  const approved = connect();
  const refused = connect();
  const lost = connect({ canReconnect: () => false });
  const pair = { type: "pair", code: "ABCD", device_name: "Phone", protocol_version: 1 };

  // Act
  for (const { connection, transports } of [approved, refused, lost]) {
    connection.pair(pair);
    transports.opened[0].open();
    transports.opened[0].receive({ type: "awaiting_approval", confirmation: 1234 });
  }
  approved.transports.opened[0].receive({ type: "paired", token: "t" });
  refused.transports.opened[0].receive({ type: "rejected", reason: "Pairing was denied" });
  refused.transports.opened[0].drop();
  lost.transports.opened[0].drop();

  // Assert
  assert.deepEqual(approved.transports.opened[0].sent, [pair]);
  assert.deepEqual(approved.events.slice(1), [
    ["state", "waiting"],
    ["awaiting", "1234"],
    ["state", "connected"],
    ["authenticated", "paired"],
  ]);
  assert.equal(approved.connection.isPairing(), false);
  assert.deepEqual(refused.events.slice(-2), [
    ["state", "rejected"],
    ["rejected", "Pairing was denied", true],
  ]);
  // A rejection already said why the connection ended.
  assert.equal(refused.connection.kind(), "rejected");
  assert.deepEqual(refused.window.runTimers(), []);
  assert.deepEqual(lost.events.slice(-2), [["pairingLost"], ["state", "offline"]]);
});

test("a rejection of a paired device names no pairing and stops reconnecting", () => {
  // Arrange
  const { connection, events, transports, window } = connect();
  connection.connect();
  transports.opened[0].open();

  // Act
  transports.opened[0].receive({ type: "rejected" });
  transports.opened[0].drop();

  // Assert
  assert.deepEqual(events.at(-1), ["rejected", "Connection rejected", false]);
  assert.deepEqual(window.runTimers(), []);
});
