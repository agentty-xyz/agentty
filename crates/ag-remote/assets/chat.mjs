// Open session of the remote control client: the header with its status
// and actions, the transcript, the questions panel, the queued messages,
// and the composer. Transcript text, titles and questions are untrusted
// and only ever become text nodes. Updates change the nodes in place, so a
// refresh keeps focus, scroll position and what the user is typing.
// Functions take `document` and `window` as parameters, so the Node tests
// in crates/ag-remote/tests/web/chat.test.mjs pass the fake DOM.

import { parseActivity, parseNotice, statusInfo } from "./format.mjs";

// Unsent text of one session, in `sessionStorage`, under this prefix and
// the session id.
export const DRAFT_KEY_PREFIX = "agentty.remote.draft.";
// Shortest time between two refreshes.
export const REFRESH_INTERVAL_MS = 1000;
// How close to the end of the transcript the reader must be for new
// content to scroll into view.
export const FOLLOW_DISTANCE_PX = 80;
// Most rows the composer grows to.
export const MAX_COMPOSER_ROWS = 8;
// An activity box with more lines than this starts closed.
const OPEN_ACTIVITY_LINES = 3;
const MESSAGE_KINDS = new Set(["activity", "assistant", "notice", "user"]);
// Statuses in which a message waits for the running turn.
const QUEUEING_STATUSES = new Set(["InProgress", "Rebasing"]);
// What a read-only status says instead of the composer, when its label
// alone would not.
const READ_ONLY_LABELS = { Queued: "Queued to merge" };

// The kind a message renders as. A kind this client does not know, from a
// newer Agentty, renders as a notice.
export function messageKind(message) {
  return MESSAGE_KINDS.has(message?.kind) ? message.kind : "notice";
}

// Compares the transcript on screen, as `{ key, content }` entries, with
// the next one, and returns the indexes to replace, the indexes to append,
// and how many trailing entries to remove. Entries are keyed by position
// and kind, so a growing transcript only appends.
export function transcriptChanges(previous, next) {
  const replace = [];
  const append = [];
  next.forEach((entry, index) => {
    const old = previous[index];
    if (!old) {
      append.push(index);
    } else if (old.key !== entry.key || old.content !== entry.content) {
      replace.push(index);
    }
  });

  return { append, remove: Math.max(0, previous.length - next.length), replace };
}

// The entries `transcriptChanges` compares, for `messages`.
export function transcriptEntries(messages) {
  return messages.map((message, index) => ({
    content: String(message.content ?? ""),
    key: index + ":" + messageKind(message),
  }));
}

// Decides what the dock shows for a session in `statusName`: `compose`
// with a placeholder and a hint, `answer` while questions wait, or
// `readonly` with the reason. `capabilities` gates `Stop`; `isFinePointer`
// picks the key hint.
export function composerState(
  statusName,
  { capabilities = [], hasQuestions = false, isConnected = true, isFinePointer = true } = {},
) {
  const info = statusInfo(statusName);
  if (statusName === "Question" && hasQuestions) {
    return { mode: "answer" };
  }
  if (!info.composer) {
    const label = READ_ONLY_LABELS[statusName] || info.label;

    return { mode: "readonly", text: label + ". Chat is read-only." };
  }
  const isQueueing = QUEUEING_STATUSES.has(statusName);
  const showStop = statusName === "InProgress" && capabilities.includes("stop_turn");
  let placeholder = statusName === "Draft" ? "Describe the task" : "Reply to the agent";
  let hint = isFinePointer ? "Enter sends · Shift+Enter adds a line" : "Enter adds a line";
  if (isQueueing) {
    placeholder = "Queue a message for the next turn";
    hint = (isFinePointer ? "Enter queues the message" : "Sending queues the message") +
      (showStop ? " · Stop returns the session to Review" : "");
  }
  if (!isConnected) {
    placeholder = "Offline. Your draft is kept.";
  }

  return { hint, isQueueing, mode: "compose", placeholder, showStop };
}

// What a key press in the composer does: `send`, or `null` to let the
// browser add a line. With a fine pointer `Enter` sends and `Shift+Enter`
// adds a line; on touch `Enter` adds a line. `Ctrl+Enter` and `Cmd+Enter`
// always send. Nothing sends while an input method is composing.
export function composerKeyAction(event, { isFinePointer }) {
  if (event.key !== "Enter" || event.isComposing || event.keyCode === 229) {
    return null;
  }
  if (event.ctrlKey || event.metaKey) {
    return "send";
  }

  return isFinePointer && !event.shiftKey && !event.altKey ? "send" : null;
}

// Rows the composer shows for `text`: one per line, from 1 to
// `MAX_COMPOSER_ROWS`.
export function composerRows(text) {
  return Math.min(MAX_COMPOSER_ROWS, Math.max(1, String(text).split("\n").length));
}

// Collects the answers to `questions`: typed text wins over the chosen
// option in `picks`. Returns the answers, how many questions have one, and
// whether the set is complete.
export function collectAnswers(questions, picks, typed) {
  const answers = questions.map((question, index) => ({
    answer: (typed[index] || "").trim() || picks[index] || "",
    question: question.text,
  }));
  const answered = answers.filter((answer) => answer.answer !== "").length;

  return {
    answered,
    answers,
    isComplete: questions.length > 0 && answered === questions.length,
  };
}

// Which header actions a session in `statusName` shows, and which of them
// are enabled. Merge appears only with the `merge` capability.
export function sessionActions(statusName, capabilities = []) {
  const info = statusInfo(statusName);

  return {
    cancel: info.cancel,
    merge: capabilities.includes("merge"),
    mergeEnabled: capabilities.includes("merge") && info.review,
  };
}

// The branch a merge lands on, or `null` when the host does not say.
export function mergeTarget(session) {
  const branch = session?.base_branch;

  return typeof branch === "string" && branch !== "" ? branch : null;
}

// Runs `run` at most once per `intervalMs`, one run at a time, and never
// while the page is hidden. Requests that arrive meanwhile collapse into
// one later run; a hidden page runs it once it shows again. `run` handles
// its own errors. `request(true)` skips the wait, as when a session opens.
export function createRefreshGate({
  document,
  window,
  run,
  intervalMs = REFRESH_INTERVAL_MS,
  now = () => Date.now(),
}) {
  const gate = { isPending: false, isRunning: false, lastStart: -Infinity, timer: null };

  function start() {
    gate.isPending = false;
    gate.isRunning = true;
    gate.lastStart = now();
    void Promise.resolve()
      .then(run)
      .finally(() => {
        gate.isRunning = false;
        if (gate.isPending) {
          request(false);
        }
      });
  }

  function request(force = false) {
    gate.isPending = true;
    if (force && gate.timer !== null) {
      window.clearTimeout(gate.timer);
      gate.timer = null;
    }
    if (gate.isRunning || gate.timer !== null || document.visibilityState === "hidden") {
      return;
    }
    const wait = force ? 0 : gate.lastStart + intervalMs - now();
    if (wait <= 0) {
      start();

      return;
    }
    gate.timer = window.setTimeout(() => {
      gate.timer = null;
      request(false);
    }, wait);
  }

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "hidden" && gate.isPending) {
      request(false);
    }
  });

  return { request };
}

// Unsent text per session, in memory and in `sessionStorage`, so a draft
// survives switching sessions and reloading the tab. Storage failures
// leave the copy in memory.
export function createDrafts(window) {
  const drafts = new Map();

  return {
    clear: (sessionId) => {
      drafts.delete(sessionId);
      try {
        window.sessionStorage.removeItem(DRAFT_KEY_PREFIX + sessionId);
      } catch (_error) {
        // Only the copy in memory existed.
      }
    },
    get: (sessionId) => {
      if (drafts.has(sessionId)) {
        return drafts.get(sessionId);
      }
      try {
        return window.sessionStorage.getItem(DRAFT_KEY_PREFIX + sessionId) || "";
      } catch (_error) {
        return "";
      }
    },
    set: (sessionId, text) => {
      drafts.set(sessionId, text);
      try {
        if (text) {
          window.sessionStorage.setItem(DRAFT_KEY_PREFIX + sessionId, text);
        } else {
          window.sessionStorage.removeItem(DRAFT_KEY_PREFIX + sessionId);
        }
      } catch (_error) {
        // The copy in memory still lasts for this page.
      }
    },
  };
}

// Wires the open session. `elements` holds the nodes with the ids named in
// index.html, keyed as in `app.js`. `request(body)` sends one request and
// resolves with its result; `onHome()` returns to home.
//
// Returns `open(sessionId)`, `close()`, `render(session)` for a snapshot
// (`null` when the session is gone), `setConnected(isConnected)`,
// `setCapabilities(list)`, and `showError(message)`.
export function createChat({ document, window, elements, request, onHome }) {
  const drafts = createDrafts(window);
  const state = {
    capabilities: [],
    isAnswering: false,
    isCanceling: false,
    isConnected: false,
    isMerging: false,
    isSending: false,
    isStopping: false,
    picks: [],
    questions: [],
    questionsKey: null,
    rendered: [],
    session: null,
    sessionId: null,
  };

  function isFinePointer() {
    return Boolean(window.matchMedia?.("(pointer: fine)").matches);
  }

  function isNearEnd() {
    const scroller = elements.scroller;

    return scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - FOLLOW_DISTANCE_PX;
  }

  function scrollToEnd() {
    elements.scroller.scrollTop = elements.scroller.scrollHeight;
    elements.jump.hidden = true;
  }

  function renderHeader(session) {
    elements.heading.textContent = session.title || "No title";
    const info = statusInfo(session.status);
    elements.badge.hidden = false;
    elements.badge.className = "status status-badge wide-only status--" + info.tone;
    elements.badge.replaceChildren(element(document, "i"), info.label);
    const line = element(document, "span", "status narrow-only status--" + info.tone);
    line.append(element(document, "i"), info.label);
    elements.sub.replaceChildren(line);
    elements.sheetTitle.textContent = session.title || "No title";
    elements.sheetSub.textContent = info.label;
    renderActions();
  }

  // Shows the actions the status and the host allow. Merge sits in the
  // header on wide screens and in the sheet on phones; the sheet holds
  // `Cancel session` everywhere.
  function renderActions() {
    const actions = state.session
      ? sessionActions(state.session.status, state.capabilities)
      : { cancel: false, merge: false, mergeEnabled: false };
    elements.merge.hidden = !actions.merge;
    elements.merge.disabled = !actions.mergeEnabled || state.isMerging || !state.isConnected;
    elements.sheetMerge.hidden = !actions.merge;
    elements.sheetMerge.disabled = elements.merge.disabled;
    elements.cancel.hidden = !actions.cancel;
    elements.cancel.disabled = state.isCanceling || !state.isConnected;
    elements.more.hidden = !actions.cancel && !actions.merge;
    // With only Merge in it, the sheet adds nothing to the wide header.
    elements.more.classList.toggle("narrow-only", !actions.cancel);
    const target = mergeTarget(state.session);
    elements.sheetMergeLabel.textContent = target ? "Merge into " + target : "Merge";
    elements.mergeTarget.textContent = target || "its base branch";
  }

  function renderTranscript(messages) {
    const next = transcriptEntries(messages);
    const changes = transcriptChanges(state.rendered, next);
    const children = [...elements.transcript.children];
    for (const index of changes.replace) {
      const node = renderMessage(messages[index]);
      keepActivityOpen(children[index], node);
      children[index].replaceWith(node);
    }
    for (const index of changes.append) {
      elements.transcript.append(renderMessage(messages[index]));
    }
    for (const child of children.slice(next.length)) {
      child.remove();
    }
    state.rendered = next;

    return changes.append.length + changes.replace.length + changes.remove > 0;
  }

  function renderMessage(message) {
    const kind = messageKind(message);
    const content = String(message.content ?? "");
    const item = element(document, "li", "msg msg--" + kind);
    item.dataset.kind = kind;
    if (kind === "activity") {
      item.append(renderActivity(content));
    } else if (kind === "notice") {
      const notice = parseNotice(content);
      item.classList.toggle("is-error", notice.tone === "error");
      item.classList.toggle("is-warning", notice.tone === "warning");
      if (notice.label) {
        item.append(element(document, "span", "notice-label", notice.label));
      }
      item.append(element(document, "span", "notice-body", notice.body));
    } else {
      item.textContent = content;
    }

    return item;
  }

  function renderActivity(content) {
    const lines = parseActivity(content);
    const details = element(document, "details");
    details.open = lines.length <= OPEN_ACTIVITY_LINES;
    const summary = element(document, "summary", null, "Activity");
    summary.append(
      element(document, "span", "count", lines.length === 1 ? "1 line" : lines.length + " lines"),
    );
    const list = element(document, "ul", "activity-lines");
    list.append(
      ...lines.map((line) => {
        const row = element(document, "li", "tone-" + line.tone, line.text);
        row.title = line.text;

        return row;
      }),
    );
    details.append(summary, list);

    return details;
  }

  // Rebuilds the questions only when the set changes, so chosen options
  // and typed answers survive refreshes.
  function renderQuestions(questions) {
    const key = JSON.stringify(questions);
    if (key === state.questionsKey) {
      return false;
    }
    state.questionsKey = key;
    state.questions = questions;
    state.picks = questions.map(() => null);
    elements.answerError.textContent = "";
    elements.questions.hidden = questions.length === 0;
    elements.questions.replaceChildren(
      ...questions.map((question, index) => renderQuestion(question, index, questions.length)),
    );

    return true;
  }

  function renderQuestion(question, index, total) {
    const group = element(document, "div", "question");
    group.dataset.question = question.text;
    const options = question.options || [];
    group.append(
      element(document, "p", "eyebrow", "Question " + (index + 1) + " of " + total),
      element(document, "p", "question-text", question.text),
    );
    if (options.length > 0) {
      const list = element(document, "div", "options");
      options.forEach((option, optionIndex) => {
        const button = element(document, "button", "option", option);
        button.type = "button";
        button.setAttribute("aria-pressed", "false");
        if (optionIndex === 0) {
          // The protocol lists the recommended answer first.
          button.append(element(document, "small", null, "recommended"));
        }
        button.addEventListener("click", () => {
          state.picks[index] = option;
          for (const sibling of list.children) {
            sibling.setAttribute("aria-pressed", String(sibling === button));
          }
          renderAnswerBar();
        });
        list.append(button);
      });
      group.append(list);
    }
    const input = element(document, "input", "field");
    input.placeholder = options.length > 0 ? "Or type an answer" : "Your answer";
    input.setAttribute("aria-label", "Answer " + (index + 1));
    input.addEventListener("input", renderAnswerBar);
    group.append(input);

    return group;
  }

  function typedAnswers() {
    return [...elements.questions.querySelectorAll("input")].map((input) => input.value);
  }

  function currentAnswers() {
    return collectAnswers(state.questions, state.picks, typedAnswers());
  }

  function renderAnswerBar() {
    const collected = currentAnswers();
    elements.answerCount.textContent =
      collected.answered + " of " + state.questions.length + " answered";
    elements.answerSend.disabled = !collected.isComplete || state.isAnswering || !state.isConnected;
  }

  function renderQueued(queued) {
    elements.queuedBox.hidden = queued.length === 0;
    const items = [...elements.queued.children];
    queued.forEach((text, index) => {
      if (items[index]?.title === text) {
        return;
      }
      const item = element(document, "li");
      item.title = text;
      item.append(element(document, "span", null, text));
      if (items[index]) {
        items[index].replaceWith(item);
      } else {
        elements.queued.append(item);
      }
    });
    for (const item of items.slice(queued.length)) {
      item.remove();
    }
  }

  function renderDock() {
    const session = state.session;
    if (!session) {
      return;
    }
    const dock = composerState(session.status, {
      capabilities: state.capabilities,
      hasQuestions: state.questions.length > 0,
      isConnected: state.isConnected,
      isFinePointer: isFinePointer(),
    });
    elements.composer.hidden = dock.mode !== "compose";
    elements.answerBar.hidden = dock.mode !== "answer";
    elements.readonly.hidden = dock.mode !== "readonly";
    if (dock.mode === "readonly") {
      elements.readonly.textContent = dock.text;
    }
    if (dock.mode === "answer") {
      renderAnswerBar();
    }
    if (dock.mode !== "compose") {
      return;
    }
    elements.message.placeholder = dock.placeholder;
    elements.composerMeta.textContent = dock.hint;
    elements.send.disabled = !state.isConnected || state.isSending;
    elements.send.setAttribute("aria-label", dock.isQueueing ? "Queue message" : "Send");
    elements.stop.hidden = !dock.showStop;
    elements.stop.disabled = !state.isConnected || state.isStopping;
  }

  function resizeComposer() {
    elements.message.rows = composerRows(elements.message.value);
  }

  function reset() {
    state.session = null;
    state.rendered = [];
    state.questions = [];
    state.questionsKey = null;
    state.picks = [];
    elements.transcript.replaceChildren();
    elements.queued.replaceChildren();
    elements.queuedBox.hidden = true;
    elements.questions.replaceChildren();
    elements.questions.hidden = true;
    elements.jump.hidden = true;
    elements.gone.hidden = true;
    elements.scroller.hidden = false;
    elements.composer.hidden = false;
    elements.answerBar.hidden = true;
    elements.readonly.hidden = true;
    elements.stop.hidden = true;
    elements.sessionError.textContent = "";
    elements.answerError.textContent = "";
    elements.actionError.textContent = "";
    elements.badge.hidden = true;
    elements.sub.replaceChildren();
    elements.heading.textContent = "";
    if (elements.sheet.open) {
      elements.sheet.close();
    }
    renderActions();
  }

  function open(sessionId) {
    reset();
    state.sessionId = sessionId;
    elements.message.value = drafts.get(sessionId);
    resizeComposer();
  }

  function close() {
    reset();
    state.sessionId = null;
  }

  function render(session) {
    if (!session) {
      reset();
      elements.heading.textContent = "Session not found";
      elements.gone.hidden = false;
      elements.scroller.hidden = true;
      elements.composer.hidden = true;

      return;
    }
    const isFirst = state.session === null;
    const wasNearEnd = isNearEnd();
    state.session = session;
    elements.gone.hidden = true;
    elements.scroller.hidden = false;
    renderHeader(session);
    const transcriptChanged = renderTranscript(session.messages || []);
    const questionsChanged = renderQuestions(session.questions || []);
    renderQueued(session.queued_messages || []);
    renderDock();
    if (isFirst || (wasNearEnd && (transcriptChanged || questionsChanged))) {
      scrollToEnd();
    } else if (transcriptChanged || questionsChanged) {
      elements.jump.hidden = false;
    }
  }

  async function sendMessage() {
    const sessionId = state.sessionId;
    const message = elements.message.value;
    if (!message.trim() || !sessionId || state.isSending || !state.isConnected) {
      return;
    }
    state.isSending = true;
    renderDock();
    try {
      await request({ method: "send_message", session_id: sessionId, message: message });
      drafts.clear(sessionId);
      if (state.sessionId === sessionId) {
        if (elements.message.value === message) {
          elements.message.value = "";
          resizeComposer();
        } else {
          drafts.set(sessionId, elements.message.value);
        }
        elements.sessionError.textContent = "";
      }
    } catch (error) {
      if (state.sessionId === sessionId) {
        elements.sessionError.textContent = error.message;
      }
    } finally {
      state.isSending = false;
      renderDock();
    }
  }

  async function sendAnswers() {
    const sessionId = state.sessionId;
    const collected = currentAnswers();
    if (!sessionId || state.isAnswering) {
      return;
    }
    if (!collected.isComplete) {
      elements.answerError.textContent = "Answer every question first.";

      return;
    }
    state.isAnswering = true;
    renderAnswerBar();
    try {
      await request({
        method: "answer_questions",
        session_id: sessionId,
        answers: collected.answers,
      });
      elements.answerError.textContent = "";
    } catch (error) {
      if (state.sessionId === sessionId) {
        elements.answerError.textContent = error.message;
      }
    } finally {
      state.isAnswering = false;
      renderAnswerBar();
    }
  }

  // Runs one session action and shows its error under the header.
  async function runAction(flag, body, errorNode) {
    const sessionId = state.sessionId;
    if (!sessionId || state[flag]) {
      return;
    }
    state[flag] = true;
    renderActions();
    renderDock();
    try {
      await request({ ...body, session_id: sessionId });
      errorNode.textContent = "";
    } catch (error) {
      if (state.sessionId === sessionId) {
        errorNode.textContent = error.message;
      }
    } finally {
      state[flag] = false;
      renderActions();
      renderDock();
    }
  }

  function confirmWith(dialog) {
    if (!state.sessionId) {
      return;
    }
    dialog.returnValue = "";
    dialog.showModal();
  }

  elements.message.addEventListener("input", () => {
    if (state.sessionId) {
      drafts.set(state.sessionId, elements.message.value);
    }
    resizeComposer();
  });
  elements.message.addEventListener("keydown", (event) => {
    if (composerKeyAction(event, { isFinePointer: isFinePointer() }) === "send") {
      event.preventDefault();
      void sendMessage();
    }
  });
  elements.composer.addEventListener("submit", (event) => {
    event.preventDefault();
    void sendMessage();
  });
  elements.stop.addEventListener("click", () => {
    void runAction("isStopping", { method: "stop_turn" }, elements.sessionError);
  });
  elements.questions.addEventListener("submit", (event) => {
    event.preventDefault();
    void sendAnswers();
  });
  elements.answerSend.addEventListener("click", () => void sendAnswers());
  elements.scroller.addEventListener("scroll", () => {
    if (isNearEnd()) {
      elements.jump.hidden = true;
    }
  });
  elements.jump.addEventListener("click", scrollToEnd);
  elements.goneHome.addEventListener("click", () => onHome());
  elements.more.addEventListener("click", () => {
    elements.sheet.returnValue = "";
    elements.sheet.showModal();
  });
  // A tap beside the sheet, on its backdrop, closes it.
  elements.sheet.addEventListener("click", (event) => {
    if (event.target === elements.sheet) {
      elements.sheet.close();
    }
  });
  elements.sheet.addEventListener("close", () => {
    if (elements.sheet.returnValue === "cancel") {
      confirmWith(elements.cancelDialog);
    } else if (elements.sheet.returnValue === "merge") {
      confirmWith(elements.mergeDialog);
    }
  });
  elements.merge.addEventListener("click", () => confirmWith(elements.mergeDialog));
  elements.cancelDialog.addEventListener("close", () => {
    if (elements.cancelDialog.returnValue === "confirm") {
      void runAction("isCanceling", { method: "cancel_session" }, elements.actionError);
    }
  });
  elements.mergeDialog.addEventListener("close", () => {
    if (elements.mergeDialog.returnValue === "confirm") {
      void runAction("isMerging", { method: "merge_session" }, elements.actionError);
    }
  });

  return {
    close,
    open,
    render,
    setCapabilities: (capabilities) => {
      state.capabilities = capabilities;
      renderActions();
      renderDock();
    },
    setConnected: (isConnected) => {
      state.isConnected = isConnected;
      renderActions();
      renderDock();
      if (!isConnected && !state.session) {
        elements.send.disabled = true;
        elements.message.placeholder = "Offline. Your draft is kept.";
      }
    },
    showError: (message) => {
      elements.actionError.textContent = message;
    },
  };
}

// An activity box the reader opened or closed stays that way when its
// content changes.
function keepActivityOpen(oldNode, newNode) {
  const oldDetails = oldNode?.querySelector?.("details");
  const newDetails = newNode.querySelector("details");
  if (oldDetails && newDetails) {
    newDetails.open = oldDetails.open;
  }
}

function element(document, tag, className, text) {
  const node = document.createElement(tag);
  if (className) {
    node.className = className;
  }
  if (text !== undefined) {
    node.textContent = text;
  }

  return node;
}
