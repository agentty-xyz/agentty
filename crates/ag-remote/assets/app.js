// Agentty remote control client. Speaks protocol version 1 over one
// WebSocket at /ws, or over an encrypted stream through the relay that
// serves this page; see crates/ag-remote/src/protocol.rs for the frames.

import { forgetDeviceKey, loadDeviceKey } from "./device_key.mjs";
import { relativeTime, statusInfo } from "./format.mjs";
import { isSupported } from "./noise.mjs";
import { openRelayTransport, parsePairingLink } from "./relay.mjs";
import { THEMES, applyTheme, createShell, loadTheme, parseRoute, saveTheme } from "./shell.mjs";

// The theme comes first, so a dark-theme device shows green for at most
// one frame. The content security policy forbids an inline script that
// could set it before the first paint.
applyTheme(document, loadTheme(window));

const PROTOCOL_VERSION = 1;
const TOKEN_KEY = "agentty.remote.token";
const RELAY_KEY = "agentty.remote.relay";
const DEVICE_NAME_KEY = "agentty.remote.device_name";
const MAX_BACKOFF_MS = 30000;
// Rejections that mean the stored credential can never work again.
const CREDENTIAL_REJECTIONS = new Set(["Unknown device token", "Device revoked"]);
// A relay serves this page with the marker set to "relay". The device then
// pairs with a link and reaches Agentty through that relay, encrypted end
// to end, and its key replaces the token.
const usesRelay =
  document.querySelector('meta[name="agentty-transport"]')?.content === "relay";
// What the device footer says in each connection state.
const CONNECTION_LABELS = {
  connected: usesRelay ? "Connected · relay, encrypted" : "Connected · local listener",
  connecting: "Connecting",
  offline: "Offline",
  reconnecting: "Reconnecting",
  rejected: "Disconnected",
  waiting: "Waiting for approval",
};

// Each id is a literal, so the contract test in
// crates/ag-remote/tests/server/http.rs can check it exists in index.html.
const elements = {
  announcer: document.getElementById("announcer"),
  "pair-view": document.getElementById("pair-view"),
  "pair-hint-local": document.getElementById("pair-hint-local"),
  "pair-hint-relay": document.getElementById("pair-hint-relay"),
  "pair-form": document.getElementById("pair-form"),
  "pair-fields": document.getElementById("pair-fields"),
  "pair-code": document.getElementById("pair-code"),
  "pair-name": document.getElementById("pair-name"),
  "pair-confirm": document.getElementById("pair-confirm"),
  "pair-confirmation": document.getElementById("pair-confirmation"),
  "pair-error": document.getElementById("pair-error"),
  "pair-submit": document.getElementById("pair-submit"),
  "pair-cancel": document.getElementById("pair-cancel"),
  "pair-foot": document.getElementById("pair-foot"),
  shell: document.getElementById("shell"),
  sidebar: document.getElementById("sidebar"),
  "sidebar-hide": document.getElementById("sidebar-hide"),
  "drawer-close": document.getElementById("drawer-close"),
  sessions: document.getElementById("sessions"),
  "sessions-empty": document.getElementById("sessions-empty"),
  "list-error": document.getElementById("list-error"),
  "device-name": document.getElementById("device-name"),
  connection: document.getElementById("connection"),
  "device-more": document.getElementById("device-more"),
  "device-menu": document.getElementById("device-menu"),
  forget: document.getElementById("forget"),
  "forget-dialog": document.getElementById("forget-dialog"),
  scrim: document.getElementById("scrim"),
  main: document.getElementById("main"),
  menu: document.getElementById("menu"),
  heading: document.getElementById("heading"),
  "heading-sub": document.getElementById("heading-sub"),
  "session-status": document.getElementById("session-status"),
  cancel: document.getElementById("cancel"),
  "cancel-dialog": document.getElementById("cancel-dialog"),
  banner: document.getElementById("banner"),
  "banner-text": document.getElementById("banner-text"),
  retry: document.getElementById("retry"),
  reload: document.getElementById("reload"),
  "home-view": document.getElementById("home-view"),
  "home-title": document.getElementById("home-title"),
  "home-pick": document.getElementById("home-pick"),
  "home-none": document.getElementById("home-none"),
  "home-sessions": document.getElementById("home-sessions"),
  "session-view": document.getElementById("session-view"),
  "chat-scroll": document.getElementById("chat-scroll"),
  transcript: document.getElementById("transcript"),
  queued: document.getElementById("queued"),
  questions: document.getElementById("questions"),
  composer: document.getElementById("composer"),
  message: document.getElementById("message"),
  send: document.getElementById("send"),
  "session-error": document.getElementById("session-error"),
};

const state = {
  backoffMs: 1000,
  // Optional requests the server supports; offer a feature only when listed.
  capabilities: [],
  // One of the keys of `CONNECTION_LABELS`.
  connection: "offline",
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
  // When the next reconnect attempt starts, in milliseconds since the epoch.
  reconnectAt: 0,
  reconnectTimer: null,
  refreshAgain: false,
  // Why Agentty refused this device for good, until the page reloads.
  rejection: null,
  // Relay address and instance key that relay streams connect to.
  relay: null,
  session: null,
  sessionCount: 0,
  stopReconnecting: false,
  // Ticks the banner's reconnect countdown.
  tickTimer: null,
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
  writeStored(DEVICE_NAME_KEY, null);
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

// Shows `kind`, one of the keys of `CONNECTION_LABELS`, in the device
// footer, announces a change once, and updates the banner and composer.
function setConnection(kind) {
  const label = CONNECTION_LABELS[kind];
  elements.connection.textContent = label;
  elements.connection.classList.toggle("is-online", kind === "connected");
  elements.connection.classList.toggle("is-error", kind === "rejected");
  if (state.connection !== kind) {
    elements.announcer.textContent = label;
  }
  state.connection = kind;
  renderOffline();
}

// While the device is paired but not connected, a banner under the header
// says so and the composer keeps its draft without sending it.
function renderOffline() {
  window.clearInterval(state.tickTimer);
  state.tickTimer = null;
  const kind = state.connection;
  const isOffline = kind === "reconnecting" || kind === "offline";
  elements.banner.hidden = !state.rejection && !isOffline;
  elements.banner.classList.toggle("is-error", Boolean(state.rejection));
  elements.retry.hidden = !isOffline || Boolean(state.rejection);
  elements.reload.hidden = !state.rejection;
  if (state.rejection) {
    elements["banner-text"].textContent = state.rejection;
  } else if (kind === "reconnecting") {
    tickCountdown();
    state.tickTimer = window.setInterval(tickCountdown, 1000);
  } else {
    elements["banner-text"].textContent = "Offline";
  }
  const canSend = kind === "connected";
  elements.send.disabled = !canSend;
  elements.message.placeholder = canSend ? "Reply to the agent" : "Offline. Your draft is kept.";
}

function tickCountdown() {
  const seconds = Math.max(1, Math.ceil((state.reconnectAt - Date.now()) / 1000));
  elements["banner-text"].textContent = "Reconnecting in " + seconds + "s";
}

// Shows the pairing card instead of the app. Through a relay the code
// comes from the pairing link, so the fields appear only once a link was
// opened.
function showPairing() {
  setPairingWait(null);
  if (usesRelay) {
    const link = state.pairingLink;
    elements["pair-hint-local"].hidden = true;
    elements["pair-hint-relay"].hidden = Boolean(link);
    elements["pair-fields"].hidden = !link;
    elements["pair-submit"].hidden = !link;
    elements["pair-foot"].hidden = false;
    if (link) {
      elements["pair-code"].value = link.code;
    }
  }
  elements["pair-view"].hidden = false;
  elements.shell.hidden = true;
}

function showShell() {
  elements["pair-view"].hidden = true;
  elements.shell.hidden = false;
  elements["device-name"].textContent = readStored(DEVICE_NAME_KEY) || defaultDeviceName();
  renderMain();
}

// Shows the confirmation code while Agentty waits for approval, or the
// form when `confirmation` is null.
function setPairingWait(confirmation) {
  const isWaiting = confirmation !== null;
  elements["pair-confirm"].hidden = !isWaiting;
  elements["pair-confirmation"].textContent = confirmation || "";
  elements["pair-submit"].hidden = isWaiting;
  elements["pair-cancel"].hidden = !isWaiting;
}

// Shows home or the open session in the main area.
function renderMain() {
  const sessionId = state.currentSessionId;
  elements["home-view"].hidden = Boolean(sessionId);
  elements["session-view"].hidden = !sessionId;
  if (!sessionId) {
    elements.heading.textContent = "agentty";
    renderStatus(null);
    elements.cancel.hidden = true;
    renderHome();
  }
}

function renderHome() {
  const isEmpty = state.isAuthenticated && state.sessionCount === 0;
  elements["home-title"].textContent = isEmpty ? "No sessions yet" : "Pick a session";
  elements["home-pick"].hidden = isEmpty;
  elements["home-none"].hidden = !isEmpty;
  elements["home-sessions"].hidden = isEmpty;
  let summary = "";
  if (state.isAuthenticated) {
    summary = isEmpty ? "no sessions in this project" : sessionCountText(state.sessionCount);
  }
  elements["heading-sub"].replaceChildren(summary);
}

function sessionCountText(count) {
  return count === 1 ? "1 session" : count + " sessions";
}

// Shows the session's status as a badge on wide screens and in the
// header's sub-line on narrow ones, or neither for `null`.
function renderStatus(statusName) {
  const badge = elements["session-status"];
  if (statusName === null) {
    badge.hidden = true;
    elements["heading-sub"].replaceChildren();

    return;
  }
  badge.hidden = false;
  fillStatus(badge, statusName);
  const line = element("span", "status narrow-only");
  fillStatus(line, statusName);
  elements["heading-sub"].replaceChildren(line);
}

function fillStatus(node, statusName) {
  const info = statusInfo(statusName);
  for (const name of [...node.classList]) {
    if (name.startsWith("status--")) {
      node.classList.remove(name);
    }
  }
  node.classList.add("status--" + info.tone);
  node.replaceChildren(element("i"), info.label);
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
  setConnection("connecting");

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
  setConnection("offline");
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
      setPairingWait(String(frame.confirmation));
      setConnection("waiting");
      break;
    case "paired":
      if (!usesRelay) {
        setToken(frame.token);
      }
      state.capabilities = Array.isArray(frame.capabilities) ? frame.capabilities : [];
      onAuthenticated();
      break;
    case "welcome":
      // A relay device that pairs again with a key Agentty already knows
      // is welcomed directly.
      state.capabilities = Array.isArray(frame.capabilities) ? frame.capabilities : [];
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
  if (state.pendingPairing) {
    writeStored(DEVICE_NAME_KEY, state.pendingPairing.deviceName);
    state.pendingPairing = null;
  }
  if (usesRelay) {
    state.isRelayPaired = true;
    state.pairingLink = null;
    writeStored(RELAY_KEY, state.relay.instanceKey + " " + state.relay.relayUrl);
  }
  state.isAuthenticated = true;
  state.backoffMs = 1000;
  elements["list-error"].textContent = "";
  elements["pair-error"].textContent = "";
  setConnection("connected");
  showShell();
  refresh();
}

// Pairing failures, including a denial or a missed approval in the
// terminal, return to the pair form with the reason.
function onRejected(reason) {
  state.stopReconnecting = true;
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
    state.rejection = reason;
  }
  setConnection("rejected");
}

function handleClose(transport) {
  if (state.transport !== transport) {
    return;
  }
  state.isAuthenticated = false;
  state.transport = null;
  failPending("Disconnected");
  if (state.pendingPairing) {
    // The connection ended before Agentty answered the pairing.
    state.pendingPairing = null;
    setPairingWait(null);
    elements["pair-error"].textContent =
      "Could not reach Agentty. Check that remote control is on, then try again.";
  }
  if (state.stopReconnecting || !hasCredential()) {
    // A rejection already said why the connection ended.
    if (state.connection !== "rejected") {
      setConnection("offline");
    }

    return;
  }
  const delay = state.backoffMs;
  state.backoffMs = Math.min(state.backoffMs * 2, MAX_BACKOFF_MS);
  state.reconnectAt = Date.now() + delay;
  state.reconnectTimer = window.setTimeout(() => {
    connect(helloFrame());
  }, delay);
  setConnection("reconnecting");
}

// Reloads the list and the open session. Changes that arrive while a
// reload runs coalesce into one more reload after it.
function refresh() {
  if (!state.isAuthenticated) {
    return;
  }
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
    const sessionId = state.currentSessionId;
    const loaded = await request({ method: "get_session", session_id: sessionId });
    // A session opened while the request ran is loaded by the next one.
    if (sessionId === state.currentSessionId) {
      renderSession(loaded.session);
    }
  }
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
  state.sessionCount = sessions.length;
  elements.sessions.replaceChildren(
    ...sessions.map((session) => {
      const item = element("li");
      const button = element("button");
      button.type = "button";
      button.dataset.sessionId = session.id;
      button.append(
        element("span", "title", session.title || session.id),
        element(
          "span",
          "detail",
          session.status + " · " + relativeTime(session.updated_at, Date.now() / 1000),
        ),
      );
      button.addEventListener("click", () => {
        shell.showSession(session.id);
        enterSession(session.id);
      });
      item.append(button);

      return item;
    }),
  );
  markCurrentRow();
  elements["sessions-empty"].hidden = sessions.length > 0;
  if (!state.currentSessionId) {
    renderHome();
  }
}

// Opens `sessionId` in the main area. The caller has already routed to it.
function enterSession(sessionId) {
  if (sessionId === state.currentSessionId) {
    return;
  }
  state.currentSessionId = sessionId;
  state.session = null;
  elements.heading.textContent = "";
  renderStatus(null);
  elements.cancel.hidden = true;
  elements.transcript.replaceChildren();
  elements.queued.replaceChildren();
  elements.questions.hidden = true;
  elements["session-error"].textContent = "";
  markCurrentRow();
  renderMain();
  refresh();
}

// Returns to home. The caller has already routed there.
function leaveSession() {
  if (!state.currentSessionId) {
    return;
  }
  state.currentSessionId = null;
  state.session = null;
  markCurrentRow();
  renderMain();
  refresh();
}

// Marks the open session's row in the list.
function markCurrentRow() {
  for (const button of elements.sessions.querySelectorAll("button")) {
    if (button.dataset.sessionId === state.currentSessionId) {
      button.setAttribute("aria-current", "page");
    } else {
      button.removeAttribute("aria-current");
    }
  }
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
  renderStatus(session.status);
  elements.cancel.hidden = !statusInfo(session.status).cancel;

  // The transcript follows new content only while the reader is near its
  // end.
  const scroller = elements["chat-scroll"];
  const nearBottom = scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 80;
  if (isNewTranscript) {
    elements.transcript.replaceChildren(
      ...session.messages.map((message) => element("li", message.kind, message.content)),
    );
  }
  elements.queued.replaceChildren(
    ...session.queued_messages.map((message) => element("li", null, "Queued: " + message)),
  );
  if (questionsChanged) {
    renderQuestions(session.questions);
  }
  if (nearBottom && (isNewTranscript || questionsChanged)) {
    scroller.scrollTop = scroller.scrollHeight;
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
    const group = element("div");
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

  const submit = element("button", null, "Answer");
  submit.type = "submit";
  form.append(submit);
}

const shell = createShell({
  document: document,
  elements: {
    app: elements.shell,
    close: elements["drawer-close"],
    hide: elements["sidebar-hide"],
    main: elements.main,
    menu: elements.menu,
    scrim: elements.scrim,
    sidebar: elements.sidebar,
  },
  onRoute: (route) => {
    if (route.kind === "session") {
      enterSession(route.sessionId);
    } else {
      leaveSession();
    }
  },
  parsePairingLink: parsePairingLink,
  window: window,
});

elements["pair-name"].value = readStored(DEVICE_NAME_KEY) || defaultDeviceName();

elements["pair-form"].addEventListener("submit", (event) => {
  event.preventDefault();
  const code = elements["pair-code"].value.trim();
  if (!code || state.pendingPairing) {
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
  const deviceName = elements["pair-name"].value.trim();
  elements["pair-error"].textContent = "";
  state.pendingPairing = { code: code, deviceName: deviceName };
  connect({
    type: "pair",
    code: code,
    device_name: deviceName,
    protocol_version: PROTOCOL_VERSION,
  });
});

// Stops waiting for approval. Agentty drops the request once the
// connection closes.
elements["pair-cancel"].addEventListener("click", () => {
  state.pendingPairing = null;
  disconnect();
  setPairingWait(null);
});

for (const button of document.querySelectorAll("[data-theme]")) {
  button.addEventListener("click", () => {
    const theme = THEMES.includes(button.dataset.theme) ? button.dataset.theme : THEMES[0];
    applyTheme(document, theme);
    saveTheme(window, theme);
  });
}

function setDeviceMenu(isOpen) {
  elements["device-menu"].hidden = !isOpen;
  elements["device-more"].setAttribute("aria-expanded", String(isOpen));
}

elements["device-more"].addEventListener("click", () => {
  setDeviceMenu(elements["device-menu"].hidden);
});

document.addEventListener("click", (event) => {
  const target = event.target;
  if (
    !elements["device-menu"].hidden &&
    !elements["device-menu"].contains(target) &&
    !elements["device-more"].contains(target)
  ) {
    setDeviceMenu(false);
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !elements["device-menu"].hidden) {
    setDeviceMenu(false);
    elements["device-more"].focus();
  }
});

elements.forget.addEventListener("click", () => {
  setDeviceMenu(false);
  elements["forget-dialog"].returnValue = "";
  elements["forget-dialog"].showModal();
});

elements["forget-dialog"].addEventListener("close", () => {
  if (elements["forget-dialog"].returnValue !== "confirm") {
    return;
  }
  forgetCredential();
  state.currentSessionId = null;
  state.session = null;
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
  elements["pair-name"].value = defaultDeviceName();
  showPairing();
});

elements["home-sessions"].addEventListener("click", () => elements.menu.click());

elements.retry.addEventListener("click", () => {
  if (hasCredential()) {
    connect(helloFrame());
  }
});

elements.reload.addEventListener("click", () => window.location.reload());

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
  if (!message.trim() || !state.currentSessionId || elements.send.disabled) {
    return;
  }
  elements.send.disabled = true;
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
    elements.send.disabled = state.connection !== "connected";
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
  const route = parseRoute(window.location.hash, parsePairingLink);
  if (route.kind !== "pairing") {
    return false;
  }
  window.history.replaceState(null, "", window.location.pathname + window.location.search);
  state.pairingLink = route.link;

  return true;
}

// Opens the session the address names, if any, once the app connects.
function adoptSessionRoute() {
  const route = parseRoute(window.location.hash, parsePairingLink);
  state.currentSessionId = route.kind === "session" ? route.sessionId : null;
}

// Shows the app and connects with the saved credential.
function resume() {
  adoptSessionRoute();
  showShell();
  connect(helloFrame());
}

async function startThroughRelay() {
  if (!(await isSupported())) {
    elements["pair-hint-local"].hidden = true;
    elements["pair-fields"].hidden = true;
    elements["pair-submit"].hidden = true;
    elements["pair-error"].textContent =
      "This browser cannot pair through a relay. Open the link in an up-to-date browser, over HTTPS.";
    elements["pair-view"].hidden = false;

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
      state.pendingPairing = null;
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
  resume();
}

if (usesRelay) {
  await startThroughRelay();
} else if (token()) {
  resume();
} else {
  showPairing();
}
