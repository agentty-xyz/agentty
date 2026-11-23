// One connection of the remote control client to one Agentty: the first
// frame, requests and their answers, the frames Agentty pushes, and
// reconnecting with growing waits. It knows nothing of the page; the
// transport comes from `openTransport`, and what the frames mean for the
// page goes to the callbacks in `on`. The page holds one connection for
// each Agentty it shows, so one that drops or is refused leaves the others
// alone. The Node tests in crates/ag-remote/tests/web/connection.test.mjs
// pass a fake transport and window.

import { connectionLostError } from "./chat.mjs";

export const PROTOCOL_VERSION = 1;
// Longest wait between reconnect attempts.
export const MAX_BACKOFF_MS = 30000;
// Rejections that mean the stored credential can never work again.
export const CREDENTIAL_REJECTIONS = new Set(["Unknown device token", "Device revoked"]);

// Opens a connection that is offline until `connect` or `pair`.
//
// `openTransport({ onopen, onmessage, onclose })` opens one transport:
// `send(text)` reports whether the frame was accepted, and `close()` ends
// it without running `onclose`. `helloFrame()` returns the frame that
// resumes a pairing, and `canReconnect()` whether one is stored. `on`
// holds the callbacks, each optional:
//
// - `state(kind)`: the connection moved to `kind`, one of `offline`,
//   `connecting`, `waiting` (for approval), `connected`, `reconnecting`
//   or `rejected`.
// - `awaiting(confirmation)`: Agentty waits for approval of a pairing.
// - `authenticated(frame)`: a `paired` or `welcome` frame arrived.
// - `changed()`: Agentty reports a change.
// - `rejected(reason, wasPairing)`: Agentty refused the connection.
// - `pairingLost()`: the connection ended before a pairing was answered.
export function createConnection({ window, openTransport, helloFrame, canReconnect, on = {} }) {
  const state = {
    backoffMs: 1000,
    capabilities: [],
    isAuthenticated: false,
    isPairing: false,
    kind: "offline",
    nextRequestId: 1,
    pending: new Map(),
    // When the next reconnect attempt starts, in milliseconds since the
    // epoch.
    reconnectAt: 0,
    reconnectTimer: null,
    stopReconnecting: false,
    transport: null,
  };

  function setKind(kind) {
    state.kind = kind;
    on.state?.(kind);
  }

  // Ends the requests in flight when the connection drops. Their error
  // says so, as Agentty may still run them.
  function failPending(message) {
    for (const pending of state.pending.values()) {
      pending.reject(connectionLostError(message));
    }
    state.pending.clear();
  }

  function connect(firstFrame) {
    window.clearTimeout(state.reconnectTimer);
    state.transport?.close();
    failPending("Disconnected");
    state.stopReconnecting = false;
    state.isAuthenticated = false;
    setKind("connecting");
    const transport = openTransport({
      onclose: () => handleClose(transport),
      onmessage: handleFrame,
      onopen: () => transport.send(JSON.stringify(firstFrame)),
    });
    state.transport = transport;
  }

  function authenticated(frame) {
    state.capabilities = Array.isArray(frame.capabilities) ? frame.capabilities : [];
    state.isAuthenticated = true;
    state.isPairing = false;
    state.backoffMs = 1000;
    setKind("connected");
    on.authenticated?.(frame);
  }

  function handleFrame(data) {
    let frame;
    try {
      frame = JSON.parse(data);
    } catch (_error) {
      return;
    }

    switch (frame.type) {
      case "awaiting_approval":
        setKind("waiting");
        on.awaiting?.(String(frame.confirmation));
        break;
      // A relay device that pairs again with a key Agentty already knows
      // is welcomed directly.
      case "paired":
      case "welcome":
        authenticated(frame);
        break;
      case "response": {
        const pending = state.pending.get(frame.id);
        if (!pending) {
          break;
        }
        state.pending.delete(frame.id);
        if (frame.result && "ok" in frame.result) {
          pending.resolve(frame.result.ok);
        } else {
          pending.reject(new Error(frame.result?.error || "Request failed"));
        }
        break;
      }
      case "changed":
        on.changed?.();
        break;
      case "rejected": {
        const wasPairing = state.isPairing;
        state.isPairing = false;
        state.stopReconnecting = true;
        setKind("rejected");
        on.rejected?.(frame.reason || "Connection rejected", wasPairing);
        break;
      }
      default:
        break;
    }
  }

  function handleClose(transport) {
    if (state.transport !== transport) {
      return;
    }
    state.isAuthenticated = false;
    state.transport = null;
    failPending("Disconnected");
    if (state.isPairing) {
      state.isPairing = false;
      on.pairingLost?.();
    }
    if (state.stopReconnecting || !canReconnect()) {
      // A rejection already said why the connection ended.
      if (state.kind !== "rejected") {
        setKind("offline");
      }

      return;
    }
    const delay = state.backoffMs;
    state.backoffMs = Math.min(state.backoffMs * 2, MAX_BACKOFF_MS);
    state.reconnectAt = Date.now() + delay;
    state.reconnectTimer = window.setTimeout(() => connect(helloFrame()), delay);
    setKind("reconnecting");
  }

  return {
    capabilities: () => state.capabilities,
    // Connects with the stored credential.
    connect: () => connect(helloFrame()),
    // Ends the connection until the next `connect` or `pair`.
    disconnect: () => {
      window.clearTimeout(state.reconnectTimer);
      state.stopReconnecting = true;
      state.isPairing = false;
      state.transport?.close();
      state.transport = null;
      state.isAuthenticated = false;
      failPending("Disconnected");
      setKind("offline");
    },
    isAuthenticated: () => state.isAuthenticated,
    isPairing: () => state.isPairing,
    kind: () => state.kind,
    // Pairs with `frame`, a `pair` frame.
    pair: (frame) => {
      connect(frame);
      state.isPairing = true;
    },
    reconnectAt: () => state.reconnectAt,
    // Sends one request and resolves with its result.
    request: (body) =>
      new Promise((resolve, reject) => {
        const id = state.nextRequestId++;
        const text = JSON.stringify({ type: "request", id: id, request: body });
        if (!state.isAuthenticated || !state.transport?.send(text)) {
          reject(new Error("Not connected"));

          return;
        }
        state.pending.set(id, { reject: reject, resolve: resolve });
      }),
  };
}
