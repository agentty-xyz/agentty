// Message composer of the remote control client, shared by the open
// session and home's first message: the growing text field, the settings
// line that opens the settings sheet, the key hint, the send button, the
// suggestion popup with the slash commands and the `@` file lookup, and
// `Up` and `Down` through the session's earlier prompts.
// Setting names and file paths only ever become text nodes. Functions
// take `document` and `window` as parameters, so the Node tests in
// crates/ag-remote/tests/web/ pass the fake DOM.

import { settingsParts } from "./format.mjs";
import { createMentions } from "./mention.mjs";
import { separator } from "./session_list.mjs";
import { offersSettings } from "./settings.mjs";
import { createSlashCommands, offersGraph } from "./slash.mjs";
import { createSuggestions } from "./suggest.mjs";

// Unsent text of one session, in `sessionStorage`, under this prefix and
// the session id.
export const DRAFT_KEY_PREFIX = "agentty.remote.draft.";
// Marks, under this prefix and the session id, a draft `/graph` armed.
export const DRAFT_GRAPH_KEY_PREFIX = "agentty.remote.draft-graph.";
// Most rows the composer grows to.
export const MAX_COMPOSER_ROWS = 8;
// What an armed `/graph` composer asks for, as the terminal's does.
export const GRAPH_PLACEHOLDER = "Describe the task for the execution graph";

// What the `Graph` marker says while armed, and while the session or the
// host takes no graph request.
export const GRAPH_ARMED_NOTE = "Sending asks for a graph proposal";
export const GRAPH_BLOCKED_NOTE = "Graph requests unavailable now";

// The `placeholder`, `hint` and `sendLabel` a composer shows for `dock`:
// its own, or the graph request's while `/graph` has armed it. A graph
// task the session or the host cannot take now, `isGraphBlocked`, says so
// and how to return to chat.
export function composerLabels(dock, { isFinePointer, isGraphArmed, isGraphBlocked = false }) {
  if (!isGraphArmed) {
    return { hint: dock.hint, placeholder: dock.placeholder, sendLabel: dock.sendLabel };
  }
  if (isGraphBlocked) {
    const leave = isFinePointer ? "Esc" : "×";

    return {
      hint: GRAPH_BLOCKED_NOTE + " · " + leave + " returns to chat",
      placeholder: GRAPH_PLACEHOLDER,
      sendLabel: GRAPH_BLOCKED_NOTE,
    };
  }

  return {
    hint: isFinePointer ? "Enter asks for a graph proposal · Esc returns to chat" : "Sending asks for a graph proposal",
    placeholder: GRAPH_PLACEHOLDER,
    sendLabel: "Ask for a graph proposal",
  };
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

// Which way an arrow key moves through the earlier prompts, as in the
// terminal: `back` for `ArrowUp` with the caret on the first line of
// `text`, `forward` for `ArrowDown` with it on the last line, or `null` to
// let the browser move the caret. Keys with a modifier, and keys while an
// input method is composing, move nothing.
export function historyKeyAction(event, text, selectionStart, selectionEnd = selectionStart) {
  const isPlain =
    !event.altKey && !event.ctrlKey && !event.metaKey && !event.shiftKey &&
    !event.isComposing && event.keyCode !== 229;
  if (isPlain && event.key === "ArrowUp" && !text.slice(0, selectionStart).includes("\n")) {
    return "back";
  }
  if (isPlain && event.key === "ArrowDown" && !text.slice(selectionEnd).includes("\n")) {
    return "forward";
  }

  return null;
}

// The prompts sent in `session`, oldest first: the text of its `user`
// messages, as `session_prompt_history_entries` in
// crates/agentty/src/runtime/mode/session_view.rs lists them.
export function promptHistory(session) {
  const messages = Array.isArray(session?.messages) ? session.messages : [];

  return messages
    .filter((message) => message?.kind === "user" && typeof message.content === "string")
    .map((message) => message.content);
}

// Rows the composer shows for `text`: one per line, from 1 to
// `MAX_COMPOSER_ROWS`.
export function composerRows(text) {
  return Math.min(MAX_COMPOSER_ROWS, Math.max(1, String(text).split("\n").length));
}

// Unsent text per session, in memory and in `sessionStorage`, so a draft
// survives switching sessions and reloading the tab, and whether `/graph`
// armed it, so a graph task never comes back as a chat message. Storage
// failures leave the copy in memory.
export function createDrafts(window) {
  const drafts = new Map();
  const graphDrafts = new Map();

  // Stores `value` under `key`, or removes the key when `value` is empty.
  function store(key, value) {
    try {
      if (value) {
        window.sessionStorage.setItem(key, value);
      } else {
        window.sessionStorage.removeItem(key);
      }
    } catch (_error) {
      // The copy in memory still lasts for this page.
    }
  }

  // The stored value under `key`, or `""`.
  function stored(key) {
    try {
      return window.sessionStorage.getItem(key) || "";
    } catch (_error) {
      return "";
    }
  }

  return {
    clear: (sessionId) => {
      drafts.delete(sessionId);
      graphDrafts.delete(sessionId);
      store(DRAFT_KEY_PREFIX + sessionId, "");
      store(DRAFT_GRAPH_KEY_PREFIX + sessionId, "");
    },
    get: (sessionId) => (drafts.has(sessionId) ? drafts.get(sessionId) : stored(DRAFT_KEY_PREFIX + sessionId)),
    isGraph: (sessionId) =>
      graphDrafts.has(sessionId) ? graphDrafts.get(sessionId) : stored(DRAFT_GRAPH_KEY_PREFIX + sessionId) === "1",
    set: (sessionId, text) => {
      drafts.set(sessionId, text);
      store(DRAFT_KEY_PREFIX + sessionId, text);
    },
    setGraph: (sessionId, isArmed) => {
      graphDrafts.set(sessionId, isArmed);
      store(DRAFT_GRAPH_KEY_PREFIX + sessionId, isArmed ? "1" : "");
    },
  };
}

// The model with its reasoning level, as `claude/opus-5.5 [high]`.
export function modelNode(document, part) {
  const node = element(document, "span", "model");
  node.append(element(document, "b", null, part.label));
  if (part.reasoning) {
    node.append(" [" + part.reasoning + "]");
  }

  return node;
}

// Wires one composer. `elements` holds `form`, `message`, `meta`,
// `settingsButton`, `send` and `hint`; `suggestionsId` names the popup.
// With `graphMode`, the `Graph` marker, `graphNote`, its note, and
// `graphLeave`, its button that leaves graph mode, the slash menu also
// offers `/graph` for a session whose host has the `graph_proposal`
// capability. It arms the composer: the next send asks for a graph
// proposal of the typed task, as `isGraphArmed()` says, until
// `disarmGraph(draftId)`, the marker's button or `Escape` ends it,
// keeping the text. Graph mode belongs to the draft, so it comes back
// with the draft after a session switch or a reload. While the host or
// the session takes no graph request, as `isGraphBlocked()` says, the
// marker says so and sending is off until graph mode ends.
//
// `context()` returns what the composer writes for: `session`, whose
// settings the line shows and the slash commands change, or `null`;
// `summary`, its entry in the listing; `draftId`, where unsent text is
// kept, or `null`; `fileScope`, where `@` looks up files: `"session"`,
// the default, for the session's worktree, `"project"` for the project
// checkout of a session yet to start, or `null` for none;
// `isRunning`, for the settings sheet; and the host's `capabilities` and
// `isConnected`. `history()` returns the prompts `Up` and `Down` browse,
// oldest first, as `promptHistory` lists them; home's composer has none.
//
// `request(body)` sends one request and resolves with its result.
// `applySettings(session, change)` applies a `set_session_settings`
// change and resolves once it has; it sends the request unless given.
// `onSend()` runs on `Enter` or submit. `showError(message)` shows a
// failure, or clears it when empty. `settings` is the settings sheet from
// settings.mjs, when the page has one.
//
// Returns `render(dock)` with the `placeholder`, `hint`, `sendLabel`,
// `isSending` and `isLocked` to show, `refresh()` for a newer snapshot,
// `load(draftId)`, `keepDraft(draftId, text)`, `forgetDraft(draftId, text)`,
// `clearText(text)`, `isFinePointer()`, `isGraphArmed()`,
// `isGraphBlocked()`, `disarmGraph(draftId)` and `reset()`.
export function createComposer({
  document,
  window,
  elements,
  request,
  context,
  onSend,
  showError,
  suggestionsId,
  applySettings = (session, change) =>
    request({ method: "set_session_settings", session_id: session.id, ...change }),
  history = () => [],
  settings = null,
}) {
  const drafts = createDrafts(window);
  // Whether a send in flight holds the settings as they are.
  let isLocked = false;
  // The dock last rendered, which arming and disarming render again.
  let lastDock = null;
  // The earlier prompt on screen, by its index in `history`, or `null`;
  // and the text it replaced, which `Down` past the newest brings back.
  // A shown prompt is not saved as the draft, so the draft survives
  // browsing; editing it ends browsing and makes it the draft, as in the
  // terminal.
  const browsing = { draft: "", index: null };
  // The suggestion popup, and the slash commands and `@` file lookup it
  // lists. The slash commands take the text first.
  const suggestions = createSuggestions({
    container: elements.form,
    document: document,
    id: suggestionsId,
    input: elements.message,
    label: "Suggestions",
  });
  const slash = createSlashCommands({
    applySettings: applySettings,
    context: () => {
      const current = context();

      // Only a composer on screen offers commands.
      return {
        ...current,
        isGraphArmed: isGraphArmed(),
        session: elements.form.hidden || isLocked ? null : current.session,
      };
    },
    input: elements.message,
    onGraph: elements.graphMode ? () => setGraphArmed(true) : null,
    request: request,
    setText: (text) => setText(text),
    showError: showError,
    suggestions: suggestions,
  });
  const mentions = createMentions({
    context: () => {
      const current = context();

      // Only a composer on screen looks up files.
      return { ...current, fileScope: elements.form.hidden ? null : (current.fileScope ?? "session") };
    },
    input: elements.message,
    request: request,
    setText: (text, caret) => {
      setText(text);
      elements.message.setSelectionRange(caret, caret);
    },
    showError: showError,
    suggestions: suggestions,
  });

  // Whether `/graph` armed the draft the box shows. Graph mode is only
  // ever read from the draft, so the marker and the text cannot part.
  function isGraphArmed() {
    const draftId = context().draftId;

    return Boolean(elements.graphMode && draftId && drafts.isGraph(draftId));
  }

  // Arms or disarms `/graph` for the draft on screen, and shows it.
  // Returns whether the state changed.
  function setGraphArmed(armed) {
    const draftId = context().draftId;
    if (!draftId || isGraphArmed() === armed) {
      return false;
    }
    drafts.setGraph(draftId, armed);
    showGraphMode();

    return true;
  }

  // Shows graph mode as the draft on screen keeps it: the marker, and
  // whether the task can go now, also before the first dock, and renders
  // the dock again once there is one.
  function showGraphMode() {
    if (!elements.graphMode) {
      return;
    }
    if (lastDock) {
      renderDock(lastDock);

      return;
    }
    renderGraphMarker(isGraphBlocked());
  }

  // Shows the marker, blocked or not, and turns sending off while blocked.
  function renderGraphMarker(isBlocked) {
    elements.graphMode.hidden = !isGraphArmed();
    elements.graphMode.classList.toggle("is-blocked", isBlocked);
    elements.graphNote.textContent = isBlocked ? GRAPH_BLOCKED_NOTE : GRAPH_ARMED_NOTE;
    if (isBlocked) {
      elements.send.disabled = true;
    }
  }

  // Whether graph mode is armed while the host or the session takes no
  // graph request.
  function isGraphBlocked() {
    const { capabilities = [], session = null } = context();

    return isGraphArmed() && !offersGraph(capabilities, session);
  }

  function renderDock(dock) {
    lastDock = dock;
    isLocked = dock.isLocked ?? false;
    const isBlocked = isGraphBlocked();
    const labels = composerLabels(dock, {
      isFinePointer: isFinePointer(),
      isGraphArmed: isGraphArmed(),
      isGraphBlocked: isBlocked,
    });
    elements.message.placeholder = labels.placeholder;
    renderMeta(labels.hint);
    elements.send.disabled = !context().isConnected || Boolean(dock.isSending);
    elements.send.setAttribute("aria-label", labels.sendLabel);
    if (elements.graphMode) {
      renderGraphMarker(isBlocked);
    }
  }

  function setText(text) {
    elements.message.value = text;
    const draftId = context().draftId;
    if (draftId) {
      drafts.set(draftId, text);
    }
    resize();
  }

  function resize() {
    elements.message.rows = composerRows(elements.message.value);
  }

  function isFinePointer() {
    return Boolean(window.matchMedia?.("(pointer: fine)").matches);
  }

  // Shows `text` in the field, with the caret at its end, without saving
  // it as the draft.
  function showText(text) {
    elements.message.value = text;
    elements.message.setSelectionRange(text.length, text.length);
    resize();
  }

  function stopBrowsing() {
    browsing.draft = "";
    browsing.index = null;
  }

  // Moves `step` (`back` or `forward`) through the earlier prompts, and
  // says whether it did. `back` starts at the newest and stops at the
  // oldest; `forward` past the newest brings back the text browsing
  // replaced.
  function browseHistory(step) {
    const prompts = history();
    if (step === "back") {
      if (prompts.length === 0) {
        return false;
      }
      if (browsing.index === null) {
        browsing.draft = elements.message.value;
      }
      browsing.index = Math.max(0, Math.min(browsing.index ?? prompts.length, prompts.length) - 1);
    } else if (browsing.index === null) {
      return false;
    } else if (browsing.index + 1 < prompts.length) {
      browsing.index += 1;
    } else {
      const draft = browsing.draft;
      stopBrowsing();
      showText(draft);

      return true;
    }
    showText(prompts[browsing.index]);

    return true;
  }

  // Opens, filters or closes the suggestions for the text and caret. An
  // earlier prompt on screen offers none, so the arrow keys keep
  // browsing.
  function updateSuggestions() {
    if (browsing.index !== null) {
      slash.close();
      mentions.close();
    } else if (slash.update()) {
      mentions.close();
    } else {
      mentions.update();
    }
  }

  // Whether the settings line opens the settings sheet now.
  function canChangeSettings() {
    const { capabilities = [], session = null } = context();

    return Boolean(settings) && Boolean(session) && offersSettings(capabilities);
  }

  // Shows the session settings in the line, or the key hint when the host
  // names none. A host that lets this device change them makes the line a
  // button that opens the settings sheet.
  function renderMeta(hint) {
    const { isConnected = false, session = null, summary = null } = context();
    const parts = settingsParts(session, summary);
    elements.hint.textContent = parts.length > 0 ? hint : "";
    const isButton = parts.length > 0 && canChangeSettings();
    elements.meta.hidden = isButton;
    elements.settingsButton.hidden = !isButton;
    elements.settingsButton.disabled = !isConnected || isLocked;
    if (parts.length === 0) {
      elements.meta.textContent = hint;

      return;
    }
    const nodes = parts.flatMap((part, index) => {
      const node =
        part.type === "model"
          ? modelNode(document, part)
          : element(document, "span", null, part.label);

      return index === 0 ? [node] : [separator(document), node];
    });
    (isButton ? elements.settingsButton : elements.meta).replaceChildren(...nodes);
  }

  function openSettings() {
    const { isConnected = false, isRunning = false, session = null } = context();
    if (!isConnected || isLocked || !canChangeSettings()) {
      return;
    }
    settings.open(session, {
      apply: (change) => applySettings(session, change),
      isRunning: isRunning,
    });
  }

  elements.message.addEventListener("input", () => {
    stopBrowsing();
    const draftId = context().draftId;
    if (draftId) {
      drafts.set(draftId, elements.message.value);
    }
    resize();
    updateSuggestions();
  });
  // An open suggestion list takes its keys first, so `Enter` picks there
  // instead of sending and the arrow keys move through it instead of the
  // earlier prompts.
  elements.message.addEventListener("keydown", (event) => {
    if (suggestions.handleKey(event)) {
      return;
    }
    // As in the terminal, `Escape` only leaves graph mode; the text stays.
    if (event.key === "Escape" && setGraphArmed(false)) {
      event.preventDefault();

      return;
    }
    const { selectionEnd, selectionStart, value } = elements.message;
    const step = historyKeyAction(event, value, selectionStart, selectionEnd);
    if (step && browseHistory(step)) {
      event.preventDefault();

      return;
    }
    if (composerKeyAction(event, { isFinePointer: isFinePointer() }) === "send") {
      event.preventDefault();
      onSend();
    }
  });
  // Moving the caret, or the focus, opens or closes the suggestions.
  for (const type of ["click", "focus", "keyup"]) {
    elements.message.addEventListener(type, updateSuggestions);
  }
  elements.message.addEventListener("blur", () => {
    slash.close();
    mentions.close();
  });
  elements.form.addEventListener("submit", (event) => {
    event.preventDefault();
    onSend();
  });
  elements.settingsButton.addEventListener("click", openSettings);
  elements.graphLeave?.addEventListener("click", () => {
    setGraphArmed(false);
    elements.message.focus();
  });

  return {
    // Forgets the unsent text in `text` once it went out: the draft of
    // `draftId` when it still holds it, with its graph mode. Returns
    // whether no unsent text of `draftId` is left, which is not the case
    // when the draft changed meanwhile or an earlier prompt went out.
    forgetDraft: (draftId, text) => {
      const draft = drafts.get(draftId);
      if (draft === text) {
        drafts.clear(draftId);
      }

      return draft === text || draft === "";
    },
    // Empties the field when it still holds `text`; text typed since
    // stays. A sent earlier prompt gives way to the draft it replaced, and
    // a draft browsing replaced that went out meanwhile is not brought
    // back.
    clearText: (text) => {
      if (browsing.draft === text) {
        browsing.draft = "";
      }
      if (elements.message.value === text) {
        const draft = browsing.draft;
        stopBrowsing();
        elements.message.value = draft;
        resize();
      }
    },
    // Ends graph mode of the draft of `draftId`, as once its graph request
    // went out, also when that draft is no longer on screen.
    disarmGraph: (draftId) => {
      drafts.setGraph(draftId, false);
      showGraphMode();
    },
    isFinePointer,
    isGraphArmed,
    isGraphBlocked,
    keepDraft: (draftId, text) => drafts.set(draftId, text),
    // Shows the draft of `draftId`, the composer's `draftId` now, in graph
    // mode when `/graph` armed it.
    load: (draftId) => {
      stopBrowsing();
      elements.message.value = draftId ? drafts.get(draftId) : "";
      resize();
      showGraphMode();
    },
    // The suggestions follow the session, the host and the composer.
    refresh: () => {
      slash.refresh();
      mentions.refresh();
    },
    render: renderDock,
    // Clears what the composer shows for another session; the draft keeps
    // its graph mode, which shows with the draft.
    reset: () => {
      stopBrowsing();
      isLocked = false;
      lastDock = null;
      showGraphMode();
      elements.hint.textContent = "";
      slash.reset();
      mentions.reset();
    },
  };
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
