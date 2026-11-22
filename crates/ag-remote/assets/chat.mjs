// Open session of the remote control client: the header with its status
// and actions, the transcript, the questions panel, the queued messages,
// and the composer. Transcript text, titles and questions are untrusted
// and only ever become text nodes, directly or through the element
// builder in markdown.mjs for answers, notices and questions. Updates change the nodes in place, so a
// refresh keeps focus, scroll position and what the user is typing.
// Functions take `document` and `window` as parameters, so the Node tests
// in crates/ag-remote/tests/web/chat.test.mjs pass the fake DOM.

import {
  activeSeconds,
  formatTimer,
  parseActivity,
  parseNotice,
  reviewLabel,
  sessionFigures,
  settingsParts,
  statusInfo,
} from "./format.mjs";
import { parseInlineMarkdown, parseMarkdown, renderMarkdown, safeLink } from "./markdown.mjs";
import { lineCounts, separator } from "./session_list.mjs";
import { offersSettings } from "./settings.mjs";

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
// are enabled. Diff, Merge and Publish appear only with their capability,
// and Sync only with its capability in a status that allows it.
export function sessionActions(statusName, capabilities = []) {
  const info = statusInfo(statusName);

  return {
    cancel: info.cancel,
    diff: capabilities.includes("diff"),
    diffEnabled: capabilities.includes("diff") && info.diff,
    merge: capabilities.includes("merge"),
    mergeEnabled: capabilities.includes("merge") && info.review,
    publish: capabilities.includes("publish"),
    publishEnabled: capabilities.includes("publish") && info.review,
    sync: capabilities.includes("sync") && info.sync,
  };
}

// The branch a merge or sync lands on, or `null` when the host does not
// say.
export function mergeTarget(session) {
  const branch = session?.base_branch;

  return typeof branch === "string" && branch !== "" ? branch : null;
}

// The remote branch a publish pushes to: the one an earlier publish used,
// without its remote's name, or else the session branch, which the host
// uses the first time. `null` when the host names neither.
export function publishBranch(session) {
  const upstream = session?.published_upstream_ref;
  if (typeof upstream === "string" && upstream !== "") {
    const slash = upstream.indexOf("/");

    return slash === -1 ? upstream : upstream.slice(slash + 1);
  }
  const branch = session?.branch;

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
// resolves with its result; `onHome()` returns to home. `now()` returns
// the time in seconds, for the header's timer.
//
// `diff` and `settings` are the diff view and the settings sheet, from
// diff.mjs and settings.mjs, when the page has them.
//
// Returns `open(sessionId, failure)`, `close()`, `render(session, summary)` for a
// snapshot and the session's entry in the listing (`session` is `null`
// when the session is gone), `setConnected(isConnected)`,
// `setCapabilities(list)`, `keepDraft(sessionId, text)`, `tick()`, and
// `showLoadError(sessionId, message)`.
export function createChat({
  document,
  window,
  elements,
  request,
  onHome,
  diff = null,
  settings = null,
  now = () => Date.now() / 1000,
}) {
  const drafts = createDrafts(window);
  const state = {
    capabilities: [],
    // Sessions with a publish in flight. A publish can take long and
    // outlasts switching sessions, so this outlives `reset`.
    publishing: new Set(),
    // How the last publish of each session ended: `{ reviewRequest }` or
    // `{ error }`, shown whenever that session is open.
    published: new Map(),
    // Seconds this device's clock is known to run behind the host's.
    clockSkew: 0,
    // Where the transcript was when the diff replaced it.
    chatPlace: null,
    // What the open diff was loaded for; see `diffKey`.
    diffKey: null,
    isAnswering: false,
    isCanceling: false,
    isConnected: false,
    // Whether the header's error says a refresh failed.
    isLoadError: false,
    isMerging: false,
    isSending: false,
    isStopping: false,
    isSyncing: false,
    picks: [],
    questions: [],
    questionsKey: null,
    rendered: [],
    session: null,
    // The open session's entry in the listing, with its figures.
    summary: null,
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
    // Parts that did not change stay in place, so a focused review link
    // keeps its focus through a refresh.
    updateChildren(elements.sub, [line, ...headerDetails(document, session, state.summary)]);
    renderTimer();
    elements.sheetTitle.textContent = session.title || "No title";
    elements.sheetSub.textContent = info.label;
    renderActions();
  }

  // Shows how long the session has worked while it runs.
  function renderTimer() {
    const timer = elements.sub.querySelector(".timer");
    const seconds = activeSeconds(state.summary, now() + state.clockSkew);
    if (seconds === null) {
      timer?.remove();

      return;
    }
    const label = "Timer " + formatTimer(seconds);
    if (timer) {
      timer.textContent = label;
    } else {
      elements.sub.append(element(document, "span", "timer", label));
    }
  }

  // Shows the session settings in the composer's line, or the key hint
  // when the host names none. A host that lets this device change them
  // makes the line a button that opens the settings sheet.
  function renderMeta(hint) {
    const parts = settingsParts(state.session, state.summary);
    elements.composerHint.textContent = parts.length > 0 ? hint : "";
    const isButton = parts.length > 0 && Boolean(settings) && offersSettings(state.capabilities);
    elements.composerMeta.hidden = isButton;
    elements.composerSettings.hidden = !isButton;
    elements.composerSettings.disabled = !state.isConnected;
    if (parts.length === 0) {
      elements.composerMeta.textContent = hint;

      return;
    }
    const nodes = parts.flatMap((part, index) => {
      const node =
        part.type === "model"
          ? modelNode(document, part)
          : element(document, "span", null, part.label);

      return index === 0 ? [node] : [separator(document), node];
    });
    (isButton ? elements.composerSettings : elements.composerMeta).replaceChildren(...nodes);
  }

  function openSettings() {
    if (state.session && state.isConnected && offersSettings(state.capabilities)) {
      settings.open(state.session, { isRunning: statusInfo(state.session.status).running });
    }
  }

  // Shows the actions the status and the host allow. Merge sits in the
  // header on wide screens and in the sheet on phones; the sheet holds
  // `Cancel session` everywhere.
  function renderActions() {
    // Without a session, no capability offers anything.
    const actions = state.session
      ? sessionActions(state.session.status, state.capabilities)
      : sessionActions(null, []);
    if (!actions.diffEnabled && diff?.isOpen()) {
      showDiff(false);
    }
    const isDiffOpen = Boolean(diff?.isOpen());
    elements.diff.hidden = !actions.diff;
    elements.diff.disabled = !actions.diffEnabled || !state.isConnected;
    elements.diff.setAttribute("aria-pressed", String(isDiffOpen));
    elements.sheetDiff.hidden = !actions.diff;
    elements.sheetDiff.disabled = elements.diff.disabled;
    elements.sheetDiffLabel.textContent = isDiffOpen ? "Back to chat" : "Diff";
    elements.merge.hidden = !actions.merge;
    elements.merge.disabled = !actions.mergeEnabled || state.isMerging || !state.isConnected;
    elements.sheetMerge.hidden = !actions.merge;
    elements.sheetMerge.disabled = elements.merge.disabled;
    renderPublish(actions);
    elements.sheetSync.hidden = !actions.sync;
    elements.sheetSync.disabled = state.isSyncing || !state.isConnected;
    elements.cancel.hidden = !actions.cancel;
    elements.cancel.disabled = state.isCanceling || !state.isConnected;
    elements.more.hidden =
      !actions.cancel && !actions.merge && !actions.diff && !actions.publish && !actions.sync;
    // With only Diff, Publish and Merge in it, the sheet adds nothing to
    // the wide header.
    elements.more.classList.toggle("narrow-only", !actions.cancel && !actions.sync);
    const target = mergeTarget(state.session);
    elements.sheetMergeLabel.textContent = target ? "Merge into " + target : "Merge";
    elements.mergeTarget.textContent = target || "its base branch";
    elements.sheetSyncLabel.textContent = "Sync with " + (target || "base branch");
    elements.syncTarget.textContent = target || "its base branch";
    const lines = sessionFigures(state.summary).lines;
    elements.mergeLines.textContent = lines ? " (+" + lines.added + " -" + lines.deleted + ")" : "";
  }

  // Publish sits in the header on wide screens and in the sheet on
  // phones. While the open session's publish runs, both say so and stay
  // disabled, even after switching away and back.
  function renderPublish(actions) {
    const isPublishing = state.publishing.has(state.sessionId);
    elements.publish.hidden = !actions.publish;
    elements.publish.disabled = !actions.publishEnabled || isPublishing || !state.isConnected;
    elements.publish.setAttribute("aria-busy", String(isPublishing));
    elements.publishLabel.textContent = isPublishing ? "Publishing…" : "Publish";
    elements.sheetPublish.hidden = !actions.publish;
    elements.sheetPublish.disabled = elements.publish.disabled;
    // `reviewLabel` checks that the host names the review request.
    const review = state.session?.review_request;
    const reviewId = reviewLabel(review) ? review.display_id : null;
    let note = reviewId ? "refreshes " + reviewId : "opens a pull request";
    if (isPublishing) {
      note = "publishing…";
    }
    elements.sheetPublishNote.textContent = note;
    elements.publishBranch.textContent = publishBranch(state.session) || "the session branch";
    elements.publishReview.textContent = reviewId
      ? "refreshes its pull request " + reviewId
      : "opens a pull request for it";
  }

  // Says how the open session's last publish ended: a link to the review
  // request, or the host's error.
  function renderPublished() {
    const outcome = state.published.get(state.sessionId);
    const link = outcome?.reviewRequest ? reviewNode(document, outcome.reviewRequest) : null;
    if (link) {
      elements.actionNote.replaceChildren("Published · ", link);
    } else {
      elements.actionNote.replaceChildren();
    }
    if (outcome?.error) {
      elements.actionError.textContent = outcome.error;
    }
  }

  // Publishes `sessionId` once at a time, whichever session is open when
  // it ends.
  async function publish(sessionId) {
    if (!sessionId || state.publishing.has(sessionId) || !state.isConnected) {
      return;
    }
    state.publishing.add(sessionId);
    state.published.delete(sessionId);
    if (state.sessionId === sessionId) {
      elements.actionError.textContent = "";
      renderPublished();
    }
    renderActions();
    try {
      const result = await request({ method: "publish_session", session_id: sessionId });
      state.published.set(sessionId, { reviewRequest: result?.review_request ?? null });
    } catch (error) {
      state.published.set(sessionId, { error: error.message });
    } finally {
      state.publishing.delete(sessionId);
      if (state.sessionId === sessionId) {
        renderPublished();
      }
      renderActions();
    }
  }

  // Shows the session's diff in place of the transcript and the dock, or
  // the chat again.
  // The transcript keeps its place while hidden: back from the diff, it
  // returns there, or to the end if the reader was at the end, and offers
  // `Jump to latest` for what arrived meanwhile.
  function showDiff(isOpen) {
    if (isOpen) {
      state.chatPlace = {
        hasMissed: !elements.jump.hidden,
        isAtEnd: isNearEnd(),
        top: elements.scroller.scrollTop,
      };
      state.diffKey = diffKey(state.session, state.summary);
    }
    elements.scroller.hidden = isOpen;
    elements.dock.hidden = isOpen;
    if (isOpen) {
      void diff.open(state.sessionId);
    } else {
      diff.close();
      const place = state.chatPlace;
      state.chatPlace = null;
      if (place?.isAtEnd) {
        scrollToEnd();
      } else if (place) {
        elements.scroller.scrollTop = place.top;
        elements.jump.hidden = !place.hasMissed;
      }
    }
    renderActions();
  }

  function toggleDiff() {
    if (diff && state.session) {
      showDiff(!diff.isOpen());
    }
  }

  function renderTranscript(messages) {
    const next = transcriptEntries(messages);
    const changes = transcriptChanges(state.rendered, next);
    const children = [...elements.transcript.children];
    for (const index of changes.replace) {
      const node = renderMessage(messages[index]);
      keepActivityOpen(children[index], node);
      keepSourceView(children[index], node);
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
      item.append(inlineMarkdown("span", "notice-body", notice.body));
    } else if (kind === "assistant") {
      renderAnswer(item, content);
    } else {
      item.textContent = content;
    }

    return item;
  }

  // `tag` with class `className`, holding `text` rendered as inline
  // Markdown.
  function inlineMarkdown(tag, className, text) {
    const node = element(document, tag, className + " md-inline");
    node.append(renderMarkdown(parseInlineMarkdown(text), document, { window }));

    return node;
  }

  // An answer renders as Markdown, with its source one toggle away for
  // whatever the renderer gets wrong.
  function renderAnswer(item, content) {
    const rendered = element(document, "div", "md");
    rendered.append(renderMarkdown(parseMarkdown(content), document, { window }));
    const source = element(document, "div", "md-source", content);
    source.hidden = true;
    const toggle = element(document, "button", "source-toggle", "View source");
    toggle.type = "button";
    toggle.setAttribute("aria-pressed", "false");
    toggle.addEventListener("click", () => showSource(item, source.hidden));
    const tools = element(document, "div", "msg-tools");
    tools.append(toggle);
    item.append(rendered, source, tools);
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
    // Clicks and the keys that activate the summary both arrive as a click.
    summary.addEventListener("click", () => {
      details.dataset.toggled = "true";
    });
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
      inlineMarkdown("p", "question-text", question.text),
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
    renderMeta(dock.hint);
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
    elements.dock.hidden = false;
    diff?.close();
    state.chatPlace = null;
    elements.composer.hidden = false;
    elements.answerBar.hidden = true;
    elements.readonly.hidden = true;
    elements.stop.hidden = true;
    elements.sessionError.textContent = "";
    elements.answerError.textContent = "";
    elements.actionError.textContent = "";
    elements.actionNote.replaceChildren();
    elements.badge.hidden = true;
    elements.sub.replaceChildren();
    elements.composerHint.textContent = "";
    state.summary = null;
    elements.heading.textContent = "";
    state.isLoadError = false;
    // Nothing asked about the previous session may act once it is gone.
    const dialogs = [
      elements.sheet,
      elements.cancelDialog,
      elements.mergeDialog,
      elements.publishDialog,
      elements.syncDialog,
    ];
    for (const dialog of dialogs) {
      delete dialog.dataset.sessionId;
      if (dialog.open) {
        dialog.close("");
      }
    }
    settings?.close();
    renderActions();
  }

  // Opens `sessionId`. `failure`, when a first message from home did not
  // go through, keeps its text as the draft and says why.
  function open(sessionId, failure = null) {
    reset();
    state.sessionId = sessionId;
    renderPublished();
    if (failure) {
      drafts.set(sessionId, failure.draft);
      elements.sessionError.textContent = failure.error;
    }
    elements.message.value = drafts.get(sessionId);
    resizeComposer();
  }

  function close() {
    reset();
    state.sessionId = null;
  }

  function render(session, summary = null) {
    if (!session) {
      reset();
      elements.heading.textContent = "Session not found";
      elements.gone.hidden = false;
      elements.scroller.hidden = true;
      elements.composer.hidden = true;

      return;
    }
    const isFirst = state.session === null;
    state.session = session;
    state.summary = summary?.id === session.id ? summary : null;
    state.clockSkew = clockSkew(state.summary, now());
    if (state.isLoadError) {
      // A snapshot arrived, so the earlier refresh failure is over.
      state.isLoadError = false;
      elements.actionError.textContent = "";
    }
    elements.gone.hidden = true;
    // The header closes the diff when the new status allows none.
    renderHeader(session);
    const isDiffOpen = Boolean(diff?.isOpen());
    const wasNearEnd = isNearEnd();
    elements.scroller.hidden = isDiffOpen;
    const transcriptChanged = renderTranscript(session.messages || []);
    const questionsChanged = renderQuestions(session.questions || []);
    renderQueued(session.queued_messages || []);
    renderDock();
    settings?.update(session, { isRunning: statusInfo(session.status).running });
    if (isDiffOpen) {
      // The transcript is out of sight; returning to it says what arrived.
      state.chatPlace.hasMissed ||= transcriptChanged || questionsChanged;
      // The files are read again only when the session's changes may have.
      const key = diffKey(session, state.summary);
      if (key !== state.diffKey) {
        state.diffKey = key;
        void diff.refresh();
      }

      return;
    }
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
      // Text typed after sending, here or before switching away, stays.
      if (drafts.get(sessionId) === message) {
        drafts.clear(sessionId);
      }
      if (state.sessionId === sessionId) {
        if (elements.message.value === message) {
          elements.message.value = "";
          resizeComposer();
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

  // Runs one action on `sessionId` and shows its error in `errorNode`.
  async function runAction(sessionId, flag, body, errorNode) {
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

  // Asks with `dialog` about the open session, and remembers which one.
  function confirmWith(dialog) {
    if (!state.sessionId) {
      return;
    }
    dialog.dataset.sessionId = state.sessionId;
    dialog.returnValue = "";
    dialog.showModal();
  }

  // The session `dialog` was opened for, when the user confirmed it and
  // that session is still open; otherwise `null`.
  function confirmedSession(dialog, value) {
    const sessionId = dialog.dataset.sessionId ?? null;
    delete dialog.dataset.sessionId;

    return dialog.returnValue === value && sessionId === state.sessionId ? sessionId : null;
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
    void runAction(state.sessionId, "isStopping", { method: "stop_turn" }, elements.sessionError);
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
  elements.more.addEventListener("click", () => confirmWith(elements.sheet));
  // A tap beside the sheet, on its backdrop, closes it.
  elements.sheet.addEventListener("click", (event) => {
    if (event.target === elements.sheet) {
      elements.sheet.close();
    }
  });
  elements.sheet.addEventListener("close", () => {
    const choice = elements.sheet.returnValue;
    if (!confirmedSession(elements.sheet, choice)) {
      return;
    }
    if (choice === "cancel") {
      confirmWith(elements.cancelDialog);
    } else if (choice === "merge") {
      confirmWith(elements.mergeDialog);
    } else if (choice === "publish") {
      confirmWith(elements.publishDialog);
    } else if (choice === "sync") {
      confirmWith(elements.syncDialog);
    } else if (choice === "diff") {
      toggleDiff();
    }
  });
  elements.diff.addEventListener("click", toggleDiff);
  elements.merge.addEventListener("click", () => confirmWith(elements.mergeDialog));
  elements.publish.addEventListener("click", () => confirmWith(elements.publishDialog));
  elements.publishDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.publishDialog, "confirm");
    if (sessionId) {
      void publish(sessionId);
    }
  });
  elements.syncDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.syncDialog, "confirm");
    if (sessionId) {
      void runAction(sessionId, "isSyncing", { method: "sync_session" }, elements.actionError);
    }
  });
  elements.composerSettings.addEventListener("click", openSettings);
  elements.cancelDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.cancelDialog, "confirm");
    if (sessionId) {
      void runAction(sessionId, "isCanceling", { method: "cancel_session" }, elements.actionError);
    }
  });
  elements.mergeDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.mergeDialog, "confirm");
    if (sessionId) {
      void runAction(sessionId, "isMerging", { method: "merge_session" }, elements.actionError);
    }
  });

  return {
    close,
    // Keeps `text` as the unsent message of `sessionId`, for when it opens.
    keepDraft: (sessionId, text) => drafts.set(sessionId, text),
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
    // Advances the header's timer, once a second.
    tick: () => {
      if (state.session) {
        renderTimer();
      }
    },
    // Shows that refreshing `sessionId` failed, if it is still open. The
    // next snapshot clears it.
    showLoadError: (sessionId, message) => {
      if (sessionId !== state.sessionId) {
        return;
      }
      state.isLoadError = true;
      elements.actionError.textContent = message;
    },
  };
}

// An activity box the reader opened or closed stays that way when its
// content changes. One the reader left alone takes the default for its
// new length.
function keepActivityOpen(oldNode, newNode) {
  const oldDetails = oldNode?.querySelector?.("details");
  const newDetails = newNode.querySelector("details");
  if (oldDetails?.dataset.toggled && newDetails) {
    newDetails.open = oldDetails.open;
    newDetails.dataset.toggled = "true";
  }
}

// Shows the source of the answer in `item` instead of its rendering, or
// the rendering again.
function showSource(item, isSource) {
  item.querySelector(".md").hidden = isSource;
  item.querySelector(".md-source").hidden = !isSource;
  const toggle = item.querySelector(".source-toggle");
  toggle.textContent = isSource ? "View rendered" : "View source";
  toggle.setAttribute("aria-pressed", String(isSource));
}

// An answer whose source the reader opened keeps showing it when its
// content changes.
function keepSourceView(oldNode, newNode) {
  const oldSource = oldNode?.querySelector?.(".md-source");
  if (oldSource && !oldSource.hidden && newNode.querySelector(".md-source")) {
    showSource(newNode, true);
  }
}

// What says a session's changes may differ: the listing's line counts,
// or, from a host that lists none, the session's update time.
export function diffKey(session, summary) {
  if (Number.isFinite(summary?.added_lines) && Number.isFinite(summary?.deleted_lines)) {
    return "lines:" + summary.added_lines + ":" + summary.deleted_lines;
  }

  return "updated:" + session?.updated_at;
}

// How many seconds this device's clock runs behind the host's, as far as
// `summary` shows: the host's times cannot lie in its future, so a time
// ahead of `nowSeconds` reveals the gap. The protocol carries no host
// clock, so a device clock that runs ahead stays unnoticed and the timer
// then reads high by that much.
export function clockSkew(summary, nowSeconds) {
  const latest = Math.max(
    Number.isFinite(summary?.updated_at) ? summary.updated_at : -Infinity,
    Number.isFinite(summary?.in_progress_started_at) ? summary.in_progress_started_at : -Infinity,
  );

  return Math.max(0, latest - nowSeconds);
}

// Makes `container`'s children match `nodes`, leaving each child whose
// class, text, link and title did not change in place, so it keeps focus.
// A `.timer` child goes, and `renderTimer` adds it again last.
function updateChildren(container, nodes) {
  container.querySelector(".timer")?.remove();
  const current = [...container.children];
  nodes.forEach((node, index) => {
    const old = current[index];
    if (!old) {
      container.append(node);
    } else if (nodeKey(old) !== nodeKey(node)) {
      old.replaceWith(node);
    }
  });
  for (const extra of current.slice(nodes.length)) {
    extra.remove();
  }
}

function nodeKey(node) {
  return [
    node.tagName,
    node.className,
    node.textContent,
    node.getAttribute("href") ?? "",
    node.title ?? "",
  ].join("\u0000");
}

// The header's details after the status: the size badge and line counts
// from the listing, the branch and its base, the model, and the review
// request as a link. Whatever the host omits is left out. The branch and
// model give way first on a phone.
function headerDetails(document, session, summary) {
  const figures = sessionFigures(summary);
  const nodes = [];
  if (figures.size) {
    nodes.push(element(document, "span", "size", "[" + figures.size + "]"));
  }
  if (figures.lines) {
    nodes.push(lineCounts(document, figures.lines));
  }
  const branch = typeof session.branch === "string" ? session.branch : "";
  const base = typeof session.base_branch === "string" ? session.base_branch : "";
  if (branch) {
    nodes.push(element(document, "span", "wide-only", base ? branch + " → " + base : branch));
  }
  const settings = settingsParts(session, summary).find((part) => part.type === "model");
  if (settings) {
    const model = modelNode(document, settings);
    model.classList.add("wide-only");
    nodes.push(model);
  }
  const review = reviewNode(document, session.review_request);
  if (review) {
    nodes.push(review);
  }

  return nodes;
}

// The model with its reasoning level, as `claude/opus-5.5 [high]`.
function modelNode(document, part) {
  const node = element(document, "span", "model");
  node.append(element(document, "b", null, part.label));
  if (part.reasoning) {
    node.append(" [" + part.reasoning + "]");
  }

  return node;
}

// The linked review request as a link to the forge. Its address goes
// through the same filter as Markdown links, so only an `http:`, `https:`
// or `mailto:` address becomes a link; any other shows as text.
function reviewNode(document, reviewRequest) {
  const label = reviewLabel(reviewRequest);
  if (!label) {
    return null;
  }
  const href = safeLink(reviewRequest.web_url);
  const node = element(document, href ? "a" : "span", "review", label);
  if (href) {
    node.setAttribute("href", href);
    node.setAttribute("rel", "noopener noreferrer");
    node.setAttribute("target", "_blank");
  }
  if (typeof reviewRequest.title === "string" && reviewRequest.title !== "") {
    node.title = reviewRequest.title;
  }

  return node;
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
