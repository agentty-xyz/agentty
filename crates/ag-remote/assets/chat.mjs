// Open session of the remote control client: the header with its status
// and actions, a draft's preview, the transcript, the questions panel, the
// queued messages and actions, and the composer from composer.mjs.
// Transcript text, titles, staged drafts and questions are untrusted
// and only ever become text nodes, directly or through the element
// builder in markdown.mjs for answers, notices and questions. Updates change the nodes in place, so a
// refresh keeps focus, scroll position and what the user is typing.
// Functions take `document` and `window` as parameters, so the Node tests
// in crates/ag-remote/tests/web/chat.test.mjs pass the fake DOM.

import { createComposer, modelNode, promptHistory } from "./composer.mjs";
import {
  activeSeconds,
  formatTimer,
  parseActivity,
  parseNotice,
  reviewLabel,
  sessionFigures,
  settingsParts,
  startedLabel,
  statusInfo,
  statusLine,
} from "./format.mjs";
import { renderGraphSummary } from "./graph.mjs";
import {
  keepSourceView,
  parseInlineMarkdown,
  renderAnswer,
  renderMarkdown,
  safeLink,
} from "./markdown.mjs";
import { createMentions } from "./mention.mjs";
import { conflictLabel, lineCounts } from "./session_list.mjs";
import { createSuggestions } from "./suggest.mjs";

// Shortest time between two refreshes.
export const REFRESH_INTERVAL_MS = 1000;
// How close to the end of the transcript the reader must be for new
// content to scroll into view.
export const FOLLOW_DISTANCE_PX = 80;
// An activity box with more lines than this starts closed.
const OPEN_ACTIVITY_LINES = 3;
const MESSAGE_KINDS = new Set(["activity", "assistant", "notice", "user"]);
// Statuses in which a message waits for the running turn.
const QUEUEING_STATUSES = new Set(["InProgress", "Rebasing"]);
// What a read-only status says instead of the composer, when its label
// alone would not.
const READ_ONLY_LABELS = { Queued: "Queued to merge" };
// The focused-review states the page shows: `ready` carries Markdown and
// `failed` a plain message.
const FOCUSED_REVIEW_STATES = new Set(["failed", "ready"]);
// Why Merge is off for a session with a linked review request: the
// header button's tooltip, and the short note that fits the phone sheet.
const MERGE_OFF_TITLE = "Merge is off: merge the session's pull request instead";
const MERGE_OFF_NOTE = "has a pull request";
// What Publish says while a turn runs, as the terminal's `p` does.
const PUBLISH_WAIT_HINT = "Publishes after the current turn";
// The classes of the timing parts that end the header's details, in order.
const TIMING_PARTS = ["timer", "started"];
// What a draft session's preview says: a message sent to a draft stages
// it beside any staged ones and starts the session.
const DRAFT_STAGED_NOTE = "Staged on the host. Your next message starts the session with it.";
const DRAFT_EMPTY_NOTE = "Your first message starts the session.";

// The error a request in flight ends with when the connection drops
// before Agentty answers. The request may still run on the host.
export function connectionLostError(message) {
  const error = new Error(message);
  error.isConnectionLost = true;

  return error;
}

// Whether `error` says the connection dropped rather than Agentty
// refusing; see `connectionLostError`.
export function isConnectionLost(error) {
  return error?.isConnectionLost === true;
}

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

// Which of the header's parts, at the given `tops`, wrapped below its
// one line, which starts at `lineTop` and is `lineHeight` tall. A part
// counts as wrapped once it starts half a line or more down; a part with
// no box, such as a phone's hidden `wide-only` part, measures at 0 and
// stays as it is.
export function wrappedParts(tops, lineTop, lineHeight) {
  return tops.map((top) => top - lineTop >= Math.max(lineHeight / 2, 1));
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
// and Sync only with its capability in a status that allows it. A session
// with a linked review request merges on the forge, so the host refuses
// to merge it and `hasReviewRequest` disables Merge, as the terminal
// hides `m`. As `p` in the terminal, Publish also works while a turn or a
// sync runs, and `publishWaits` says that the host then publishes after
// it.
export function sessionActions(statusName, capabilities = [], hasReviewRequest = false) {
  const info = statusInfo(statusName);
  const publishWaits = QUEUEING_STATUSES.has(statusName);

  return {
    cancel: info.cancel,
    diff: capabilities.includes("diff"),
    diffEnabled: capabilities.includes("diff") && info.diff,
    merge: capabilities.includes("merge"),
    mergeEnabled: capabilities.includes("merge") && info.review && !hasReviewRequest,
    publish: capabilities.includes("publish"),
    publishEnabled: capabilities.includes("publish") && (info.review || publishWaits),
    publishWaits: publishWaits,
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

// Wires the open session. `elements` holds the nodes with the ids named in
// index.html, keyed as in `app.js`. `request(body)` sends one request and
// resolves with its result; `onHome()` returns to home. `now()` returns
// the time in seconds, for the header's timer.
//
// `diff`, `graph` and `settings` are the diff view, the graphs panel and
// the settings sheet, from diff.mjs, graph.mjs and settings.mjs, when the
// page has them.
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
  graph = null,
  settings = null,
  now = () => Date.now() / 1000,
}) {
  const state = {
    capabilities: [],
    // Sessions with a publish in flight. A publish can take long and
    // outlasts switching sessions, so this outlives `reset`.
    publishing: new Set(),
    // Sessions whose publish, queued behind a turn, lost its answer to a
    // dropped connection. Agentty still runs it, so until a snapshot shows
    // the turn over the page keeps saying it is queued.
    queuedPublishes: new Set(),
    // How the last publish of each session ended, `{ reviewRequest }` or
    // `{ error }`, until it shows once with that session open.
    published: new Map(),
    // The outcome on screen for the open session; see `expirePublished`.
    shownPublish: null,
    // Requests in flight, as `actionKey(method, sessionId)`, so an action
    // on one session never blocks or answers for another.
    acting: new Set(),
    // The `@` file lookups of the answer fields on screen.
    answerLookups: [],
    // Seconds this device's clock is known to run behind the host's.
    clockSkew: 0,
    // Where the transcript was when the diff or the graphs replaced it.
    chatPlace: null,
    // How many rows the sheet's details list.
    detailCount: 0,
    // What the open diff was loaded for; see `diffKey`.
    diffKey: null,
    // Whether the open graphs added a history entry the back gesture pops.
    graphEntry: false,
    isAnswering: false,
    isConnected: false,
    // Whether the page went back over the graphs' entry and the browser
    // has not landed yet.
    isLeavingGraphs: false,
    // Whether the header's error says a refresh failed.
    isLoadError: false,
    isSending: false,
    picks: [],
    questions: [],
    questionsKey: null,
    rendered: [],
    session: null,
    // The open session's entry in the listing, with its figures.
    summary: null,
    sessionId: null,
  };
  // The composer, shared with home's first message.
  const composer = createComposer({
    context: () => ({
      capabilities: state.capabilities,
      draftId: state.sessionId,
      isConnected: state.isConnected,
      isRunning: Boolean(state.session) && statusInfo(state.session.status).running,
      session: state.session,
      summary: state.summary,
    }),
    document: document,
    elements: {
      form: elements.composer,
      hint: elements.composerHint,
      message: elements.message,
      meta: elements.composerMeta,
      send: elements.send,
      settingsButton: elements.composerSettings,
    },
    history: () => promptHistory(state.session),
    onSend: () => void sendMessage(),
    request: request,
    settings: settings,
    showError: (message) => {
      elements.sessionError.textContent = message;
    },
    suggestionsId: "message-suggestions",
    window: window,
  });

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
    const badgeStatus = element(document, "span", "status status--" + info.tone);
    badgeStatus.append(element(document, "i", "status-dot"), info.label);
    elements.badge.replaceChildren(badgeStatus);
    const line = element(document, "span", "status narrow-only status--" + info.tone);
    line.append(element(document, "i", "status-dot"), info.label);
    // Parts that did not change stay in place, so a focused review link
    // keeps its focus through a refresh.
    updateChildren(elements.sub, [line, ...headerDetails(document, session, state.summary)]);
    renderTimer();
    elements.sheetTitle.textContent = session.title || "No title";
    elements.sheetSub.textContent = info.label;
    renderSheetDetails(session);
    renderActions();
  }

  // The header's timing values now: the active time and the session's
  // age, each `null` when the host does not say.
  function timing() {
    const time = now() + state.clockSkew;
    const seconds = activeSeconds(state.summary, time);

    return {
      started: startedLabel(state.session?.created_at, time),
      timer: seconds === null ? null : formatTimer(seconds),
    };
  }

  // Lists the session's details in the `⋯` sheet, in full, for what the
  // header line has no room for. The list is built again only when a row
  // other than the timing ones changes, so a focused link keeps its
  // focus; `renderTimer` updates the timing rows in place.
  function renderSheetDetails(session) {
    const rows = session ? sheetDetailRows(document, session, state.summary, timing()) : [];
    const key = JSON.stringify(rows.map((row) => [row.label, row.isTiming ? "" : nodeKey(row.value)]));
    state.detailCount = rows.length;
    elements.sheetDetails.hidden = rows.length === 0;
    if (elements.sheetDetails.dataset.key === key) {
      return;
    }
    elements.sheetDetails.dataset.key = key;
    elements.sheetDetails.replaceChildren(
      ...rows.map((row) => {
        const item = element(document, "div", "sheet-detail");
        const value = element(document, "dd");
        value.append(row.value);
        item.append(element(document, "dt", null, row.label), value);

        return item;
      }),
    );
  }

  // Ends the header's details as the terminal's composer border does:
  // `Timer: 4m 12s`, counting while a turn runs and still between turns,
  // then `Started 3h 5m ago`. Each goes when the host does not say. The
  // sheet's timing rows follow.
  function renderTimer() {
    const values = timing();
    const labels = {
      started: values.started,
      timer: values.timer === null ? null : "Timer: " + values.timer,
    };
    renderSheetTiming(values);
    const parts = TIMING_PARTS.filter((part) => labels[part] !== null);
    const nodes = TIMING_PARTS.map((part) => elements.sub.querySelector("." + part)).filter(Boolean);
    // The same parts update in place, so the timer keeps its node.
    if (nodes.map((node) => node.className).join() !== parts.join()) {
      for (const node of nodes) {
        node.remove();
      }
      elements.sub.append(...parts.map((part) => element(document, "span", part, labels[part])));
    } else {
      for (const node of nodes) {
        if (node.textContent !== labels[node.className]) {
          node.textContent = labels[node.className];
        }
      }
    }
    // Every header render ends here, and a longer time can push a part
    // off the line.
    markWrappedParts();
  }

  // Updates the sheet's timing rows in place to the header's `values`.
  function renderSheetTiming(values) {
    for (const [part, value] of Object.entries(sheetTimingValues(values))) {
      const node = elements.sheetDetails.querySelector(".sheet-" + part);
      if (node && node.textContent !== value) {
        node.textContent = value;
      }
    }
  }

  // Makes the header's parts that wrapped onto its hidden second line
  // `inert`, so Tab and screen readers skip what is not on screen, and
  // frees them once they fit again. A part on the line, such as a focused
  // review link, is never marked.
  function markWrappedParts() {
    const line = elements.sub.getBoundingClientRect();
    const parts = [...elements.sub.children];
    const isWrapped = wrappedParts(
      parts.map((part) => part.getBoundingClientRect().top),
      line.top,
      line.height,
    );
    parts.forEach((part, index) => {
      if (part.hasAttribute("inert") !== isWrapped[index]) {
        part.toggleAttribute("inert", isWrapped[index]);
      }
    });
  }

  // Shows the actions the status and the host allow. Merge sits in the
  // header on wide screens and in the sheet on phones; the sheet holds
  // `Cancel session` everywhere.
  function renderActions() {
    // Without a session, no capability offers anything.
    const hasReviewRequest = Boolean(reviewLabel(state.session?.review_request));
    const actions = state.session
      ? sessionActions(state.session.status, state.capabilities, hasReviewRequest)
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
    renderGraphActions();
    elements.merge.hidden = !actions.merge;
    elements.merge.disabled = !actions.mergeEnabled || isActing("merge_session") || !state.isConnected;
    elements.merge.title = hasReviewRequest ? MERGE_OFF_TITLE : "";
    elements.sheetMergeNote.textContent = hasReviewRequest ? MERGE_OFF_NOTE : "asks first";
    elements.sheetMerge.hidden = !actions.merge;
    elements.sheetMerge.disabled = elements.merge.disabled;
    renderPublish(actions);
    elements.sheetSync.hidden = !actions.sync;
    elements.sheetSync.disabled = isActing("sync_session") || !state.isConnected;
    elements.cancel.hidden = !actions.cancel;
    elements.cancel.disabled = isActing("cancel_session") || !state.isConnected;
    renderMore(actions);
    const target = mergeTarget(state.session);
    elements.sheetMergeLabel.textContent = target ? "Merge into " + target : "Merge";
    elements.mergeTarget.textContent = target || "its base branch";
    elements.sheetSyncLabel.textContent = "Sync with " + (target || "base branch");
    elements.syncTarget.textContent = target || "its base branch";
    const lines = sessionFigures(state.summary).lines;
    elements.mergeLines.textContent = lines ? " (+" + lines.added + " -" + lines.deleted + ")" : "";
  }

  // Whether the host lets a device read the open session's graphs, which
  // the terminal's `w` offers in every status.
  function hasGraphs() {
    return Boolean(graph && state.session) && state.capabilities.includes("graphs");
  }

  // Graphs sits in the header on wide screens and in the sheet on phones,
  // like Diff; the summary beside the chat opens it too.
  function renderGraphActions() {
    const isOpen = Boolean(graph?.isOpen());
    if (isOpen && !hasGraphs()) {
      // Closing renders the actions again.
      leaveGraphEntry();
      showGraphs(false);

      return;
    }
    elements.graphs.hidden = !hasGraphs();
    elements.graphs.disabled = !state.isConnected;
    elements.graphs.setAttribute("aria-pressed", String(isOpen));
    elements.sheetGraphs.hidden = elements.graphs.hidden;
    elements.sheetGraphs.disabled = elements.graphs.disabled;
    elements.sheetGraphsLabel.textContent = isOpen ? "Back to chat" : "Graphs";
    elements.graphSummary.disabled = !hasGraphs() || !state.isConnected;
  }

  // Shows the `⋯` button when its sheet has details or an action to offer.
  function renderMore(actions) {
    const hasDetails = state.detailCount > 0;
    elements.more.hidden = !hasDetails && !hasGraphs() &&
      !actions.cancel && !actions.merge && !actions.diff && !actions.publish && !actions.sync;
    // With only Diff, Publish and Merge in it and no details, the sheet
    // adds nothing to the wide header.
    elements.more.classList.toggle("narrow-only", !hasDetails && !actions.cancel && !actions.sync);
  }

  // Publish sits in the header on wide screens and in the sheet on
  // phones. While the open session's publish runs, both say so and stay
  // disabled, even after switching away and back. A publish asked for
  // during a turn waits for it: the host answers only once it ran, so
  // until the turn ends both say it is queued.
  function renderPublish(actions) {
    const isPublishing = isPublishingSession(state.sessionId);
    elements.publish.hidden = !actions.publish;
    elements.publish.disabled = !actions.publishEnabled || isPublishing || !state.isConnected;
    elements.publish.setAttribute("aria-busy", String(isPublishing));
    elements.publish.title = actions.publishWaits && !isPublishing ? PUBLISH_WAIT_HINT : "";
    let label = "Publish";
    // `reviewLabel` checks that the host names the review request.
    const review = state.session?.review_request;
    const reviewId = reviewLabel(review) ? review.display_id : null;
    let note = reviewId ? "refreshes " + reviewId : "opens a pull request";
    // The sheet's notes stay short enough for a phone's width.
    if (isPublishing && actions.publishWaits) {
      label = "Publish queued";
      note = "queued";
    } else if (isPublishing) {
      label = "Publishing…";
      note = "publishing…";
    } else if (actions.publishWaits) {
      note = "after this turn";
    }
    elements.publishLabel.textContent = label;
    elements.sheetPublish.hidden = !actions.publish;
    elements.sheetPublish.disabled = elements.publish.disabled;
    elements.sheetPublishNote.textContent = note;
    elements.publishWait.hidden = !actions.publishWaits;
    elements.publishBranch.textContent = publishBranch(state.session) || "the session branch";
    elements.publishReview.textContent = reviewId
      ? "refreshes its pull request " + reviewId
      : "opens a pull request for it";
  }

  // Says once how the open session's last publish ended: a link to the
  // review request, or the host's error. The outcome is then forgotten,
  // so coming back later does not announce it again; the header's review
  // link stays as the lasting record.
  function showPublished() {
    const outcome = state.published.get(state.sessionId);
    if (!outcome) {
      return;
    }
    state.published.delete(state.sessionId);
    state.shownPublish = {
      error: outcome.error ?? null,
      // What the snapshot said about publishing when the outcome showed.
      publication: publicationKey(state.session),
      review: outcome.reviewRequest ? reviewKey(outcome.reviewRequest) : null,
    };
    const link = outcome.reviewRequest ? reviewNode(document, outcome.reviewRequest) : null;
    if (link) {
      elements.actionNote.replaceChildren("Published · ", link);
    }
    if (outcome.error) {
      elements.actionError.textContent = outcome.error;
    }
  }

  // Takes a shown outcome away once a snapshot tells a newer story: the
  // review request is no longer the one published, or, after a failure,
  // the session was published or its review request changed since.
  function expirePublished(session) {
    const shown = state.shownPublish;
    if (!shown) {
      return;
    }
    const isStale = shown.review
      ? Boolean(session.review_request) && reviewKey(session.review_request) !== shown.review
      : shown.publication !== null && publicationKey(session) !== shown.publication;
    if (shown.publication === null) {
      // The outcome showed before the first snapshot; this one is the basis.
      shown.publication = publicationKey(session);
    }
    if (!isStale) {
      return;
    }
    state.shownPublish = null;
    elements.actionNote.replaceChildren();
    if (shown.error && elements.actionError.textContent === shown.error) {
      elements.actionError.textContent = "";
    }
  }

  // Whether a publish of `sessionId` is on its way, answered or not.
  function isPublishingSession(sessionId) {
    return state.publishing.has(sessionId) || state.queuedPublishes.has(sessionId);
  }

  // Publishes `sessionId` once at a time, whichever session is open when
  // it ends. A publish asked for during a turn that loses its answer to a
  // dropped connection still runs on the host, so it stays queued instead
  // of failing.
  async function publish(sessionId) {
    if (!sessionId || isPublishingSession(sessionId) || !state.isConnected) {
      return;
    }
    const waits = state.sessionId === sessionId && QUEUEING_STATUSES.has(state.session?.status);
    state.publishing.add(sessionId);
    state.published.delete(sessionId);
    if (state.sessionId === sessionId) {
      elements.actionError.textContent = "";
      elements.actionNote.replaceChildren();
      state.shownPublish = null;
    }
    renderActions();
    try {
      const result = await request({ method: "publish_session", session_id: sessionId });
      state.published.set(sessionId, { reviewRequest: result?.review_request ?? null });
    } catch (error) {
      if (waits && isConnectionLost(error)) {
        state.queuedPublishes.add(sessionId);
      } else {
        state.published.set(sessionId, { error: error.message });
      }
    } finally {
      state.publishing.delete(sessionId);
      if (state.sessionId === sessionId) {
        showPublished();
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
      leaveGraphEntry();
      closeGraphs();
      leaveChat();
      state.diffKey = diffKey(state.session, state.summary);
      void diff.open(state.sessionId);
    } else {
      diff.close();
      returnToChat();
    }
    renderActions();
  }

  // Hides the transcript and the dock for a panel, remembering where the
  // reader was. A panel that replaces another keeps the first place.
  function leaveChat() {
    state.chatPlace ??= {
      hasMissed: !elements.jump.hidden,
      isAtEnd: isNearEnd(),
      top: elements.scroller.scrollTop,
    };
    elements.scroller.hidden = true;
    elements.dock.hidden = true;
  }

  // Shows the transcript and the dock again where the reader left them.
  function returnToChat() {
    elements.scroller.hidden = false;
    elements.dock.hidden = false;
    const place = state.chatPlace;
    state.chatPlace = null;
    if (place?.isAtEnd) {
      scrollToEnd();
    } else if (place) {
      elements.scroller.scrollTop = place.top;
      elements.jump.hidden = !place.hasMissed;
    }
  }

  // Shows the session's graphs in place of the transcript and the dock, as
  // the terminal's `w` does, or the chat again. Opening adds a history
  // entry, so the back gesture returns to the chat; the page never stacks
  // a second one.
  function showGraphs(isOpen) {
    if (isOpen) {
      if (diff?.isOpen()) {
        diff.close();
      }
      leaveChat();
      if (window.history.state?.graphs !== true) {
        window.history.pushState({ graphs: true }, "", window.location.href);
      }
      state.graphEntry = true;
      void graph.open(state.sessionId);
    } else {
      graph.close();
      state.graphEntry = false;
      returnToChat();
    }
    renderActions();
  }

  // Opens the graphs, or closes them at once and drops their history
  // entry when it is still the current one, so the entry goes with them.
  function toggleGraphs() {
    if (!hasGraphs()) {
      return;
    }
    if (graph.isOpen()) {
      leaveGraphEntry();
      showGraphs(false);
    } else {
      showGraphs(true);
    }
  }

  // Closes the graphs without returning to the chat, for a panel or a
  // session that replaces them. Their history entry stays: after a session
  // switch it is no longer the current one.
  function closeGraphs() {
    if (graph?.isOpen()) {
      graph.close();
    }
    state.graphEntry = false;
  }

  // Goes back over the open graphs' history entry, so no back step is left
  // that shows nothing, as when the diff replaces them on the same entry.
  // An entry above theirs is left alone: going back would only reach
  // theirs. The flag clears first, so the `popstate` the step fires finds
  // nothing more to close, and a step already under way is not asked for
  // twice: until it lands, the entry still reads as theirs.
  function leaveGraphEntry() {
    const isCurrent =
      state.graphEntry && !state.isLeavingGraphs && window.history.state?.graphs === true;
    state.graphEntry = false;
    if (isCurrent) {
      state.isLeavingGraphs = true;
      window.history.back();
    }
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

  // Shows the session's latest focused review under the transcript, as the
  // terminal's session view does: a finished review as Markdown with its
  // source a toggle away, like an answer, and a failed one as its message.
  // Both are untrusted text. Older hosts send none, and a state this client
  // does not know shows nothing. An unchanged review keeps its node, and
  // with it the reader's choice of source or rendered view. Returns whether
  // the block changed.
  function renderFocusedReview(review) {
    const node = elements.focusedReview;
    const isShown = FOCUSED_REVIEW_STATES.has(review?.state) && typeof review.text === "string";
    const key = isShown ? JSON.stringify([review.state, review.text]) : "";
    if ((node.dataset.review ?? "") === key) {
      return false;
    }
    node.dataset.review = key;
    node.hidden = !isShown;
    node.classList.toggle("is-failed", isShown && review.state === "failed");
    if (!isShown) {
      node.replaceChildren();

      return true;
    }
    const body = element(document, "div", "focused-review-body");
    if (review.state === "ready") {
      renderAnswer(body, review.text, document, { window });
      keepSourceView(node.querySelector(".focused-review-body"), body);
    } else {
      body.append(element(document, "p", "focused-review-failure", review.text));
    }
    node.replaceChildren(element(document, "p", "eyebrow", "Focused review"), body);

    return true;
  }

  // Ends the transcript with what the session is doing, as the terminal
  // does, such as `Working... Reading files`. Returns whether the line
  // changed. The marker node stays while the line shows, so its loader
  // animates on when only the text changes.
  function renderStatusLine(statusName, progress) {
    const node = elements.statusLine;
    const line = statusLine(statusName, progress);
    const key = line ? JSON.stringify([statusName, line.text, line.details]) : "";
    if (node.dataset.status === key) {
      return false;
    }
    node.dataset.status = key;
    node.hidden = line === null;
    if (!line) {
      node.replaceChildren();

      return true;
    }
    const loading = line.isLoading ? " status--live" : "";
    node.className = "status status--line status-line status--" + statusInfo(statusName).tone + loading;
    if (!node.firstElementChild) {
      node.replaceChildren(element(document, "i", "status-dot"), element(document, "span", "status-line-body"));
    }
    node.lastElementChild.replaceChildren(
      element(document, "span", null, line.text),
      ...line.details.map((detail) => element(document, "span", "status-line-detail", detail)),
    );

    return true;
  }

  // Heads the transcript of a draft session, as the terminal's
  // `## Draft Session` preview does, with the messages it has staged on the
  // host, which are untrusted text. Older hosts send no staged messages.
  // Returns whether the preview changed.
  function renderDraft(session) {
    const node = elements.draftPreview;
    const isDraft = session?.is_draft === true && session.status === "Draft";
    const staged = isDraft && typeof session.draft_prompt === "string" ? session.draft_prompt : "";
    const key = isDraft ? "draft:" + staged : "";
    if ((node.dataset.draft ?? "") === key) {
      return false;
    }
    node.dataset.draft = key;
    node.hidden = !isDraft;
    if (!isDraft) {
      node.replaceChildren();

      return true;
    }
    node.replaceChildren(
      element(document, "p", "eyebrow", "Draft session"),
      element(document, "p", "draft-note", staged ? DRAFT_STAGED_NOTE : DRAFT_EMPTY_NOTE),
    );
    if (staged) {
      node.append(element(document, "div", "msg msg--user", staged));
    }

    return true;
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
      renderAnswer(item, content, document, { window });
    } else {
      item.textContent = content;
    }

    return item;
  }

  // `tag` with class `className`, holding `text` rendered as inline
  // Markdown, whose bare code elements take the shared `.prose` style.
  function inlineMarkdown(tag, className, text) {
    const node = element(document, tag, className + " md-inline prose");
    node.append(renderMarkdown(parseInlineMarkdown(text), document, { window }));

    return node;
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
    forgetAnswerLookups();
    elements.answerError.textContent = "";
    elements.questions.hidden = questions.length === 0;
    elements.questions.replaceChildren(
      ...questions.map((question, index) => renderQuestion(question, index, questions.length)),
    );

    return true;
  }

  // Ends the file lookups of answer fields that are going, so a file load
  // still on its way shows nothing.
  function forgetAnswerLookups() {
    for (const lookup of state.answerLookups) {
      lookup.reset();
    }
    state.answerLookups = [];
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
    const field = element(document, "div", "answer-field");
    field.append(input);
    group.append(field);
    lookUpFiles(input, field, index);

    return group;
  }

  // Wires the `@` file lookup of the composer to answer `input`, with its
  // list in `field`. An open list takes `Enter`, so picking a file does
  // not send the answers.
  function lookUpFiles(input, field, index) {
    const suggestions = createSuggestions({
      container: field,
      document: document,
      id: "answer-suggestions-" + index,
      input: input,
      label: "Files",
    });
    const mentions = createMentions({
      context: () => ({
        capabilities: state.capabilities,
        isConnected: state.isConnected,
        session: state.session,
      }),
      input: input,
      request: request,
      setText: (text, caret) => {
        input.value = text;
        input.setSelectionRange(caret, caret);
        renderAnswerBar();
      },
      showError: (message) => {
        elements.answerError.textContent = message;
      },
      suggestions: suggestions,
    });
    state.answerLookups.push(mentions);
    input.addEventListener("input", () => {
      renderAnswerBar();
      mentions.update();
    });
    input.addEventListener("keydown", (event) => {
      suggestions.handleKey(event);
    });
    for (const type of ["click", "focus", "keyup"]) {
      input.addEventListener(type, () => mentions.update());
    }
    input.addEventListener("blur", () => mentions.close());
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

  // Lists the queued chat messages, then the workflow actions the host
  // queued, such as a sync or a model change, marked as actions. Older
  // hosts send no actions.
  function renderQueued(messages, actions) {
    const queued = [
      ...messages.map((text) => ({ isAction: false, text: String(text) })),
      ...actions.map((text) => ({ isAction: true, text: String(text) })),
    ];
    elements.queuedBox.hidden = queued.length === 0;
    const items = [...elements.queued.children];
    queued.forEach((entry, index) => {
      const old = items[index];
      const isAction = old?.classList.contains("queued-action");
      if (old?.title === entry.text && isAction === entry.isAction) {
        return;
      }
      const item = element(document, "li");
      item.title = entry.text;
      if (entry.isAction) {
        item.classList.add("queued-action");
        item.append(element(document, "b", "queued-tag", "Action"));
      }
      item.append(element(document, "span", null, entry.text));
      if (old) {
        old.replaceWith(item);
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
      isFinePointer: composer.isFinePointer(),
    });
    elements.composer.hidden = dock.mode !== "compose";
    elements.answerBar.hidden = dock.mode !== "answer";
    elements.readonly.hidden = dock.mode !== "readonly";
    composer.refresh();
    for (const lookup of state.answerLookups) {
      lookup.refresh();
    }
    if (dock.mode === "readonly") {
      elements.readonly.textContent = dock.text;
    }
    if (dock.mode === "answer") {
      renderAnswerBar();
    }
    if (dock.mode !== "compose") {
      return;
    }
    composer.render({
      hint: dock.hint,
      isSending: state.isSending,
      placeholder: dock.placeholder,
      sendLabel: dock.isQueueing ? "Queue message" : "Send",
    });
    elements.stop.hidden = !dock.showStop;
    elements.stop.disabled = !state.isConnected || isActing("stop_turn");
  }

  function reset() {
    const previous = state.session;
    state.session = null;
    state.rendered = [];
    state.questions = [];
    state.questionsKey = null;
    state.picks = [];
    forgetAnswerLookups();
    elements.transcript.replaceChildren();
    renderDraft(null);
    renderFocusedReview(null);
    renderStatusLine(null);
    elements.queued.replaceChildren();
    elements.queuedBox.hidden = true;
    elements.questions.replaceChildren();
    elements.questions.hidden = true;
    elements.jump.hidden = true;
    elements.gone.hidden = true;
    elements.scroller.hidden = false;
    elements.dock.hidden = false;
    diff?.close();
    closeGraphs();
    renderGraphSummary(document, elements.graphSummary, null);
    state.chatPlace = null;
    elements.composer.hidden = false;
    elements.answerBar.hidden = true;
    elements.readonly.hidden = true;
    elements.stop.hidden = true;
    elements.sessionError.textContent = "";
    elements.answerError.textContent = "";
    elements.actionError.textContent = "";
    elements.actionNote.replaceChildren();
    state.shownPublish = null;
    elements.badge.hidden = true;
    elements.sub.replaceChildren();
    state.summary = null;
    renderSheetDetails(null);
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
    // The sheet stays open for home's new session.
    if (previous) {
      settings?.close();
    }
    composer.reset();
    renderActions();
  }

  // Opens `sessionId`. `failure`, when a first message from home did not
  // go through, keeps its text as the draft and says why.
  function open(sessionId, failure = null) {
    reset();
    state.sessionId = sessionId;
    showPublished();
    if (failure) {
      composer.keepDraft(sessionId, failure.draft);
      elements.sessionError.textContent = failure.error;
    }
    composer.load(sessionId);
  }

  function close() {
    reset();
    state.sessionId = null;
  }

  function render(session, summary = null) {
    if (!session) {
      renderGone();

      return;
    }
    const isFirst = state.session === null;
    adoptSnapshot(session, summary);
    // The header closes the diff or the graphs when they are no longer
    // offered.
    renderHeader(session);
    const isPanelOpen = Boolean(diff?.isOpen() || graph?.isOpen());
    const wasNearEnd = isNearEnd();
    elements.scroller.hidden = isPanelOpen;
    const changes = renderSessionBody(session);
    if (isPanelOpen) {
      refreshPanel(session, changes);
    } else {
      followChanges(changes, isFirst, wasNearEnd);
    }
  }

  // Shows that the open session no longer exists.
  function renderGone() {
    reset();
    elements.heading.textContent = "Session not found";
    elements.gone.hidden = false;
    elements.scroller.hidden = true;
    elements.composer.hidden = true;
  }

  // Keeps `session` and its listing `summary` as the open snapshot, and
  // ends what the snapshot shows to be over.
  function adoptSnapshot(session, summary) {
    state.session = session;
    state.summary = summary?.id === session.id ? summary : null;
    state.clockSkew = clockSkew(state.summary, now());
    if (state.isLoadError) {
      // A snapshot arrived, so the earlier refresh failure is over.
      state.isLoadError = false;
      elements.actionError.textContent = "";
    }
    expirePublished(session);
    // The turn a publish waited for is over, so it ran; the header's
    // review link is the lasting record.
    if (!QUEUEING_STATUSES.has(session.status)) {
      state.queuedPublishes.delete(session.id);
    }
    elements.gone.hidden = true;
  }

  // Shows everything below the header, and returns what changed: the
  // transcript, the questions, and the `layout` of the status line and
  // the graph summary above the composer, which move the transcript's end.
  function renderSessionBody(session) {
    const summaryChanged = renderGraphSummary(document, elements.graphSummary, session.graph ?? null);
    const draftChanged = renderDraft(session);
    const messagesChanged = renderTranscript(session.messages || []);
    const reviewChanged = renderFocusedReview(session.focused_review);
    const statusLineChanged = renderStatusLine(session.status, session.progress);
    const changes = {
      layout: statusLineChanged || summaryChanged,
      questions: renderQuestions(session.questions || []),
      transcript: messagesChanged || reviewChanged || draftChanged,
    };
    renderQueued(session.queued_messages || [], session.queued_actions || []);
    renderDock();
    settings?.update(session, { isRunning: statusInfo(session.status).running });

    return changes;
  }

  // Brings the open diff or graphs up to date with `session`. The
  // transcript is out of sight; returning to it says what arrived.
  function refreshPanel(session, changes) {
    state.chatPlace.hasMissed ||= changes.transcript || changes.questions;
    if (!diff?.isOpen()) {
      // Each snapshot follows a change, and graph work rarely shows in
      // the session's own fields, so the open graphs are read again.
      void graph.refresh();

      return;
    }
    // The files are read again only when the session's changes may have.
    const key = diffKey(session, state.summary);
    if (key !== state.diffKey) {
      state.diffKey = key;
      void diff.refresh();
    }
  }

  // The status line and the graph summary follow the end only for a
  // reader already there; they alone are no news worth the jump button:
  // the summary shows beside the transcript, not in it.
  function followChanges(changes, isFirst, wasNearEnd) {
    const hasNews = changes.transcript || changes.questions;
    if (isFirst || (wasNearEnd && (hasNews || changes.layout))) {
      scrollToEnd();
    } else if (hasNews) {
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
      composer.forgetDraft(sessionId, message);
      if (state.sessionId === sessionId) {
        composer.clearText(message);
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

  // Whether request `method` runs for the open session.
  function isActing(method) {
    return state.acting.has(actionKey(method, state.sessionId));
  }

  // Sends request `method` for `sessionId`, at most once at a time per
  // session, and shows its outcome in `errorNode` only while that session
  // is open.
  async function runAction(sessionId, method, errorNode) {
    const key = actionKey(method, sessionId);
    if (!sessionId || state.acting.has(key)) {
      return;
    }
    state.acting.add(key);
    renderActions();
    renderDock();
    try {
      await request({ method: method, session_id: sessionId });
      if (state.sessionId === sessionId) {
        errorNode.textContent = "";
      }
    } catch (error) {
      if (state.sessionId === sessionId) {
        errorNode.textContent = error.message;
      }
    } finally {
      state.acting.delete(key);
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

  elements.stop.addEventListener("click", () => {
    void runAction(state.sessionId, "stop_turn", elements.sessionError);
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
  // The header line's width changes with the window, the sidebar and the
  // drawer, and with it which parts fit.
  if (window.ResizeObserver) {
    new window.ResizeObserver(markWrappedParts).observe(elements.sub);
  }
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
    } else if (choice === "graphs") {
      toggleGraphs();
    }
  });
  elements.diff.addEventListener("click", toggleDiff);
  elements.graphs.addEventListener("click", toggleGraphs);
  elements.graphSummary.addEventListener("click", () => {
    if (!graph?.isOpen()) {
      toggleGraphs();
    }
  });
  // The back gesture pops the entry the graphs added and returns to the
  // chat.
  window.addEventListener("popstate", () => {
    state.isLeavingGraphs = false;
    if (state.graphEntry && window.history.state?.graphs !== true) {
      showGraphs(false);
    }
  });
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
      void runAction(sessionId, "sync_session", elements.actionError);
    }
  });
  elements.cancelDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.cancelDialog, "confirm");
    if (sessionId) {
      void runAction(sessionId, "cancel_session", elements.actionError);
    }
  });
  elements.mergeDialog.addEventListener("close", () => {
    const sessionId = confirmedSession(elements.mergeDialog, "confirm");
    if (sessionId) {
      void runAction(sessionId, "merge_session", elements.actionError);
    }
  });

  return {
    close,
    // Keeps `text` as the unsent message of `sessionId`, for when it opens.
    keepDraft: (sessionId, text) => composer.keepDraft(sessionId, text),
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

// The key of a request `method` in flight for `sessionId`.
function actionKey(method, sessionId) {
  return method + "\u0000" + sessionId;
}

// What identifies a review request for the publish outcome: its id, state
// and address.
function reviewKey(reviewRequest) {
  return reviewRequest
    ? JSON.stringify([reviewRequest.display_id, reviewRequest.state, reviewRequest.web_url])
    : "";
}

// What `session` says about its publication, or `null` before a snapshot.
function publicationKey(session) {
  return session
    ? JSON.stringify([session.published_upstream_ref ?? null, reviewKey(session.review_request)])
    : null;
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
// The `.timer` and `.started` children go, and `renderTimer` adds them
// again last.
function updateChildren(container, nodes) {
  for (const part of TIMING_PARTS) {
    container.querySelector("." + part)?.remove();
  }
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
// from the listing, a merge conflict with the base branch, the branch and
// its base with how far apart they are, the remote the branch was
// published to with how far apart those are, the model, and the review
// request as a link. Whatever the host omits is left out. The branches
// and model give way first on a phone. When the line is full, app.css
// drops whole parts by priority, so their order here is the reading
// order, not the priority.
function headerDetails(document, session, summary) {
  const figures = sessionFigures(summary);
  const branches = sessionBranches(session);
  const settings = modelSetting(session, summary);

  return [
    figures.size ? element(document, "span", "size", "[" + figures.size + "]") : null,
    figures.lines ? lineCounts(document, figures.lines) : null,
    figures.hasConflict ? conflictPart(document, branches.base) : null,
    branches.branch ? branchPart(document, branches, figures.base) : null,
    branches.upstream ? upstreamPart(document, branches, figures.upstream) : null,
    settings ? wideOnly(modelNode(document, settings)) : null,
    reviewNode(document, session.review_request),
  ].filter(Boolean);
}

// The session's branch, its base branch, and the upstream ref it was
// published to, each `""` when the host omits it.
function sessionBranches(session) {
  return {
    base: textOrEmpty(session.base_branch),
    branch: textOrEmpty(session.branch),
    upstream: textOrEmpty(session.published_upstream_ref),
  };
}

// `value` when it is a string, otherwise `""`.
function textOrEmpty(value) {
  return typeof value === "string" ? value : "";
}

// The model part of `settingsParts`, or `undefined` when the host names
// no model.
function modelSetting(session, summary) {
  return settingsParts(session, summary).find((part) => part.type === "model");
}

// `node`, hidden on a phone.
function wideOnly(node) {
  node.classList.add("wide-only");

  return node;
}

// `branch → base`, or the branch alone without a base.
function branchLabel({ base, branch }) {
  return base ? branch + " → " + base : branch;
}

// `node` followed by `counts` against `target`, when there are counts.
function withCounts(document, node, counts, target) {
  if (counts) {
    node.append(" ", branchCountsNode(document, counts, target));
  }

  return node;
}

// The header's merge conflict. A phone has room for the conflict, not
// for the branch it is with.
function conflictPart(document, base) {
  const conflict = conflictLabel(document, "Merge conflict");
  if (base) {
    conflict.append(element(document, "span", "wide-only", " with " + base));
  }

  return conflict;
}

// The header's branch and its base with their counts, as one part, so a
// full line drops them together. A long name may be cut short.
function branchPart(document, branches, counts) {
  const label = branchLabel(branches);
  const node = element(document, "span", "branch wide-only");
  const name = element(document, "span", "branch-name", label);
  name.title = label;
  node.append(name);

  return withCounts(document, node, branches.base ? counts : null, branches.base);
}

// The header's upstream with its counts, as one part. An upstream named
// after the session branch, such as `origin/wt/3f9a1c2e` for branch
// `wt/3f9a1c2e`, shows as its remote, `origin`.
function upstreamPart(document, { branch, upstream }, counts) {
  const suffix = "/" + branch;
  const remote = branch && upstream.endsWith(suffix) ? upstream.slice(0, -suffix.length) : upstream;
  const node = element(document, "span", "upstream wide-only", "⇡ " + remote);
  node.title = "Published to " + upstream;

  return withCounts(document, node, counts, upstream);
}

// The labelled rows of the `⋯` sheet's details, as `{ label, value,
// isTiming }`, from the same helpers as the header: the branch and its
// base with their counts, the whole upstream ref with its counts, the
// model, the active time and age from `timing`, and the review request as
// a link. Whatever the host omits is left out.
function sheetDetailRows(document, session, summary, timing) {
  const figures = sessionFigures(summary);
  const branches = sessionBranches(session);
  const model = modelSetting(session, summary);
  const review = reviewNode(document, session.review_request);

  return [
    branches.branch ? branchRow(document, branches, figures.base) : null,
    branches.upstream ? upstreamRow(document, branches.upstream, figures.upstream) : null,
    model ? { label: "Model", value: modelNode(document, model) } : null,
    ...timingRows(document, timing),
    review ? { label: "Pull request", value: review } : null,
  ].filter(Boolean);
}

// The sheet's branch row: the branch and its base with their counts.
function branchRow(document, branches, counts) {
  const value = element(document, "span", null, branchLabel(branches));

  return {
    label: "Branch",
    value: withCounts(document, value, branches.base ? counts : null, branches.base),
  };
}

// The sheet's upstream row: the whole ref with its counts.
function upstreamRow(document, upstream, counts) {
  const value = element(document, "span", null, upstream);

  return { label: "Upstream", value: withCounts(document, value, counts, upstream) };
}

// The sheet's timer and age rows, which `renderTimer` updates in place.
function timingRows(document, timing) {
  return Object.entries(sheetTimingValues(timing)).map(([part, value]) => ({
    isTiming: true,
    label: part === "timer" ? "Timer" : "Started",
    value: element(document, "span", "sheet-" + part, value),
  }));
}

// The sheet's values for the header's timing parts it has, keyed `timer`
// and `started`: the active time, and the age without the header's
// `Started` that the row's label says.
function sheetTimingValues(timing) {
  const values = {};
  if (timing.timer !== null) {
    values.timer = timing.timer;
  }
  if (timing.started !== null) {
    values.started = timing.started.replace(/^Started /, "");
  }

  return values;
}

// How far the session branch is from `target`, such as `↓1 ↑3`, in the
// muted tone of the terminal's footer counts.
function branchCountsNode(document, counts, target) {
  const node = element(document, "span", "branch-counts", counts);
  node.title = counts === "✓" ? "Even with " + target : "Behind and ahead of " + target;

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
