// Agentty remote control client. Speaks protocol version 1 over one
// WebSocket at /ws, or over an encrypted stream through the relay that
// serves this page; see crates/ag-remote/src/protocol.rs for the frames.

import { createChat, createRefreshGate } from "./chat.mjs";
import { forgetDeviceKey, loadDeviceKey } from "./device_key.mjs";
import { createDiff } from "./diff.mjs";
import { createHome } from "./home.mjs";
import { isSupported } from "./noise.mjs";
import { openRelayTransport, parsePairingLink } from "./relay.mjs";
import { TICK_MS, createSessionList } from "./session_list.mjs";
import { createSettings } from "./settings.mjs";
import {
  NARROW_QUERY,
  THEMES,
  applyTheme,
  createShell,
  loadTheme,
  parseRoute,
  saveTheme,
} from "./shell.mjs";

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
  "pair-wait-label": document.getElementById("pair-wait-label"),
  "pair-wait-hint": document.getElementById("pair-wait-hint"),
  "pair-error": document.getElementById("pair-error"),
  "pair-submit": document.getElementById("pair-submit"),
  "pair-cancel": document.getElementById("pair-cancel"),
  "pair-foot": document.getElementById("pair-foot"),
  shell: document.getElementById("shell"),
  sidebar: document.getElementById("sidebar"),
  "sidebar-hide": document.getElementById("sidebar-hide"),
  "drawer-close": document.getElementById("drawer-close"),
  project: document.getElementById("project"),
  "project-name": document.getElementById("project-name"),
  "new-session": document.getElementById("new-session"),
  filter: document.getElementById("filter"),
  "filter-key": document.getElementById("filter-key"),
  chips: document.getElementById("chips"),
  sessions: document.getElementById("sessions"),
  "sessions-empty": document.getElementById("sessions-empty"),
  "filter-summary": document.getElementById("filter-summary"),
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
  diff: document.getElementById("diff"),
  merge: document.getElementById("merge"),
  publish: document.getElementById("publish"),
  "publish-label": document.getElementById("publish-label"),
  "session-more": document.getElementById("session-more"),
  "action-error": document.getElementById("action-error"),
  "action-note": document.getElementById("action-note"),
  "session-sheet": document.getElementById("session-sheet"),
  "sheet-title": document.getElementById("sheet-title"),
  "sheet-sub": document.getElementById("sheet-sub"),
  "sheet-diff": document.getElementById("sheet-diff"),
  "sheet-diff-label": document.getElementById("sheet-diff-label"),
  "sheet-merge": document.getElementById("sheet-merge"),
  "sheet-merge-label": document.getElementById("sheet-merge-label"),
  "sheet-sync": document.getElementById("sheet-sync"),
  "sheet-sync-label": document.getElementById("sheet-sync-label"),
  "sheet-publish": document.getElementById("sheet-publish"),
  "sheet-publish-note": document.getElementById("sheet-publish-note"),
  cancel: document.getElementById("cancel"),
  "cancel-dialog": document.getElementById("cancel-dialog"),
  "merge-dialog": document.getElementById("merge-dialog"),
  "merge-target": document.getElementById("merge-target"),
  "publish-dialog": document.getElementById("publish-dialog"),
  "publish-branch": document.getElementById("publish-branch"),
  "publish-review": document.getElementById("publish-review"),
  "sync-dialog": document.getElementById("sync-dialog"),
  "sync-target": document.getElementById("sync-target"),
  "settings-dialog": document.getElementById("settings-dialog"),
  "settings-form": document.getElementById("settings-form"),
  "settings-model": document.getElementById("settings-model"),
  "settings-reasoning": document.getElementById("settings-reasoning"),
  "settings-style": document.getElementById("settings-style"),
  "settings-speed-row": document.getElementById("settings-speed-row"),
  "settings-speed": document.getElementById("settings-speed"),
  "settings-note": document.getElementById("settings-note"),
  "settings-error": document.getElementById("settings-error"),
  "settings-apply": document.getElementById("settings-apply"),
  banner: document.getElementById("banner"),
  "banner-text": document.getElementById("banner-text"),
  retry: document.getElementById("retry"),
  reload: document.getElementById("reload"),
  "home-view": document.getElementById("home-view"),
  "home-eyebrow": document.getElementById("home-eyebrow"),
  "home-title": document.getElementById("home-title"),
  "home-form": document.getElementById("home-form"),
  "home-message": document.getElementById("home-message"),
  "home-composer-hint": document.getElementById("home-composer-hint"),
  "home-composer-meta": document.getElementById("home-composer-meta"),
  "home-composer-settings": document.getElementById("home-composer-settings"),
  "home-send": document.getElementById("home-send"),
  "home-modes": document.getElementById("home-modes"),
  "home-error": document.getElementById("home-error"),
  "home-pick": document.getElementById("home-pick"),
  "home-none": document.getElementById("home-none"),
  "home-sessions": document.getElementById("home-sessions"),
  "home-waiting": document.getElementById("home-waiting"),
  "home-cards": document.getElementById("home-cards"),
  "session-view": document.getElementById("session-view"),
  "session-gone": document.getElementById("session-gone"),
  "session-gone-home": document.getElementById("session-gone-home"),
  "chat-scroll": document.getElementById("chat-scroll"),
  "diff-view": document.getElementById("diff-view"),
  "diff-summary": document.getElementById("diff-summary"),
  "diff-error": document.getElementById("diff-error"),
  "diff-files": document.getElementById("diff-files"),
  dock: document.getElementById("dock"),
  transcript: document.getElementById("transcript"),
  questions: document.getElementById("questions"),
  jump: document.getElementById("jump"),
  "queued-box": document.getElementById("queued-box"),
  queued: document.getElementById("queued"),
  composer: document.getElementById("composer"),
  message: document.getElementById("message"),
  "composer-meta": document.getElementById("composer-meta"),
  "composer-settings": document.getElementById("composer-settings"),
  "composer-hint": document.getElementById("composer-hint"),
  "merge-lines": document.getElementById("merge-lines"),
  stop: document.getElementById("stop"),
  send: document.getElementById("send"),
  "session-error": document.getElementById("session-error"),
  "answer-bar": document.getElementById("answer-bar"),
  "answer-count": document.getElementById("answer-count"),
  "answer-send": document.getElementById("answer-send"),
  "answer-error": document.getElementById("answer-error"),
  "read-only": document.getElementById("read-only"),
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
  // Why Agentty refused this device for good, until the page reloads.
  rejection: null,
  // Relay address and instance key that relay streams connect to.
  relay: null,
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
  chat.setConnected(kind === "connected");
  home.setConnected(kind === "connected");
}

function tickCountdown() {
  const seconds = Math.max(1, Math.ceil((state.reconnectAt - Date.now()) / 1000));
  elements["banner-text"].textContent = "Reconnecting in " + seconds + "s";
}

// Shows the pairing card instead of the app. Through a relay the code
// comes from the pairing link, so the fields appear only once a link was
// opened.
function showPairing() {
  // The drawer and the session route belong to the app this card replaces.
  shell.reset();
  clearSessionRoute();
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
  setPairingPhase(confirmation === null ? "idle" : "waiting", confirmation);
}

// Shows the pairing form in `phase`: `idle` offers Pair; `connecting`
// and `waiting` offer Cancel instead, and `waiting` adds the confirmation
// code Agentty shows.
function setPairingPhase(phase, confirmation = null) {
  const isBusy = phase !== "idle";
  const isWaiting = phase === "waiting";
  elements["pair-confirm"].hidden = !isBusy;
  elements["pair-wait-label"].textContent = isWaiting
    ? "Waiting for approval in Agentty"
    : "Connecting to Agentty";
  elements["pair-confirmation"].textContent = confirmation || "";
  elements["pair-confirmation"].hidden = !isWaiting;
  elements["pair-wait-hint"].hidden = !isWaiting;
  elements["pair-submit"].hidden = isBusy;
  elements["pair-cancel"].hidden = !isBusy;
}

// Removes a session route from the address without a new history entry.
function clearSessionRoute() {
  if (parseRoute(window.location.hash, parsePairingLink).kind === "session") {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
  }
}

// Shows home or the open session in the main area.
function renderMain() {
  const sessionId = state.currentSessionId;
  elements["home-view"].hidden = Boolean(sessionId);
  elements["session-view"].hidden = !sessionId;
  if (!sessionId) {
    chat.close();
    elements.heading.textContent = "agentty";
    renderHome();
  }
}

// Home asks for a new session's first message when the host offers that,
// and otherwise offers the sessions to pick from.
function renderHome() {
  const isEmpty = state.isAuthenticated && state.sessionCount === 0;
  const host = home.host();
  elements["home-view"].classList.toggle("is-composing", Boolean(host));
  if (host) {
    elements["home-eyebrow"].textContent =
      "New session · " + host.project + (host.baseBranch ? " · base " + host.baseBranch : "");
    elements["home-title"].textContent = "What should the agent work on?";
  } else {
    elements["home-eyebrow"].textContent = isEmpty ? "Connected" : "Remote control";
    elements["home-title"].textContent = isEmpty ? "No sessions yet" : "Pick a session";
  }
  elements["home-pick"].hidden = isEmpty || Boolean(host);
  elements["home-none"].hidden = !isEmpty || Boolean(host);
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
  chat.setCapabilities(state.capabilities);
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
  refresh(true);
  // Starting a session needs the `host` and `create_session` capabilities;
  // the button appears once the host described its project.
  void home.setCapabilities(state.capabilities);
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

// Reloads the list and the open session, at most once a second and not
// while the page is hidden, since a relay limits how much a device reads.
// Changes that arrive meanwhile coalesce into one more reload.
// `refresh(true)` skips the wait, as when a session opens.
function refresh(isUrgent = false) {
  refreshGate.request(isUrgent);
}

async function loadSessions() {
  if (!state.isAuthenticated) {
    return;
  }
  // A session opened while this runs is loaded by the next run, and an
  // answer for this one is dropped once another is open.
  const sessionId = state.currentSessionId;
  try {
    const listed = await request({ method: "list_sessions" });
    const sessions = listed.sessions || [];
    renderSessions(sessions);
    elements["list-error"].textContent = "";
    if (sessionId) {
      const loaded = await request({ method: "get_session", session_id: sessionId });
      if (sessionId === state.currentSessionId) {
        // The listing carries the figures the header shows beside the
        // snapshot.
        chat.render(loaded.session, sessions.find((summary) => summary.id === sessionId));
      }
    }
  } catch (error) {
    if (sessionId) {
      chat.showLoadError(sessionId, error.message);
    } else {
      elements["list-error"].textContent = error.message;
    }
  }
}

function renderSessions(sessions) {
  state.sessionCount = sessions.length;
  sessionList.update(sessions, state.currentSessionId);
  if (!state.currentSessionId) {
    renderHome();
  }
}

// Opens `sessionId` in the main area. The caller has already routed to it.
// `failure` is a first message from home that did not go through.
function enterSession(sessionId, failure = null) {
  if (sessionId === state.currentSessionId) {
    return;
  }
  state.currentSessionId = sessionId;
  chat.open(sessionId, failure);
  markCurrentRow();
  renderMain();
  refresh(true);
}

// Returns to home. The caller has already routed there.
function leaveSession() {
  if (!state.currentSessionId) {
    return;
  }
  state.currentSessionId = null;
  markCurrentRow();
  renderMain();
  refresh(true);
}

// Marks the open session's row in the list and counts it as seen.
function markCurrentRow() {
  sessionList.setOpen(state.currentSessionId);
}

// The settings sheet, for the open session and for home's new one.
const settingsSheet = createSettings({
  document: document,
  elements: {
    apply: elements["settings-apply"],
    dialog: elements["settings-dialog"],
    error: elements["settings-error"],
    form: elements["settings-form"],
    model: elements["settings-model"],
    note: elements["settings-note"],
    reasoning: elements["settings-reasoning"],
    speed: elements["settings-speed"],
    speedRow: elements["settings-speed-row"],
    style: elements["settings-style"],
  },
  request: request,
});

const chat = createChat({
  diff: createDiff({
    document: document,
    elements: {
      error: elements["diff-error"],
      files: elements["diff-files"],
      summary: elements["diff-summary"],
      view: elements["diff-view"],
    },
    request: request,
  }),
  document: document,
  elements: {
    actionError: elements["action-error"],
    actionNote: elements["action-note"],
    answerBar: elements["answer-bar"],
    answerCount: elements["answer-count"],
    answerError: elements["answer-error"],
    answerSend: elements["answer-send"],
    badge: elements["session-status"],
    cancel: elements.cancel,
    cancelDialog: elements["cancel-dialog"],
    composer: elements.composer,
    composerHint: elements["composer-hint"],
    composerMeta: elements["composer-meta"],
    composerSettings: elements["composer-settings"],
    diff: elements.diff,
    dock: elements.dock,
    gone: elements["session-gone"],
    goneHome: elements["session-gone-home"],
    heading: elements.heading,
    jump: elements.jump,
    merge: elements.merge,
    mergeDialog: elements["merge-dialog"],
    mergeLines: elements["merge-lines"],
    mergeTarget: elements["merge-target"],
    message: elements.message,
    more: elements["session-more"],
    publish: elements.publish,
    publishBranch: elements["publish-branch"],
    publishDialog: elements["publish-dialog"],
    publishLabel: elements["publish-label"],
    publishReview: elements["publish-review"],
    questions: elements.questions,
    queued: elements.queued,
    queuedBox: elements["queued-box"],
    readonly: elements["read-only"],
    scroller: elements["chat-scroll"],
    send: elements.send,
    sessionError: elements["session-error"],
    sheet: elements["session-sheet"],
    sheetDiff: elements["sheet-diff"],
    sheetDiffLabel: elements["sheet-diff-label"],
    sheetMerge: elements["sheet-merge"],
    sheetMergeLabel: elements["sheet-merge-label"],
    sheetPublish: elements["sheet-publish"],
    sheetPublishNote: elements["sheet-publish-note"],
    sheetSub: elements["sheet-sub"],
    sheetSync: elements["sheet-sync"],
    sheetSyncLabel: elements["sheet-sync-label"],
    sheetTitle: elements["sheet-title"],
    stop: elements.stop,
    sub: elements["heading-sub"],
    syncDialog: elements["sync-dialog"],
    syncTarget: elements["sync-target"],
    transcript: elements.transcript,
  },
  onHome: () => {
    shell.showSession(null);
    leaveSession();
  },
  request: request,
  settings: settingsSheet,
  window: window,
});

const refreshGate = createRefreshGate({ document: document, run: loadSessions, window: window });

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
    if (!elements["pair-view"].hidden) {
      // An unpaired page has no sessions to route to.
      clearSessionRoute();
    } else if (route.kind === "session") {
      enterSession(route.sessionId);
    } else {
      leaveSession();
    }
  },
  parsePairingLink: parsePairingLink,
  window: window,
});

const sessionList = createSessionList({
  document: document,
  elements: {
    cards: elements["home-cards"],
    chips: elements.chips,
    empty: elements["sessions-empty"],
    filter: elements.filter,
    filterKey: elements["filter-key"],
    list: elements.sessions,
    summary: elements["filter-summary"],
    waiting: elements["home-waiting"],
  },
  // The field is on screen in the open drawer, or in the sidebar on a
  // wide screen unless it was hidden.
  isFilterShown: () => {
    if (elements.shell.hidden) {
      return false;
    }

    return window.matchMedia(NARROW_QUERY).matches
      ? shell.isOpen()
      : !elements.shell.classList.contains("is-sidebar-hidden");
  },
  onOpen: (sessionId) => {
    shell.showSession(sessionId);
    enterSession(sessionId);
  },
  window: window,
});
window.setInterval(sessionList.tick, TICK_MS);
window.setInterval(chat.tick, 1000);
// An update that arrived while the page was hidden stays unread on the
// open session until the page shows again.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") {
    sessionList.markSeen();
  }
});

const home = createHome({
  document: document,
  elements: {
    error: elements["home-error"],
    form: elements["home-form"],
    hint: elements["home-composer-hint"],
    message: elements["home-message"],
    meta: elements["home-composer-meta"],
    modes: elements["home-modes"],
    send: elements["home-send"],
    settingsButton: elements["home-composer-settings"],
  },
  // The new session opens only if the user is still on home; otherwise it
  // shows up in the list, with an unsent first message kept as its draft.
  onCreated: (sessionId, failure) => {
    if (state.currentSessionId === null) {
      shell.showSession(sessionId);
      enterSession(sessionId, failure);
    } else {
      if (failure) {
        chat.keepDraft(sessionId, failure.draft);
      }
      refresh(true);
    }
  },
  onHost: (host) => {
    elements["new-session"].hidden = !host;
    elements.project.hidden = !host;
    elements["project-name"].textContent = host?.project ?? "";
    if (!state.currentSessionId) {
      renderHome();
    }
  },
  request: request,
  settings: settingsSheet,
  window: window,
});

// New session leads to the home composer.
elements["new-session"].addEventListener("click", () => {
  shell.showSession(null);
  leaveSession();
  home.focus();
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
  // Cancel shows at once, so a connection that hangs can be abandoned.
  setPairingPhase("connecting");
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

// `Esc` closes the device menu first. Handling it on the sidebar, before
// it reaches the document, keeps the same key from also closing the
// drawer, so focus returns to the visible menu button.
elements.sidebar.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !elements["device-menu"].hidden) {
    event.preventDefault();
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
  chat.close();
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
  if (state.currentSessionId) {
    chat.open(state.currentSessionId);
  }
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
      chat.close();
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
