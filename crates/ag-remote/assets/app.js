// Agentty remote control client. Speaks protocol version 1 over one
// WebSocket at /ws, or over encrypted streams through the relay that
// serves this page, one for each Agentty this browser paired with; see
// crates/ag-remote/src/protocol.rs for the frames.

import { createChat, createRefreshGate } from "./chat.mjs";
import { CREDENTIAL_REJECTIONS, PROTOCOL_VERSION, createConnection } from "./connection.mjs";
import { forgetDeviceKey, loadDeviceKey } from "./device_key.mjs";
import { createDiff } from "./diff.mjs";
import { parseProject } from "./format.mjs";
import { createGraph } from "./graph.mjs";
import { createHome, offersCreation } from "./home.mjs";
import { createInstanceView, fallbackName } from "./instance.mjs";
import { isSupported } from "./noise.mjs";
import { forgetPairing, instanceIdOf, loadPairings, savePairing } from "./pairings.mjs";
import { createProject } from "./project.mjs";
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

const TOKEN_KEY = "agentty.remote.token";
const DEVICE_NAME_KEY = "agentty.remote.device_name";
// A relay serves this page with the marker set to "relay". The device then
// pairs with a link and reaches each Agentty through that relay, encrypted
// end to end, and its key replaces the token.
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
// What an Agentty's heading says in each state, beside its name.
const STATE_LABELS = { ...CONNECTION_LABELS, connected: "Connected" };

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
  "pair-back": document.getElementById("pair-back"),
  "pair-foot": document.getElementById("pair-foot"),
  shell: document.getElementById("shell"),
  sidebar: document.getElementById("sidebar"),
  "sidebar-hide": document.getElementById("sidebar-hide"),
  "drawer-close": document.getElementById("drawer-close"),
  "instance-slot": document.getElementById("instance-slot"),
  "instance-prototype": document.getElementById("instance-prototype"),
  "project-sync-dialog": document.getElementById("project-sync-dialog"),
  "project-sync-target": document.getElementById("project-sync-target"),
  "project-sync-branch": document.getElementById("project-sync-branch"),
  filter: document.getElementById("filter"),
  "filter-key": document.getElementById("filter-key"),
  chips: document.getElementById("chips"),
  sessions: document.getElementById("sessions"),
  "filter-summary": document.getElementById("filter-summary"),
  "device-name": document.getElementById("device-name"),
  connection: document.getElementById("connection"),
  "device-more": document.getElementById("device-more"),
  "device-menu": document.getElementById("device-menu"),
  forget: document.getElementById("forget"),
  "forget-each": document.getElementById("forget-each"),
  "forget-dialog": document.getElementById("forget-dialog"),
  "forget-target": document.getElementById("forget-target"),
  scrim: document.getElementById("scrim"),
  main: document.getElementById("main"),
  menu: document.getElementById("menu"),
  heading: document.getElementById("heading"),
  "heading-sub": document.getElementById("heading-sub"),
  "session-status": document.getElementById("session-status"),
  diff: document.getElementById("diff"),
  graphs: document.getElementById("graphs"),
  merge: document.getElementById("merge"),
  publish: document.getElementById("publish"),
  "publish-label": document.getElementById("publish-label"),
  "session-more": document.getElementById("session-more"),
  "action-error": document.getElementById("action-error"),
  "action-note": document.getElementById("action-note"),
  "session-sheet": document.getElementById("session-sheet"),
  "sheet-title": document.getElementById("sheet-title"),
  "sheet-sub": document.getElementById("sheet-sub"),
  "sheet-details": document.getElementById("sheet-details"),
  "sheet-diff": document.getElementById("sheet-diff"),
  "sheet-diff-label": document.getElementById("sheet-diff-label"),
  "sheet-graphs": document.getElementById("sheet-graphs"),
  "sheet-graphs-label": document.getElementById("sheet-graphs-label"),
  "sheet-merge": document.getElementById("sheet-merge"),
  "sheet-merge-label": document.getElementById("sheet-merge-label"),
  "sheet-merge-note": document.getElementById("sheet-merge-note"),
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
  "publish-wait": document.getElementById("publish-wait"),
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
  "graph-view": document.getElementById("graph-view"),
  "graph-prev": document.getElementById("graph-prev"),
  "graph-position": document.getElementById("graph-position"),
  "graph-next": document.getElementById("graph-next"),
  "graph-refresh": document.getElementById("graph-refresh"),
  "graph-title": document.getElementById("graph-title"),
  "graph-error": document.getElementById("graph-error"),
  "graph-hints": document.getElementById("graph-hints"),
  "graph-note": document.getElementById("graph-note"),
  "graph-preview": document.getElementById("graph-preview"),
  "graph-nodes": document.getElementById("graph-nodes"),
  "graph-detail": document.getElementById("graph-detail"),
  "graph-actions": document.getElementById("graph-actions"),
  "graph-cancel-dialog": document.getElementById("graph-cancel-dialog"),
  "graph-summary": document.getElementById("graph-summary"),
  dock: document.getElementById("dock"),
  "draft-preview": document.getElementById("draft-preview"),
  transcript: document.getElementById("transcript"),
  "focused-review": document.getElementById("focused-review"),
  "status-line": document.getElementById("status-line"),
  questions: document.getElementById("questions"),
  jump: document.getElementById("jump"),
  "queued-box": document.getElementById("queued-box"),
  queued: document.getElementById("queued"),
  composer: document.getElementById("composer"),
  message: document.getElementById("message"),
  "composer-meta": document.getElementById("composer-meta"),
  "composer-settings": document.getElementById("composer-settings"),
  "composer-hint": document.getElementById("composer-hint"),
  "composer-graph": document.getElementById("composer-graph"),
  "composer-graph-leave": document.getElementById("composer-graph-leave"),
  "composer-graph-note": document.getElementById("composer-graph-note"),
  "merge-lines": document.getElementById("merge-lines"),
  stop: document.getElementById("stop"),
  send: document.getElementById("send"),
  "session-error": document.getElementById("session-error"),
  "answer-bar": document.getElementById("answer-bar"),
  "answer-count": document.getElementById("answer-count"),
  "answer-chat": document.getElementById("answer-chat"),
  "answer-send": document.getElementById("answer-send"),
  "answer-error": document.getElementById("answer-error"),
  "read-only": document.getElementById("read-only"),
};

const state = {
  // The open session and the Agentty it runs on, or null on home.
  current: null,
  // This browser's key pair for relay streams.
  deviceKey: null,
  // The Agentty `Forget this device` asks about.
  forgetTarget: null,
  // The Agentty the user chose for new sessions, by id, if any.
  homeId: null,
  // The Agentty home describes; see `describeHome`.
  homeTarget: null,
  // Every Agentty in the sidebar, in the order they were paired. Without a
  // relay, the one behind this page's listener, with the id "".
  instances: [],
  // The pairing in progress, `{ instance, deviceName }`, until Agentty
  // answers it. Its Agentty joins `instances` once paired.
  pairing: null,
  // Pairing link this page was opened with, until pairing succeeds.
  pairingLink: null,
  // The relay pairings saved in this browser; see pairings.mjs.
  pairings: [],
  // Ticks the banner's reconnect countdown.
  tickTimer: null,
};

// Storage for pairings.mjs, whose calls tolerate a throwing storage.
const storage = {
  getItem: (key) => window.localStorage.getItem(key),
  removeItem: (key) => window.localStorage.removeItem(key),
  setItem: (key, value) => window.localStorage.setItem(key, value),
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

// Whether `instance` holds a pairing it can resume.
function hasCredential(instance) {
  if (!usesRelay) {
    return Boolean(token());
  }

  return state.pairings.some((pairing) => pairing.instanceId === instance.id);
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

// Whether the sidebar shows several Agenttys, each under its own heading.
function isMany() {
  return state.instances.length > 1;
}

// The id a route names `instance` by: none without a relay.
function routeId(instance) {
  return usesRelay ? instance.id : null;
}

function findInstance(id) {
  return state.instances.find((instance) => instance.id === id) ?? null;
}

// Builds the Agentty with `id`; `pairing` holds its relay pairing.
function createInstance(id, pairing) {
  const view = createInstanceView({ document: document, prototype: elements["instance-prototype"] });
  const instance = {
    connection: null,
    id: id,
    // Whether a listing arrived since the page loaded.
    isListed: false,
    // The connection state the announcer last read out.
    lastKind: null,
    name: id ? fallbackName(id) : "",
    pairing: pairing,
    project: null,
    // Name of the project the last listing named, if any did.
    projectName: null,
    refreshGate: null,
    // Why Agentty refused this device for good, until the page reloads.
    rejection: null,
    sessions: [],
    view: view,
  };
  instance.connection = createConnection({
    canReconnect: () => hasCredential(instance) && state.pairing?.instance !== instance,
    helloFrame: helloFrame,
    on: {
      authenticated: (frame) => onAuthenticated(instance, frame),
      awaiting: (confirmation) => {
        if (state.pairing?.instance === instance) {
          setPairingWait(confirmation);
        }
      },
      changed: () => instance.refreshGate.request(false),
      pairingLost: () => onPairingLost(instance),
      rejected: (reason, wasPairing) => onRejected(instance, reason, wasPairing),
      state: () => onConnectionState(instance),
    },
    openTransport: (handlers) =>
      usesRelay
        ? openRelayTransport({
            ...handlers,
            deviceKey: state.deviceKey,
            instanceKey: instance.pairing.instanceKey,
            relayUrl: instance.pairing.relayUrl,
          })
        : openLocalTransport(handlers),
    window: window,
  });
  instance.project = createProject({
    elements: {
      ...view.project,
      dialog: elements["project-sync-dialog"],
      dialogBranch: elements["project-sync-branch"],
      dialogTarget: elements["project-sync-target"],
    },
    request: instance.connection.request,
  });
  // Reloads the list and the open session, at most once a second and not
  // while the page is hidden, since a relay limits how much a device
  // reads. Changes that arrive meanwhile coalesce into one more reload.
  instance.refreshGate = createRefreshGate({
    document: document,
    run: () => loadSessions(instance),
    window: window,
  });
  view.newSession.addEventListener("click", () => startNewSession(instance));
  view.setName(instance.name);
  view.setState("offline", STATE_LABELS.offline);

  return instance;
}

function isConnected(instance) {
  return instance?.connection.kind() === "connected";
}

// The Agentty whose state the banner and the composers follow: the open
// session's, or on home the one new sessions start on.
function activeInstance() {
  return state.current?.instance ?? state.homeTarget;
}

// Lays the Agenttys out: one alone keeps the single page's layout, several
// each get a heading. The footer's menu offers forgetting each.
function placeInstances() {
  const slot = elements["instance-slot"];
  if (isMany()) {
    slot.replaceChildren();
    slot.hidden = true;
    elements.sessions.replaceChildren(
      ...state.instances.flatMap((instance) => instance.view.mount(false, slot)),
    );
  } else {
    const nodes = state.instances[0]?.view.mount(true, slot) ?? [];
    slot.hidden = nodes.length === 0;
    if (nodes.length === 0) {
      slot.replaceChildren();
    }
    elements.sessions.replaceChildren(...nodes);
  }
  renderForgetItems();
  renderSessions();
  renderFooter();
}

// Shows each Agentty's state in its heading and the device footer,
// announces a change once, and updates the banner and composers.
function onConnectionState(instance) {
  const kind = instance.connection.kind();
  instance.view.setState(kind, STATE_LABELS[kind]);
  if (instance.lastKind !== kind && (state.instances.includes(instance) || state.pairing?.instance === instance)) {
    elements.announcer.textContent = isMany()
      ? instance.name + ": " + STATE_LABELS[kind]
      : CONNECTION_LABELS[kind];
  }
  instance.lastKind = kind;
  instance.project.setConnected(kind === "connected");
  renderFooter();
  renderNewSessionButtons();
  renderOffline();
}

// The device footer: one Agentty's state, or how many of several are
// connected.
function renderFooter() {
  const pairing = state.pairing?.instance;
  const shown = state.instances.length > 0 ? state.instances : pairing ? [pairing] : [];
  const kinds = shown.map((instance) => instance.connection.kind());
  let label;
  if (kinds.length <= 1) {
    label = CONNECTION_LABELS[kinds[0] ?? "offline"];
  } else {
    const connected = kinds.filter((kind) => kind === "connected").length;
    label = connected === kinds.length ? CONNECTION_LABELS.connected : connected + " of " + kinds.length + " connected";
  }
  elements.connection.textContent = label;
  elements.connection.classList.toggle("is-online", kinds.length > 0 && kinds.every((kind) => kind === "connected"));
  elements.connection.classList.toggle("is-error", kinds.includes("rejected"));
}

// With several Agenttys, the device menu forgets this device on each.
function renderForgetItems() {
  elements.forget.hidden = isMany();
  elements["forget-each"].hidden = !isMany();
  elements["forget-each"].replaceChildren(
    ...(isMany() ? state.instances : []).map((instance) => {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "menu-item is-danger";
      item.setAttribute("role", "menuitem");
      item.textContent = "Forget on " + instance.name;
      item.addEventListener("click", () => askToForget(instance));

      return item;
    }),
  );
}

// While the Agentty in view is paired but not connected, a banner under
// the header says so and the composer keeps its draft without sending it.
function renderOffline() {
  window.clearInterval(state.tickTimer);
  state.tickTimer = null;
  const active = activeInstance();
  const kind = active?.connection.kind() ?? "offline";
  const rejection = active?.rejection ?? null;
  const isOffline = Boolean(active) && (kind === "reconnecting" || kind === "offline");
  elements.banner.hidden = !rejection && !isOffline;
  elements.banner.classList.toggle("is-error", Boolean(rejection));
  elements.retry.hidden = !isOffline || Boolean(rejection);
  elements.reload.hidden = !rejection;
  if (rejection) {
    elements["banner-text"].textContent = bannerText(active, rejection);
  } else if (kind === "reconnecting") {
    tickCountdown();
    state.tickTimer = window.setInterval(tickCountdown, 1000);
  } else {
    elements["banner-text"].textContent = bannerText(active, "Offline");
  }
  chat.setConnected(isConnected(state.current?.instance));
  home.setConnected(isConnected(state.homeTarget));
}

// The banner names the Agentty it is about when the page shows several.
function bannerText(instance, text) {
  return isMany() && instance ? instance.name + " · " + text : text;
}

function tickCountdown() {
  const active = activeInstance();
  const reconnectAt = active?.connection.reconnectAt() ?? Date.now();
  const seconds = Math.max(1, Math.ceil((reconnectAt - Date.now()) / 1000));
  elements["banner-text"].textContent = bannerText(active, "Reconnecting in " + seconds + "s");
}

// Shows the pairing card instead of the app. Through a relay the code
// comes from the pairing link, so the fields appear only once a link was
// opened.
function showPairing() {
  // The drawer and the session route belong to the app this card replaces.
  shell.reset();
  clearSessionRoute();
  leaveSession();
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

// Shows the pairing form in `phase`: `idle` offers Pair, and Back to
// sessions when other Agenttys are paired; `connecting` and `waiting`
// offer Cancel instead, and `waiting` adds the confirmation code Agentty
// shows.
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
  elements["pair-submit"].hidden = isBusy || (usesRelay && !state.pairingLink);
  elements["pair-cancel"].hidden = !isBusy;
  elements["pair-back"].hidden = isBusy || !usesRelay || state.instances.length === 0;
}

// Removes a session route from the address without a new history entry.
function clearSessionRoute() {
  if (parseRoute(window.location.hash, parsePairingLink).kind === "session") {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
  }
}

// Shows home or the open session in the main area.
function renderMain() {
  const isOpen = Boolean(state.current);
  elements["home-view"].hidden = isOpen;
  elements["session-view"].hidden = !isOpen;
  if (!isOpen) {
    chat.close();
    elements.heading.textContent = "agentty";
    renderHome();
  }
}

// Home asks for a new session's first message when the host offers that,
// and otherwise offers the sessions to pick from.
function renderHome() {
  const isAuthenticated = state.instances.some((instance) => instance.connection.isAuthenticated());
  const count = state.instances.reduce((total, instance) => total + instance.sessions.length, 0);
  const isEmpty = isAuthenticated && count === 0;
  const host = home.host();
  elements["home-view"].classList.toggle("is-composing", Boolean(host));
  if (host) {
    const parts = ["New session"];
    if (isMany()) {
      parts.push(state.homeTarget.name);
    }
    parts.push(host.project);
    if (host.baseBranch) {
      parts.push("base " + host.baseBranch);
    }
    elements["home-eyebrow"].textContent = parts.join(" · ");
    elements["home-title"].textContent = "What should the agent work on?";
  } else {
    elements["home-eyebrow"].textContent = isEmpty ? "Connected" : "Remote control";
    elements["home-title"].textContent = isEmpty ? "No sessions yet" : "Pick a session";
  }
  elements["home-pick"].hidden = isEmpty || Boolean(host);
  elements["home-none"].hidden = !isEmpty || Boolean(host);
  elements["home-sessions"].hidden = isEmpty;
  let summary = "";
  if (isAuthenticated) {
    if (!isEmpty) {
      summary = sessionCountText(count);
    } else {
      summary = isMany() ? "no sessions" : "no sessions in this project";
    }
  }
  elements["heading-sub"].replaceChildren(summary);
}

function sessionCountText(count) {
  return count === 1 ? "1 session" : count + " sessions";
}

// Sends a request about the open session, or, without a session, to the
// Agentty in view. A request that names a session goes to the Agentty that
// lists it, so an action that outlasts switching sessions still reaches
// its own.
function sessionRequest(body) {
  const owner = body.session_id ? sessionOwner(body.session_id) : null;
  const instance = owner ?? activeInstance();
  if (!instance) {
    return Promise.reject(new Error("Not connected"));
  }

  return instance.connection.request(body);
}

// The Agentty that lists `sessionId`, preferring the open session's.
function sessionOwner(sessionId) {
  const current = state.current;
  if (current && (current.sessionId === sessionId || current.instance.sessions.some((session) => session.id === sessionId))) {
    return current.instance;
  }

  return (
    state.instances.find((instance) => instance.sessions.some((session) => session.id === sessionId)) ?? null
  );
}

// Requests of home's composer go to the Agentty it describes, which does
// not change while it creates a session.
function homeRequest(body) {
  const instance = state.homeTarget;
  if (!instance) {
    return Promise.reject(new Error("Not connected"));
  }

  return instance.connection.request(body);
}

async function loadSessions(instance) {
  if (!instance.connection.isAuthenticated()) {
    return;
  }
  // A session opened while this runs is loaded by the next run, and an
  // answer for this one is dropped once another is open.
  const sessionId = state.current?.instance === instance ? state.current.sessionId : null;
  const isStillOpen = () => state.current?.instance === instance && state.current.sessionId === sessionId;
  try {
    const listed = await instance.connection.request({ method: "list_sessions" });
    const sessions = listed.sessions || [];
    instance.sessions = sessions;
    instance.isListed = true;
    renderSessions();
    showProject(instance, parseProject(listed));
    instance.view.error.textContent = instance.rejection ?? "";
    if (sessionId) {
      const loaded = await instance.connection.request({ method: "get_session", session_id: sessionId });
      if (isStillOpen()) {
        // The listing carries the figures the header shows beside the
        // snapshot.
        chat.render(loaded.session, sessions.find((summary) => summary.id === sessionId));
      }
    }
  } catch (error) {
    if (sessionId && isStillOpen()) {
      chat.showLoadError(sessionId, error.message);
    } else {
      instance.view.error.textContent = error.message;
    }
  }
}

// Shows the project a listing names. When the terminal switched to another
// project, home describes the host again: new sessions start there.
function showProject(instance, listed) {
  instance.project.update(listed);
  if (
    listed &&
    instance.projectName !== null &&
    listed.name !== instance.projectName &&
    instance === state.homeTarget
  ) {
    void describeHome(true);
  }
  instance.projectName = listed?.name ?? instance.projectName;
}

function renderSessions() {
  sessionList.setSources(
    state.instances.map((instance) => ({
      empty: instance.view.empty,
      id: instance.id,
      isListed: instance.isListed,
      list: instance.view.list,
      name: isMany() ? instance.name : null,
      sessions: instance.sessions,
    })),
  );
  if (!state.current) {
    renderHome();
  }
}

// Opens `sessionId` of `instance` in the main area. The caller has already
// routed to it. `failure` is a first message from home that did not go
// through.
function enterSession(instance, sessionId, failure = null) {
  if (state.current?.instance === instance && state.current.sessionId === sessionId) {
    return;
  }
  state.current = { instance: instance, sessionId: sessionId };
  chat.setCapabilities(instance.connection.capabilities());
  chat.open(sessionId, failure);
  markCurrentRow();
  renderMain();
  renderOffline();
  instance.refreshGate.request(true);
}

// Returns to home. The caller has already routed there.
function leaveSession() {
  const instance = state.current?.instance;
  if (!instance) {
    return;
  }
  state.current = null;
  markCurrentRow();
  renderMain();
  renderOffline();
  instance.refreshGate.request(true);
}

// Marks the open session's row in the list and counts it as seen.
function markCurrentRow() {
  sessionList.setOpen(state.current?.sessionId ?? null, state.current?.instance.id ?? "");
}

// The Agentty a route names: without a relay, or when an older address
// names none, the first one.
function routedInstance(route) {
  return route.instanceId ? findInstance(route.instanceId) : (state.instances[0] ?? null);
}

// The Agentty home describes: the one the user chose with its `New
// session`; otherwise the one home already describes a host for, so a
// draft never moves to another Agentty when connections come and go;
// otherwise the first connected one that offers new sessions, otherwise
// the first one.
function homeInstance() {
  const chosen = state.homeId === null ? null : findInstance(state.homeId);
  if (chosen) {
    return chosen;
  }
  if (state.homeTarget && state.instances.includes(state.homeTarget) && home.host()) {
    return state.homeTarget;
  }

  return (
    state.instances.find(
      (instance) => isConnected(instance) && offersCreation(instance.connection.capabilities()),
    ) ??
    state.instances[0] ??
    null
  );
}

// Points home at `homeInstance()` and describes its host, when it changed
// or `force` says its host may have. A session being created keeps home
// where it is.
async function describeHome(force = false) {
  if (home.isCreating()) {
    return;
  }
  const target = homeInstance();
  if (target === state.homeTarget && !force) {
    return;
  }
  state.homeTarget = target;
  renderOffline();
  await home.setCapabilities(
    isConnected(target) ? target.connection.capabilities() : [],
    target && usesRelay ? target.id : "",
  );
}

// Each Agentty's `New session`: home's shows once its host is described,
// another's whenever it offers new sessions.
function renderNewSessionButtons() {
  for (const instance of state.instances) {
    const isShown =
      instance === state.homeTarget
        ? Boolean(home.host())
        : isConnected(instance) && offersCreation(instance.connection.capabilities());
    instance.view.newSession.hidden = !isShown;
  }
}

// `New session` leads to the home composer, for that Agentty.
function startNewSession(instance) {
  shell.showSession(null);
  leaveSession();
  state.homeId = instance.id;
  if (instance === state.homeTarget && home.host()) {
    home.focus();

    return;
  }
  void describeHome(true).then(() => home.focus());
}

function onAuthenticated(instance, frame) {
  if (!usesRelay && frame.type === "paired") {
    setToken(frame.token);
  }
  const pairing = state.pairing?.instance === instance ? state.pairing : null;
  if (pairing) {
    writeStored(DEVICE_NAME_KEY, pairing.deviceName);
    state.pairing = null;
    state.pairingLink = null;
    if (usesRelay) {
      state.pairings = savePairing(storage, state.pairings, instance.pairing);
    }
    if (!state.instances.includes(instance)) {
      state.instances.push(instance);
      placeInstances();
    }
  }
  instance.rejection = null;
  instance.view.error.textContent = "";
  elements["pair-error"].textContent = "";
  instance.project.setCapabilities(instance.connection.capabilities());
  if (state.current?.instance === instance) {
    chat.setCapabilities(instance.connection.capabilities());
  }
  onConnectionState(instance);
  if (pairing) {
    showShell();
  }
  instance.refreshGate.request(true);
  // Starting a session needs the `host` and `create_session`
  // capabilities; the button appears once the host described its project.
  void describeHome(instance === homeInstance());
}

// A pairing that Agentty refused returns to the pairing form with the
// reason. A credential that can never work again is forgotten; with other
// Agenttys paired, its Agentty stays in the sidebar with the reason until
// the page reloads. Other rejections, such as an unsupported protocol
// version, last until Agentty or the server of this page is updated.
function onRejected(instance, reason, wasPairing) {
  if (wasPairing || state.pairing?.instance === instance) {
    state.pairing = null;
    elements["pair-error"].textContent = reason;
    showPairing();
    if (state.instances.includes(instance) && hasCredential(instance)) {
      instance.connection.connect();
    }

    return;
  }
  if (CREDENTIAL_REJECTIONS.has(reason)) {
    forgetCredential(instance);
    if (!usesRelay || !isMany()) {
      if (usesRelay) {
        removeInstance(instance);
      }
      // A pairing with another Agentty keeps its card as it is.
      if (!state.pairing) {
        elements["pair-error"].textContent = reason;
        showPairing();
      }

      return;
    }
    instance.sessions = [];
  }
  instance.rejection = reason;
  // With one Agentty, the banner alone says why, as on a single page.
  instance.view.error.textContent = isMany() ? reason : "";
  renderSessions();
  renderOffline();
}

// The connection ended before Agentty answered the pairing.
function onPairingLost(instance) {
  if (state.pairing?.instance !== instance) {
    return;
  }
  state.pairing = null;
  setPairingWait(null);
  elements["pair-error"].textContent =
    "Could not reach Agentty. Check that remote control is on, then try again.";
}

// Forgets the credential `instance` connects with.
function forgetCredential(instance) {
  if (usesRelay) {
    state.pairings = forgetPairing(storage, state.pairings, instance.id);
  } else {
    setToken(null);
  }
  if (!usesRelay || state.pairings.length === 0) {
    writeStored(DEVICE_NAME_KEY, null);
  }
}

// Takes `instance` out of the sidebar, leaving its open session.
function removeInstance(instance) {
  instance.connection.disconnect();
  if (state.current?.instance === instance) {
    shell.showSession(null);
    leaveSession();
  }
  state.instances = state.instances.filter((known) => known !== instance);
  if (state.homeId === instance.id) {
    state.homeId = null;
  }
  placeInstances();
  void describeHome();
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
  request: sessionRequest,
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
    request: sessionRequest,
  }),
  document: document,
  graph: createGraph({
    document: document,
    elements: {
      actions: elements["graph-actions"],
      cancelDialog: elements["graph-cancel-dialog"],
      detail: elements["graph-detail"],
      error: elements["graph-error"],
      hints: elements["graph-hints"],
      next: elements["graph-next"],
      nodes: elements["graph-nodes"],
      note: elements["graph-note"],
      position: elements["graph-position"],
      prev: elements["graph-prev"],
      preview: elements["graph-preview"],
      refresh: elements["graph-refresh"],
      title: elements["graph-title"],
      view: elements["graph-view"],
    },
    request: sessionRequest,
    window: window,
  }),
  elements: {
    actionError: elements["action-error"],
    actionNote: elements["action-note"],
    answerBar: elements["answer-bar"],
    answerChat: elements["answer-chat"],
    answerCount: elements["answer-count"],
    answerError: elements["answer-error"],
    answerSend: elements["answer-send"],
    badge: elements["session-status"],
    cancel: elements.cancel,
    cancelDialog: elements["cancel-dialog"],
    composer: elements.composer,
    composerGraph: elements["composer-graph"],
    composerGraphLeave: elements["composer-graph-leave"],
    composerGraphNote: elements["composer-graph-note"],
    composerHint: elements["composer-hint"],
    composerMeta: elements["composer-meta"],
    composerSettings: elements["composer-settings"],
    diff: elements.diff,
    dock: elements.dock,
    draftPreview: elements["draft-preview"],
    focusedReview: elements["focused-review"],
    gone: elements["session-gone"],
    graphSummary: elements["graph-summary"],
    graphs: elements.graphs,
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
    publishWait: elements["publish-wait"],
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
    sheetGraphs: elements["sheet-graphs"],
    sheetGraphsLabel: elements["sheet-graphs-label"],
    sheetMerge: elements["sheet-merge"],
    sheetMergeLabel: elements["sheet-merge-label"],
    sheetMergeNote: elements["sheet-merge-note"],
    sheetPublish: elements["sheet-publish"],
    sheetPublishNote: elements["sheet-publish-note"],
    sheetDetails: elements["sheet-details"],
    sheetSub: elements["sheet-sub"],
    sheetSync: elements["sheet-sync"],
    sheetSyncLabel: elements["sheet-sync-label"],
    sheetTitle: elements["sheet-title"],
    statusLine: elements["status-line"],
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
  // An action that changed the open session reloads its Agentty.
  onRefresh: () => state.current?.instance.refreshGate.request(true),
  request: sessionRequest,
  settings: settingsSheet,
  window: window,
});

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

      return;
    }
    const instance = route.kind === "session" ? routedInstance(route) : null;
    if (instance) {
      enterSession(instance, route.sessionId);
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
    filter: elements.filter,
    filterKey: elements["filter-key"],
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
  onOpen: (sessionId, instanceId) => {
    const instance = findInstance(instanceId);
    if (!instance) {
      return;
    }
    shell.showSession(sessionId, routeId(instance));
    enterSession(instance, sessionId);
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
    const instance = state.homeTarget;
    if (state.current === null) {
      shell.showSession(sessionId, routeId(instance));
      enterSession(instance, sessionId, failure);
    } else {
      if (failure) {
        chat.keepDraft(sessionId, failure.draft);
      }
      instance.refreshGate.request(true);
    }
  },
  onHost: (host) => {
    state.homeTarget?.project.setHostName(host?.project ?? null);
    renderNewSessionButtons();
    if (!state.current) {
      renderHome();
    }
  },
  request: homeRequest,
  settings: settingsSheet,
  window: window,
});

elements["pair-name"].value = readStored(DEVICE_NAME_KEY) || defaultDeviceName();

elements["pair-form"].addEventListener("submit", (event) => {
  event.preventDefault();
  const code = elements["pair-code"].value.trim();
  if (!code || state.pairing) {
    return;
  }
  const link = state.pairingLink;
  if (usesRelay && (!link || !state.deviceKey)) {
    return;
  }
  const deviceName = elements["pair-name"].value.trim();
  elements["pair-error"].textContent = "";
  // Cancel shows at once, so a connection that hangs can be abandoned.
  setPairingPhase("connecting");
  // The pairing counts from here, so Cancel works before its Agentty is
  // known.
  const pairing = { deviceName: deviceName, instance: null };
  state.pairing = pairing;
  void pairingInstance(link).then((instance) => {
    if (state.pairing !== pairing) {
      return;
    }
    // A pairing with an Agentty this browser already knows replaces it
    // once Agentty approves; until then the earlier one is not used to
    // reconnect.
    pairing.instance = instance;
    instance.rejection = null;
    instance.connection.pair({
      type: "pair",
      code: code,
      device_name: deviceName,
      protocol_version: PROTOCOL_VERSION,
    });
  });
});

// The Agentty `link` pairs with, already in the sidebar or new; without a
// relay, the one behind this page.
async function pairingInstance(link) {
  if (!usesRelay) {
    return state.instances[0];
  }
  const instanceId = await instanceIdOf(link.instanceKey);
  const pairing = { instanceId: instanceId, instanceKey: link.instanceKey, relayUrl: link.relayUrl };
  const known = findInstance(instanceId);
  if (known) {
    known.pairing = pairing;

    return known;
  }

  return createInstance(instanceId, pairing);
}

// Stops waiting for approval. Agentty drops the request once the
// connection closes; an Agentty that was paired before reconnects.
elements["pair-cancel"].addEventListener("click", () => {
  const instance = state.pairing?.instance;
  state.pairing = null;
  instance?.connection.disconnect();
  if (instance && state.instances.includes(instance) && hasCredential(instance)) {
    instance.connection.connect();
  }
  setPairingWait(null);
});

// Returns from a pairing link to the Agenttys already paired.
elements["pair-back"].addEventListener("click", () => {
  state.pairingLink = null;
  elements["pair-error"].textContent = "";
  showShell();
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

// Asks before forgetting this device on `instance`.
function askToForget(instance) {
  setDeviceMenu(false);
  state.forgetTarget = instance;
  elements["forget-target"].textContent = isMany() ? " to " + instance.name : "";
  elements["forget-dialog"].returnValue = "";
  elements["forget-dialog"].showModal();
}

elements.forget.addEventListener("click", () => {
  if (state.instances[0]) {
    askToForget(state.instances[0]);
  }
});

elements["forget-dialog"].addEventListener("close", () => {
  const instance = state.forgetTarget;
  state.forgetTarget = null;
  if (elements["forget-dialog"].returnValue !== "confirm" || !instance) {
    return;
  }
  forgetCredential(instance);
  if (!usesRelay) {
    instance.connection.disconnect();
    elements["pair-name"].value = defaultDeviceName();
    showPairing();

    return;
  }
  removeInstance(instance);
  if (state.instances.length > 0) {
    return;
  }
  // Agentty lists the old key until it is removed there; a later pairing
  // uses a new one. Other Agenttys still know the key, so it stays while
  // any is paired.
  state.deviceKey = null;
  void forgetDeviceKey()
    .then(loadDeviceKey)
    .then((deviceKey) => {
      state.deviceKey = deviceKey.keyPair;
    });
  elements["pair-name"].value = defaultDeviceName();
  showPairing();
});

elements["home-sessions"].addEventListener("click", () => elements.menu.click());

elements.retry.addEventListener("click", () => {
  const active = activeInstance();
  if (active && hasCredential(active)) {
    active.connection.connect();
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
  const instance = route.kind === "session" ? routedInstance(route) : null;
  if (instance) {
    state.current = { instance: instance, sessionId: route.sessionId };
    chat.open(route.sessionId);
    markCurrentRow();
  } else {
    clearSessionRoute();
  }
}

// Shows the app and connects every Agentty with its saved credential.
function resume() {
  adoptSessionRoute();
  showShell();
  void describeHome();
  for (const instance of state.instances) {
    instance.connection.connect();
  }
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
  state.pairings = await loadPairings(storage);
  // A link opened in a tab that already shows this page only changes the
  // fragment. It replaces whatever this tab was doing; the Agenttys already
  // paired stay connected behind the pairing card.
  window.addEventListener("hashchange", () => {
    if (adoptPairingLink()) {
      const instance = state.pairing?.instance;
      state.pairing = null;
      instance?.connection.disconnect();
      if (instance && state.instances.includes(instance) && hasCredential(instance)) {
        instance.connection.connect();
      }
      elements["pair-error"].textContent = "";
      showPairing();
    }
  });
  const hasLink = adoptPairingLink();
  if (state.pairings.length > 0 && deviceKey.origin !== "saved") {
    // The key Agentty knows is not available, so connecting would only be
    // refused. A key that is gone ends the pairings; storage that could not
    // be read may work again after a reload, so the pairings stay.
    if (deviceKey.origin === "created") {
      for (const pairing of state.pairings) {
        state.pairings = forgetPairing(storage, state.pairings, pairing.instanceId);
      }
    } else if (!hasLink) {
      elements["pair-error"].textContent =
        "This browser's saved key could not be read. Reload the page to try again.";
    }
    showPairing();

    return;
  }
  state.instances = state.pairings.map((pairing) => createInstance(pairing.instanceId, pairing));
  placeInstances();
  if (hasLink || state.instances.length === 0) {
    showPairing();
    void describeHome();
    for (const instance of state.instances) {
      instance.connection.connect();
    }

    return;
  }
  resume();
}

if (usesRelay) {
  await startThroughRelay();
} else {
  state.instances = [createInstance("", null)];
  placeInstances();
  if (token()) {
    resume();
  } else {
    showPairing();
  }
}
