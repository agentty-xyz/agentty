// Agentty remote control client. Speaks protocol version 1 over one
// WebSocket at /ws, or over an encrypted stream through the relay that
// serves this page; see crates/ag-remote/src/protocol.rs for the frames.

import { forgetDeviceKey, loadDeviceKey } from "./device_key.mjs";
import { isSupported } from "./noise.mjs";
import { openRelayTransport, parsePairingLink } from "./relay.mjs";

const PROTOCOL_VERSION = 1;
const TOKEN_KEY = "agentty.remote.token";
const RELAY_KEY = "agentty.remote.relay";
const MAX_BACKOFF_MS = 30000;
// Rejections that mean the stored credential can never work again.
const CREDENTIAL_REJECTIONS = new Set(["Unknown device token", "Device revoked"]);
// Statuses Agentty lets a session be canceled from. Canceling removes the
// whole session, so the button asks first.
const CANCELABLE_STATUSES = new Set([
  "InProgress", "Question", "Review", "AgentReview", "Watching", "Draft",
]);
// A relay serves this page with the marker set to "relay". The device then
// pairs with a link and reaches Agentty through that relay, encrypted end
// to end, and its key replaces the token.
const usesRelay =
  document.querySelector('meta[name="agentty-transport"]')?.content === "relay";

const elements = {};
for (const id of [
  "back", "heading", "connection", "pair-view", "pair-hint-local", "pair-hint-relay",
  "pair-form", "pair-code", "pair-name", "pair-status", "pair-error", "list-view",
  "sessions", "sessions-empty", "list-error",
  "forget", "session-view", "session-status", "cancel", "cancel-dialog", "transcript",
  "queued", "questions", "composer", "message", "session-error",
]) {
  elements[id] = document.getElementById(id);
}

const state = {
  backoffMs: 1000,
  // Optional requests the server supports; offer a feature only when listed.
  capabilities: [],
  currentSessionId: null,
  // This browser's key pair for relay streams.
  deviceKey: null,
  isAuthenticated: false,
  isRefreshing: false,
  // Whether the instance behind `relay` knows this browser's key.
  isRelayPaired: false,
  nextRequestId: 1,
  // Pairing link this page was opened with, until pairing succeeds.
  pairingLink: null,
  pendingPairing: null,
  pending: new Map(),
  reconnectTimer: null,
  refreshAgain: false,
  // Relay address and instance key that relay streams connect to.
  relay: null,
  session: null,
  stopReconnecting: false,
  transport: null,
};

function readStored(key) {
  try {
    return window.localStorage.getItem(key);
  } catch (_error) {
    return null;
  }
}

function writeStored(key, value) {
  try {
    if (value) {
      window.localStorage.setItem(key, value);
    } else {
      window.localStorage.removeItem(key);
    }
  } catch (_error) {
    // Storage can be unavailable in private windows; pairing then lasts
    // for this page only.
  }
}

let memoryToken = readStored(TOKEN_KEY);

function token() {
  return memoryToken;
}

function setToken(value) {
  memoryToken = value;
  writeStored(TOKEN_KEY, value);
}

// The relay and instance this browser last paired with, if any, saved as
// the instance key and the relay address with a space between them.
function storedRelay() {
  const [instanceKey, relayUrl] = (readStored(RELAY_KEY) || "").split(" ");

  return instanceKey && relayUrl ? { instanceKey: instanceKey, relayUrl: relayUrl } : null;
}

function hasCredential() {
  return usesRelay ? state.isRelayPaired : Boolean(token());
}

function forgetCredential() {
  if (usesRelay) {
    state.isRelayPaired = false;
    writeStored(RELAY_KEY, null);
  } else {
    setToken(null);
  }
}

// A relay device sends no token: its encrypted stream proves its key.
function helloFrame() {
  const frame = { type: "hello", protocol_version: PROTOCOL_VERSION };
  if (!usesRelay) {
    frame.token = token();
  }

  return frame;
}

function defaultDeviceName() {
  const platform =
    navigator.userAgentData?.platform || navigator.platform || "";
  const agent = navigator.userAgent || "";
  for (const [pattern, name] of [
    [/iPhone/i, "iPhone"],
    [/iPad/i, "iPad"],
    [/Android/i, "Android"],
    [/Mac/i, "Mac"],
    [/Win/i, "Windows"],
    [/Linux/i, "Linux"],
  ]) {
    if (pattern.test(agent) || pattern.test(platform)) {
      return name + " browser";
    }
  }

  return "Browser";
}

function socketUrl() {
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";

  return scheme + "//" + window.location.host + "/ws";
}

function setConnection(label, isOnline) {
  elements.connection.textContent = label;
  elements.connection.classList.toggle("online", isOnline);
}

// Shows the pairing view. Through a relay the code comes from the pairing
// link, so the form appears only once a link was opened.
function showPairing() {
  if (usesRelay) {
    const link = state.pairingLink;
    elements["pair-hint-local"].hidden = true;
    elements["pair-hint-relay"].hidden = Boolean(link);
    elements["pair-form"].hidden = !link;
    if (link) {
      elements["pair-code"].value = link.code;
    }
  }
  show("pair");
}

function show(view) {
  elements["pair-view"].hidden = view !== "pair";
  elements["list-view"].hidden = view !== "list";
  elements["session-view"].hidden = view !== "session";
  elements.back.hidden = view !== "session";
  if (view !== "session") {
    elements.heading.textContent = "Agentty";
  }
}

function send(frame) {
  return Boolean(state.transport?.send(JSON.stringify(frame)));
}

function request(body) {
  return new Promise((resolve, reject) => {
    const id = state.nextRequestId++;
    if (!state.isAuthenticated || !send({ type: "request", id: id, request: body })) {
      reject(new Error("Not connected"));

      return;
    }
    state.pending.set(id, { reject: reject, resolve: resolve });
  });
}

function failPending(message) {
  for (const pending of state.pending.values()) {
    pending.reject(new Error(message));
  }
  state.pending.clear();
}

// One WebSocket to Agentty's own listener, with the same shape as a relay
// transport: `send(text)` reports whether the frame was accepted, and
// `close()` ends it without running `onclose`.
function openLocalTransport({ onclose, onmessage, onopen }) {
  const socket = new WebSocket(socketUrl());
  socket.onopen = onopen;
  socket.onmessage = (event) => onmessage(event.data);
  socket.onclose = onclose;

  return {
    close: () => {
      socket.onclose = null;
      socket.close();
    },
    send: (text) => {
      if (socket.readyState !== WebSocket.OPEN) {
        return false;
      }
      socket.send(text);

      return true;
    },
  };
}

function connect(firstFrame) {
  window.clearTimeout(state.reconnectTimer);
  state.transport?.close();
  state.stopReconnecting = false;
  state.isAuthenticated = false;
  setConnection("Connecting", false);

  const handlers = {
    onclose: () => handleClose(transport),
    onmessage: handleFrame,
    onopen: () => transport.send(JSON.stringify(firstFrame)),
  };
  const transport = usesRelay
    ? openRelayTransport({
        ...handlers,
        deviceKey: state.deviceKey,
        instanceKey: state.relay.instanceKey,
        relayUrl: state.relay.relayUrl,
      })
    : openLocalTransport(handlers);
  state.transport = transport;
}

// Ends the connection until the next `connect`.
function disconnect() {
  window.clearTimeout(state.reconnectTimer);
  state.stopReconnecting = true;
  state.transport?.close();
  state.transport = null;
  state.isAuthenticated = false;
  failPending("Disconnected");
  setConnection("Offline", false);
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
      elements["pair-status"].textContent =
        "Waiting for approval in Agentty. Confirm the code matches: " + frame.confirmation;
      setConnection("Waiting for approval", false);
      break;
    case "paired":
      if (!usesRelay) {
        setToken(frame.token);
      }
      state.capabilities = Array.isArray(frame.capabilities) ? frame.capabilities : [];
      state.pendingPairing = null;
      elements["pair-status"].textContent = "";
      onAuthenticated();
      break;
    case "welcome":
      // A relay device that pairs again with a key Agentty already knows
      // is welcomed directly.
      state.capabilities = Array.isArray(frame.capabilities) ? frame.capabilities : [];
      state.pendingPairing = null;
      elements["pair-status"].textContent = "";
      onAuthenticated();
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
      refresh();
      break;
    case "rejected":
      onRejected(frame.reason || "Connection rejected");
      break;
    default:
      break;
  }
}

function onAuthenticated() {
  if (usesRelay) {
    state.isRelayPaired = true;
    state.pairingLink = null;
    writeStored(RELAY_KEY, state.relay.instanceKey + " " + state.relay.relayUrl);
  }
  state.isAuthenticated = true;
  state.backoffMs = 1000;
  setConnection("Connected", true);
  elements["list-error"].textContent = "";
  elements["pair-error"].textContent = "";
  if (!state.currentSessionId) {
    show("list");
  }
  refresh();
}

// Pairing failures, including a denial or a missed approval in the
// terminal, return to the pair form with the reason.
function onRejected(reason) {
  state.stopReconnecting = true;
  elements["pair-status"].textContent = "";
  if (state.pendingPairing || CREDENTIAL_REJECTIONS.has(reason)) {
    if (CREDENTIAL_REJECTIONS.has(reason)) {
      forgetCredential();
    }
    state.pendingPairing = null;
    state.currentSessionId = null;
    elements["pair-error"].textContent = reason;
    showPairing();
  } else {
    // Other rejections, such as an unsupported protocol version, last
    // until Agentty or the server of this page is updated.
    elements["list-error"].textContent = reason;
    elements["session-error"].textContent = reason;
  }
  setConnection("Disconnected", false);
}

function handleClose(transport) {
  if (state.transport !== transport) {
    return;
  }
  state.isAuthenticated = false;
  state.transport = null;
  elements["pair-status"].textContent = "";
  failPending("Disconnected");
  setConnection("Offline", false);
  if (state.pendingPairing) {
    // The connection ended before Agentty answered the pairing.
    state.pendingPairing = null;
    elements["pair-error"].textContent =
      "Could not reach Agentty. Check that remote control is on, then try again.";
  }
  if (state.stopReconnecting || !hasCredential()) {
    return;
  }
  const delay = state.backoffMs;
  state.backoffMs = Math.min(state.backoffMs * 2, MAX_BACKOFF_MS);
  setConnection("Reconnecting", false);
  state.reconnectTimer = window.setTimeout(() => {
    connect(helloFrame());
  }, delay);
}

// Reloads the list and the open session. Changes that arrive while a
// reload runs coalesce into one more reload after it.
function refresh() {
  if (state.isRefreshing) {
    state.refreshAgain = true;

    return;
  }
  state.isRefreshing = true;
  state.refreshAgain = false;
  void loadSessions()
    .catch((error) => {
      elements["session-error"].textContent = error.message;
    })
    .finally(() => {
      state.isRefreshing = false;
      if (state.refreshAgain) {
        refresh();
      }
    });
}

async function loadSessions() {
  const listed = await request({ method: "list_sessions" });
  renderSessions(listed.sessions || []);
  if (state.currentSessionId) {
    const loaded = await request({
      method: "get_session",
      session_id: state.currentSessionId,
    });
    renderSession(loaded.session);
  }
}

function relativeTime(unixSeconds) {
  const seconds = Math.max(0, Math.round(Date.now() / 1000 - unixSeconds));
  if (seconds < 60) {
    return "just now";
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return minutes + " min ago";
  }
  const hours = Math.round(minutes / 60);
  if (hours < 48) {
    return hours + " h ago";
  }

  return Math.round(hours / 24) + " d ago";
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }

  return node;
}

function renderSessions(sessions) {
  elements.sessions.replaceChildren(
    ...sessions.map((session) => {
      const item = element("li");
      const button = element("button");
      button.type = "button";
      button.append(
        element("span", "title", session.title || session.id),
        element("span", "detail", session.status + " · " + relativeTime(session.updated_at)),
      );
      button.addEventListener("click", () => openSession(session.id));
      item.append(button);

      return item;
    }),
  );
  elements["sessions-empty"].hidden = sessions.length > 0;
}

function openSession(sessionId) {
  state.currentSessionId = sessionId;
  state.session = null;
  elements.transcript.replaceChildren();
  elements.queued.replaceChildren();
  elements.questions.hidden = true;
  elements["session-error"].textContent = "";
  show("session");
  refresh();
}

function closeSession() {
  state.currentSessionId = null;
  state.session = null;
  show("list");
  refresh();
}

function renderSession(session) {
  if (!session) {
    elements["session-error"].textContent = "This session no longer exists.";
    state.session = null;

    return;
  }
  const isNewTranscript =
    !state.session ||
    state.session.id !== session.id ||
    state.session.messages.length !== session.messages.length;
  const questionsChanged =
    !state.session || JSON.stringify(state.session.questions) !== JSON.stringify(session.questions);
  state.session = session;
  elements.heading.textContent = session.title || session.id;
  elements["session-status"].textContent = session.status;
  elements.cancel.hidden = !CANCELABLE_STATUSES.has(session.status);

  if (isNewTranscript) {
    const nearBottom =
      window.innerHeight + window.scrollY >= document.body.scrollHeight - 80;
    elements.transcript.replaceChildren(
      ...session.messages.map((message) => element("li", message.kind, message.content)),
    );
    if (nearBottom) {
      window.scrollTo(0, document.body.scrollHeight);
    }
  }
  elements.queued.replaceChildren(
    ...session.queued_messages.map((message) => element("li", null, "Queued: " + message)),
  );
  if (questionsChanged) {
    renderQuestions(session.questions);
  }
}

function renderQuestions(questions) {
  const form = elements.questions;
  form.replaceChildren();
  form.hidden = questions.length === 0;
  if (questions.length === 0) {
    return;
  }

  questions.forEach((question, index) => {
    const group = element("div", "stack");
    group.dataset.question = question.text;
    group.append(element("p", "question", question.text));
    const options = element("div", "options");
    for (const option of question.options) {
      const button = element("button", null, option);
      button.type = "button";
      button.setAttribute("aria-pressed", "false");
      button.addEventListener("click", () => {
        for (const sibling of options.children) {
          sibling.setAttribute("aria-pressed", String(sibling === button));
        }
        group.dataset.answer = option;
      });
      options.append(button);
    }
    if (question.options.length > 0) {
      group.append(options);
    }
    const input = element("input");
    input.placeholder = question.options.length > 0 ? "Or type an answer" : "Your answer";
    input.setAttribute("aria-label", "Answer " + (index + 1));
    group.append(input);
    form.append(group);
  });

  const submit = element("button", "primary", "Answer");
  submit.type = "submit";
  form.append(submit);
}

elements["pair-name"].value = defaultDeviceName();

elements["pair-form"].addEventListener("submit", (event) => {
  event.preventDefault();
  const code = elements["pair-code"].value.trim();
  if (!code) {
    return;
  }
  if (usesRelay) {
    const link = state.pairingLink;
    if (!link || !state.deviceKey) {
      return;
    }
    // An earlier pairing stays saved until this one succeeds, but is not
    // used to reconnect to the instance this link names.
    state.isRelayPaired = false;
    state.relay = { instanceKey: link.instanceKey, relayUrl: link.relayUrl };
  }
  elements["pair-error"].textContent = "";
  elements["pair-status"].textContent = "";
  state.pendingPairing = code;
  connect({
    type: "pair",
    code: code,
    device_name: elements["pair-name"].value.trim(),
    protocol_version: PROTOCOL_VERSION,
  });
});

elements.forget.addEventListener("click", () => {
  forgetCredential();
  state.currentSessionId = null;
  disconnect();
  if (usesRelay) {
    // Agentty lists the old key until it is removed there; a later pairing
    // uses a new one.
    state.deviceKey = null;
    void forgetDeviceKey()
      .then(loadDeviceKey)
      .then((deviceKey) => {
        state.deviceKey = deviceKey.keyPair;
      });
  }
  showPairing();
});

elements.back.addEventListener("click", closeSession);

elements.cancel.addEventListener("click", () => {
  if (!state.currentSessionId) {
    return;
  }
  elements["cancel-dialog"].returnValue = "";
  elements["cancel-dialog"].showModal();
});

elements["cancel-dialog"].addEventListener("close", async () => {
  const sessionId = state.currentSessionId;
  if (elements["cancel-dialog"].returnValue !== "confirm" || !sessionId) {
    return;
  }
  elements.cancel.disabled = true;
  try {
    await request({ method: "cancel_session", session_id: sessionId });
    elements["session-error"].textContent = "";
  } catch (error) {
    elements["session-error"].textContent = error.message;
  } finally {
    elements.cancel.disabled = false;
  }
});

elements.composer.addEventListener("submit", async (event) => {
  event.preventDefault();
  const message = elements.message.value;
  if (!message.trim() || !state.currentSessionId) {
    return;
  }
  const submit = elements.composer.querySelector("button");
  submit.disabled = true;
  try {
    await request({
      method: "send_message",
      session_id: state.currentSessionId,
      message: message,
    });
    elements.message.value = "";
    elements["session-error"].textContent = "";
  } catch (error) {
    elements["session-error"].textContent = error.message;
  } finally {
    submit.disabled = false;
  }
});

elements.questions.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!state.currentSessionId) {
    return;
  }
  const answers = [];
  for (const group of elements.questions.querySelectorAll("[data-question]")) {
    const typed = group.querySelector("input").value.trim();
    const answer = typed || group.dataset.answer || "";
    if (!answer) {
      elements["session-error"].textContent = "Answer every question first.";

      return;
    }
    answers.push({ question: group.dataset.question, answer: answer });
  }
  try {
    await request({
      method: "answer_questions",
      session_id: state.currentSessionId,
      answers: answers,
    });
    elements["session-error"].textContent = "";
  } catch (error) {
    elements["session-error"].textContent = error.message;
  }
});

// Takes a pairing link from the address bar, if one is there, and removes
// it so the one-time code stays out of the address bar and the history.
function adoptPairingLink() {
  const link = parsePairingLink(window.location.hash);
  if (!link) {
    return false;
  }
  window.history.replaceState(null, "", window.location.pathname + window.location.search);
  state.pairingLink = link;

  return true;
}

async function startThroughRelay() {
  if (!(await isSupported())) {
    elements["pair-hint-local"].hidden = true;
    elements["pair-form"].hidden = true;
    elements["pair-error"].textContent =
      "This browser cannot pair through a relay. Open the link in an up-to-date browser, over HTTPS.";
    show("pair");

    return;
  }
  const deviceKey = await loadDeviceKey();
  state.deviceKey = deviceKey.keyPair;
  // A link opened in a tab that already shows this page only changes the
  // fragment.
  window.addEventListener("hashchange", () => {
    if (adoptPairingLink()) {
      // The link replaces whatever this tab was doing.
      disconnect();
      state.currentSessionId = null;
      state.session = null;
      elements["pair-error"].textContent = "";
      showPairing();
    }
  });
  const relay = storedRelay();
  if (adoptPairingLink() || !relay) {
    showPairing();

    return;
  }
  if (deviceKey.origin !== "saved") {
    // The key Agentty knows is not available, so connecting would only be
    // refused. A key that is gone ends the pairing; storage that could not
    // be read may work again after a reload, so the pairing stays.
    if (deviceKey.origin === "created") {
      forgetCredential();
    } else {
      elements["pair-error"].textContent =
        "This browser's saved key could not be read. Reload the page to try again.";
    }
    showPairing();

    return;
  }
  state.relay = relay;
  state.isRelayPaired = true;
  show("list");
  connect(helloFrame());
}

if (usesRelay) {
  await startThroughRelay();
} else if (token()) {
  show("list");
  connect(helloFrame());
} else {
  show("pair");
}
