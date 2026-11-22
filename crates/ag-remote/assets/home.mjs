// Home composer of the remote control client: with the `host` and
// `create_session` capabilities, home asks what the agent should work on,
// creates a session in the host's active project, sends it the first
// message, and opens it. The message box is the open session's composer
// from composer.mjs, for the session about to start: with the `settings`
// capability too, its settings line and slash commands pick the model,
// reasoning level, answer style and speed among those of the agent the
// chosen mode starts on, and the picks are applied with
// `set_session_settings` before the first message goes out. The new
// session has no worktree yet, so with the `project_files` capability
// `@` lists the active project's checkout, as the terminal does for a
// draft. The project name, base
// branch and models come from `get_host` and only ever become text
// nodes. Functions take `document` and `window` as parameters, so the
// Node tests in crates/ag-remote/tests/web/home.test.mjs pass the fake
// DOM.

import { composerState } from "./chat.mjs";
import { createComposer } from "./composer.mjs";
import { currentChoice, offersSettings, parseModels, settingsChange } from "./settings.mjs";

// Where the unsent first message waits, among the per-session drafts.
export const HOME_DRAFT_ID = "home";
// What each session mode `get_host` lists is called on the page.
const MODE_LABELS = { harness: "Harness", regular: "Regular" };
// Settings a pick may change, as `set_session_settings` and a session
// snapshot name them.
const PICKED_FIELDS = {
  model: "model_id",
  reasoning: "reasoning",
  response_style: "response_style",
  speed_mode: "speed_mode",
};

// Whether the host lets this device start sessions from home.
export function offersCreation(capabilities) {
  return capabilities.includes("host") && capabilities.includes("create_session");
}

// The host's answer to `get_host`, with the fields home uses, or `null`
// when it is not one.
export function parseHost(result) {
  if (result?.kind !== "host" || typeof result.project !== "string") {
    return null;
  }
  const modes = Array.isArray(result.session_modes)
    ? result.session_modes.filter((mode) => typeof mode === "string")
    : [];
  // What each mode starts on, from a host that names it.
  const modeModels = {};
  for (const entry of Array.isArray(result.mode_models) ? result.mode_models : []) {
    if (
      !modes.includes(entry?.mode) ||
      typeof entry.agent !== "string" ||
      typeof entry.model !== "string" ||
      entry.model === ""
    ) {
      continue;
    }
    modeModels[entry.mode] = {
      agent: entry.agent,
      id: entry.model,
      name: nonEmpty(entry.name) ?? entry.model,
      options: parseModels(result, entry.agent),
      reasoning: nonEmpty(entry.reasoning),
      speed: nonEmpty(entry.speed_mode),
      style: nonEmpty(entry.response_style),
    };
  }

  return {
    baseBranch: typeof result.base_branch === "string" ? result.base_branch : "",
    model: typeof result.default_model === "string" ? result.default_model : "",
    modeModels: modeModels,
    modes: modes,
    project: result.project,
  };
}

// The session a new session in `mode` would be, as a snapshot names its
// settings, with `picks` applied: `set_session_settings` fields the user
// chose. `null` when the host names no model for the mode.
export function newSession(host, mode, picks = {}) {
  const start = host?.modeModels[mode];
  if (!start) {
    return null;
  }
  const session = {
    agent: start.agent,
    // Ids differ per mode, so models loaded for one agent are not reused
    // for another.
    id: "new:" + mode,
    model: start.name,
    model_id: start.id,
    reasoning: start.reasoning,
    response_style: start.style,
    speed_mode: start.speed,
    status: "Draft",
  };
  for (const [field, key] of Object.entries(PICKED_FIELDS)) {
    if (picks[field]) {
      session[key] = picks[field];
    }
  }
  if (session.model_id !== start.id) {
    const option = start.options.find((model) => model.id === session.model_id);
    session.model = option?.name ?? session.model_id;
  }

  return session;
}

// The label of session mode `mode`; a mode from a newer host shows as it
// is named.
export function modeLabel(mode) {
  return Object.hasOwn(MODE_LABELS, mode) ? MODE_LABELS[mode] : mode;
}

// Wires the home composer. `elements` holds `form`, `message`, `meta`,
// `settingsButton`, `hint`, `modes`, `send` and `error`, the nodes with the
// ids named in index.html. `request(body)` sends one request and resolves
// with its result. `settings` is the settings sheet from settings.mjs,
// shared with the open session. `onHost(host)` runs whenever the host
// description changes, `null` when home cannot create sessions.
// `onCreated(sessionId, failure)` opens a new session; `failure` is `null`
// when the first message went through, and otherwise `{ draft, error }`
// with the text to keep and why it failed.
//
// Returns `setCapabilities(list)`, which loads the host when it is
// offered, `setConnected(isConnected)`, `focus()`, and `host()`.
export function createHome({
  document,
  window,
  elements,
  request,
  onHost,
  onCreated,
  settings = null,
}) {
  const state = {
    capabilities: [],
    host: null,
    isConnected: false,
    isCreating: false,
    // Counts host loads, so an answer for an older one is dropped.
    loads: 0,
    mode: null,
    // Settings picked for the new session, as `set_session_settings`
    // names them.
    picks: {},
  };
  const composer = createComposer({
    // A pick waits for the session; nothing is sent until it exists.
    applySettings: async (_session, change) => {
      state.picks = { ...state.picks, ...change };
      render();
    },
    context: () => ({
      capabilities: state.capabilities,
      draftId: HOME_DRAFT_ID,
      fileScope: "project",
      isConnected: state.isConnected,
      isRunning: false,
      session: chosenSession(),
      summary: null,
    }),
    document: document,
    elements: {
      form: elements.form,
      hint: elements.hint,
      message: elements.message,
      meta: elements.meta,
      send: elements.send,
      settingsButton: elements.settingsButton,
    },
    onSend: () => void create(),
    request: request,
    settings: settings,
    showError: (message) => {
      elements.error.textContent = message;
    },
    suggestionsId: "home-message-suggestions",
    window: window,
  });

  function render() {
    const host = state.host;
    elements.form.hidden = !host;
    elements.modes.hidden = !host || host.modes.length < 2;
    if (host) {
      const dock = composerState("Draft", {
        isConnected: state.isConnected,
        isFinePointer: composer.isFinePointer(),
      });
      composer.render({
        hint: dock.hint,
        isLocked: state.isCreating,
        isSending: state.isCreating,
        placeholder: dock.placeholder,
        sendLabel: "Start session",
      });
    } else {
      elements.hint.textContent = "";
      elements.send.disabled = true;
    }
    composer.refresh();
    elements.modes.replaceChildren(
      ...(!host || host.modes.length < 2 ? [] : host.modes.map((mode) => modeButton(mode))),
    );
  }

  function modeButton(mode) {
    const button = element(document, "button", null, modeLabel(mode));
    button.type = "button";
    button.dataset.mode = mode;
    button.setAttribute("aria-pressed", String(mode === state.mode));
    button.addEventListener("click", () => {
      // Another mode may start on another agent, so its model and speed
      // picks do not carry over.
      if (mode !== state.mode) {
        const kept = { ...state.picks };
        delete kept.model;
        delete kept.speed_mode;
        state.picks = kept;
      }
      state.mode = mode;
      render();
    });

    return button;
  }

  async function loadHost() {
    const load = ++state.loads;
    let host = null;
    if (offersCreation(state.capabilities)) {
      try {
        host = parseHost(await request({ method: "get_host" }));
        elements.error.textContent = "";
      } catch (error) {
        elements.error.textContent = "Could not describe the project: " + error.message;
      }
    }
    if (load !== state.loads) {
      return;
    }
    state.host = host;
    // The chosen mode stays while the host still offers it.
    if (!host?.modes.includes(state.mode)) {
      state.mode = host?.modes[0] ?? null;
    }
    render();
    onHost(host);
  }

  // The new session with the picks applied while the host lets this
  // device change settings.
  function chosenSession() {
    const picks = offersSettings(state.capabilities) ? state.picks : {};

    return newSession(state.host, state.mode, picks);
  }

  // The settings the new session should switch to after it starts, as a
  // `set_session_settings` change; empty when it keeps its own.
  function pickedChange() {
    const start = newSession(state.host, state.mode);
    if (!start) {
      return {};
    }

    return settingsChange(start, currentChoice(chosenSession()), state.host.modeModels[state.mode].options);
  }

  async function create() {
    const text = elements.message.value;
    if (!text.trim() || !state.host || state.isCreating || !state.isConnected) {
      return;
    }
    const change = pickedChange();
    state.isCreating = true;
    elements.error.textContent = "";
    render();
    const body = { method: "create_session" };
    if (state.mode) {
      body.mode = state.mode;
    }
    let sessionId;
    try {
      sessionId = (await request(body)).session_id;
    } catch (error) {
      elements.error.textContent = error.message;
      state.isCreating = false;
      render();

      return;
    }
    // The new session holds the text from here on, sent or not. Text typed
    // while the session was being created stays.
    composer.forgetDraft(HOME_DRAFT_ID, text);
    composer.clearText(text);
    // The first message waits for the picked settings, so the first turn
    // runs with them; when they fail, the message stays unsent.
    let failure = null;
    if (Object.keys(change).length > 0) {
      try {
        await request({ method: "set_session_settings", session_id: sessionId, ...change });
      } catch (error) {
        failure = { draft: text, error: "Could not apply the settings: " + error.message };
      }
    }
    if (!failure) {
      try {
        await request({ method: "send_message", session_id: sessionId, message: text });
      } catch (error) {
        failure = { draft: text, error: error.message };
      }
    }
    state.isCreating = false;
    render();
    onCreated(sessionId, failure);
  }

  composer.load(HOME_DRAFT_ID);

  return {
    focus: () => {
      if (state.host) {
        elements.message.focus();
      }
    },
    host: () => state.host,
    setCapabilities: (capabilities) => {
      state.capabilities = capabilities;

      return loadHost();
    },
    setConnected: (isConnected) => {
      state.isConnected = isConnected;
      render();
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

// `value` when it is a non-empty string, otherwise `null`.
function nonEmpty(value) {
  return typeof value === "string" && value !== "" ? value : null;
}
