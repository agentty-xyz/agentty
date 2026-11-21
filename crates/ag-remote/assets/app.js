"use strict";

// Agentty remote control client. Speaks protocol version 1 over one
// WebSocket at /ws; see crates/ag-remote/src/protocol.rs for the frames.

(function () {
  const PROTOCOL_VERSION = 1;
  const TOKEN_KEY = "agentty.remote.token";
  const MAX_BACKOFF_MS = 30000;
  // Rejections that mean the stored token can never work again.
  const TOKEN_REJECTIONS = new Set(["Unknown device token", "Device revoked"]);

  const elements = {};
  for (const id of [
    "back", "heading", "connection", "pair-view", "pair-form", "pair-code",
    "pair-name", "pair-error", "list-view", "sessions", "sessions-empty",
    "forget", "session-view", "session-status", "cancel", "transcript",
    "queued", "questions", "composer", "message", "session-error",
  ]) {
    elements[id] = document.getElementById(id);
  }

  const state = {
    backoffMs: 1000,
    currentSessionId: null,
    isAuthenticated: false,
    isRefreshing: false,
    nextRequestId: 1,
    pendingPairing: null,
    pending: new Map(),
    reconnectTimer: null,
    refreshAgain: false,
    session: null,
    socket: null,
    stopReconnecting: false,
  };

  function readToken() {
    try {
      return window.localStorage.getItem(TOKEN_KEY);
    } catch (_error) {
      return null;
    }
  }

  function writeToken(token) {
    try {
      if (token) {
        window.localStorage.setItem(TOKEN_KEY, token);
      } else {
        window.localStorage.removeItem(TOKEN_KEY);
      }
    } catch (_error) {
      // Storage can be unavailable in private windows; pairing then lasts
      // for this page only.
    }
  }

  let memoryToken = readToken();

  function token() {
    return memoryToken;
  }

  function setToken(value) {
    memoryToken = value;
    writeToken(value);
  }

  function defaultDeviceName() {
    const platform =
      (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
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
    if (state.socket && state.socket.readyState === WebSocket.OPEN) {
      state.socket.send(JSON.stringify(frame));

      return true;
    }

    return false;
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

  function connect(firstFrame) {
    window.clearTimeout(state.reconnectTimer);
    if (state.socket) {
      state.socket.onclose = null;
      state.socket.close();
    }
    state.stopReconnecting = false;
    state.isAuthenticated = false;
    setConnection("Connecting", false);

    const socket = new WebSocket(socketUrl());
    state.socket = socket;
    socket.onopen = () => socket.send(JSON.stringify(firstFrame));
    socket.onmessage = (event) => handleFrame(event.data);
    socket.onclose = () => handleClose(socket);
  }

  function handleFrame(data) {
    let frame;
    try {
      frame = JSON.parse(data);
    } catch (_error) {
      return;
    }

    switch (frame.type) {
      case "paired":
        setToken(frame.token);
        state.pendingPairing = null;
        onAuthenticated();
        break;
      case "welcome":
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
          pending.reject(new Error((frame.result && frame.result.error) || "Request failed"));
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
    state.isAuthenticated = true;
    state.backoffMs = 1000;
    setConnection("Connected", true);
    elements["pair-error"].textContent = "";
    if (!state.currentSessionId) {
      show("list");
    }
    refresh();
  }

  function onRejected(reason) {
    state.stopReconnecting = true;
    if (state.pendingPairing || TOKEN_REJECTIONS.has(reason)) {
      if (TOKEN_REJECTIONS.has(reason)) {
        setToken(null);
      }
      state.currentSessionId = null;
      elements["pair-error"].textContent = reason;
      show("pair");
    } else {
      // Other rejections, such as an unsupported protocol version, need a
      // page reload after Agentty is updated.
      elements["session-error"].textContent = reason;
    }
    setConnection("Disconnected", false);
  }

  function handleClose(socket) {
    if (state.socket !== socket) {
      return;
    }
    state.isAuthenticated = false;
    state.socket = null;
    failPending("Disconnected");
    setConnection("Offline", false);
    if (state.stopReconnecting || !token()) {
      return;
    }
    const delay = state.backoffMs;
    state.backoffMs = Math.min(state.backoffMs * 2, MAX_BACKOFF_MS);
    setConnection("Reconnecting", false);
    state.reconnectTimer = window.setTimeout(() => {
      connect({ type: "hello", protocol_version: PROTOCOL_VERSION, token: token() });
    }, delay);
  }

  async function refresh() {
    if (state.isRefreshing) {
      state.refreshAgain = true;

      return;
    }
    state.isRefreshing = true;
    try {
      do {
        state.refreshAgain = false;
        const listed = await request({ method: "list_sessions" });
        renderSessions(listed.sessions || []);
        if (state.currentSessionId) {
          const loaded = await request({
            method: "get_session",
            session_id: state.currentSessionId,
          });
          renderSession(loaded.session);
        }
      } while (state.refreshAgain);
    } catch (error) {
      elements["session-error"].textContent = error.message;
    } finally {
      state.isRefreshing = false;
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
    elements.cancel.hidden = session.status !== "InProgress";

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
    elements["pair-error"].textContent = "";
    state.pendingPairing = code;
    connect({
      type: "pair",
      code: code,
      device_name: elements["pair-name"].value.trim(),
      protocol_version: PROTOCOL_VERSION,
    });
  });

  elements.forget.addEventListener("click", () => {
    setToken(null);
    state.stopReconnecting = true;
    state.currentSessionId = null;
    if (state.socket) {
      state.socket.close();
    }
    show("pair");
  });

  elements.back.addEventListener("click", closeSession);

  elements.cancel.addEventListener("click", async () => {
    if (!state.currentSessionId) {
      return;
    }
    elements.cancel.disabled = true;
    try {
      await request({ method: "cancel_session", session_id: state.currentSessionId });
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

  if (token()) {
    show("list");
    connect({ type: "hello", protocol_version: PROTOCOL_VERSION, token: token() });
  } else {
    show("pair");
  }
})();
