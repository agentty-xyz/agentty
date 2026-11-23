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
import { createSlashCommands } from "./slash.mjs";
import { createSuggestions } from "./suggest.mjs";

// Unsent text of one session, in `sessionStorage`, under this prefix and
// the session id.
export const DRAFT_KEY_PREFIX = "agentty.remote.draft.";
// Most rows the composer grows to.
export const MAX_COMPOSER_ROWS = 8;

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
// `clearText(text)`, `isFinePointer()` and `reset()`.
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
      return { ...current, session: elements.form.hidden || isLocked ? null : current.session };
    },
    input: elements.message,
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

  return {
    // Forgets the unsent text in `text` once it went out: the draft of
    // `draftId` when it still holds it.
    forgetDraft: (draftId, text) => {
      if (drafts.get(draftId) === text) {
        drafts.clear(draftId);
      }
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
    isFinePointer,
    keepDraft: (draftId, text) => drafts.set(draftId, text),
    load: (draftId) => {
      stopBrowsing();
      elements.message.value = draftId ? drafts.get(draftId) : "";
      resize();
    },
    // The suggestions follow the session, the host and the composer.
    refresh: () => {
      slash.refresh();
      mentions.refresh();
    },
    render: ({ hint, placeholder, sendLabel, isSending = false, isLocked: locked = false }) => {
      isLocked = locked;
      elements.message.placeholder = placeholder;
      renderMeta(hint);
      elements.send.disabled = !context().isConnected || isSending;
      elements.send.setAttribute("aria-label", sendLabel);
    },
    reset: () => {
      stopBrowsing();
      isLocked = false;
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
