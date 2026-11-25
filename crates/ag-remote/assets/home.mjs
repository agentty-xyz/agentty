// Home composer of the remote control client: with the `host` and
// `create_session` capabilities, home asks what the agent should work on,
// creates a session in the project the page shows, the host's active one
// unless the device picked another, sends it the first message, and opens
// it. The message box is the open session's composer
// from composer.mjs, for the session about to start: with the `settings`
// capability too, its settings line and slash commands pick the model,
// reasoning level, answer style and speed among those of the agent the
// chosen mode starts on, or of another agent with `settings_agent`, and
// the picks are applied with
// `set_session_settings` before the first message goes out. The new
// session has no worktree yet, so with the `project_files` capability
// `@` lists that project's checkout, as the terminal does for a
// draft. The project name, base
// branch and models come from `get_host` and only ever become text
// nodes. Functions take `document` and `window` as parameters, so the
// Node tests in crates/ag-remote/tests/web/home.test.mjs pass the fake
// DOM.

import { composerState } from "./chat.mjs";
import { createComposer } from "./composer.mjs";
import { encodeImages, numberedText } from "./image.mjs";
import { agentModels, currentChoice, offersSettings, parseModels, settingsChange } from "./settings.mjs";

// Where the unsent first message waits, among the per-session drafts.
export const HOME_DRAFT_ID = "home";
// What each session mode `get_host` lists is called on the page.
const MODE_LABELS = { harness: "Harness", regular: "Regular" };
// Why home describes nothing when Agentty describes another project than
// the one it asked for, as a server that predates `projects` would.
const OTHER_PROJECT = "Agentty described another project";
// Settings a pick may change, as `set_session_settings` and a session
// snapshot name them.
const PICKED_FIELDS = {
  agent: "agent",
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
      reasoning: nonEmpty(entry.reasoning),
      speed: nonEmpty(entry.speed_mode),
      style: nonEmpty(entry.response_style),
    };
  }

  return {
    baseBranch: typeof result.base_branch === "string" ? result.base_branch : "",
    model: typeof result.default_model === "string" ? result.default_model : "",
    modeModels: modeModels,
    models: parseModels(result),
    modes: modes,
    project: result.project,
    projectId: typeof result.project_id === "string" ? result.project_id : null,
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
    // Ids differ per Agentty and mode, so models loaded for one agent are
    // not reused for another.
    id: "new:" + (host.scope ? host.scope + ":" : "") + mode,
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
  if (session.model_id !== start.id || session.agent !== start.agent) {
    const option = agentModels(host.models, session.agent).find((model) => model.id === session.model_id);
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
// `settingsButton`, `hint`, `images`, `modes`, `send` and `error`, the nodes
// with the ids named in index.html. `request(body)` sends one request and resolves
// with its result. `settings` is the settings sheet from settings.mjs,
// shared with the open session. `onHost(host)` runs whenever the host
// description changes, `null` when home cannot create sessions.
// `onCreated(sessionId, failure)` opens a new session; `failure` is `null`
// when the first message went through, and otherwise
// `{ draft, error, images }` with the text and images to keep and why it
// failed.
//
// Returns `setCapabilities(list, scope, projectId)`, which loads the host
// when it is offered, `setConnected(isConnected)`, `focus()`, and `host()`.
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
    // The project new sessions start in, as `list_projects` names it, or
    // `null` for the host's active one; see `setCapabilities`.
    projectId: null,
    // The Agentty new sessions start on, among several; see
    // `setCapabilities`.
    scope: "",
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
      projectId: state.projectId,
      projectKey: state.scope,
      isConnected: state.isConnected,
      isRunning: false,
      session: chosenSession(),
      summary: null,
    }),
    document: document,
    elements: {
      form: elements.form,
      hint: elements.hint,
      images: elements.images,
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
      // Another mode may start on another agent, so its agent, model and
      // speed picks do not carry over.
      if (mode !== state.mode) {
        const kept = { ...state.picks };
        delete kept.agent;
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
        const described = parseHost(await request(withProject({ method: "get_host" })));
        // An answer for another project would start sessions there.
        const isOther = described && state.projectId !== null && described.projectId !== state.projectId;
        host = isOther ? null : described;
        elements.error.textContent = isOther ? "Could not describe the project: " + OTHER_PROJECT : "";
      } catch (error) {
        elements.error.textContent = "Could not describe the project: " + error.message;
      }
    }
    if (load !== state.loads) {
      return;
    }
    state.host = host && { ...host, scope: scopeKey() };
    // The chosen mode stays while the host still offers it.
    if (!host?.modes.includes(state.mode)) {
      state.mode = host?.modes[0] ?? null;
    }
    render();
    onHost(host);
  }

  // `body` naming the project home describes, when it is not the host's
  // active one.
  function withProject(body) {
    return state.projectId === null ? body : { ...body, project_id: state.projectId };
  }

  // What the new session's settings are kept under: the Agentty and the
  // project, so what home looked up for another is not reused.
  function scopeKey() {
    return state.projectId === null ? state.scope : state.scope + "#" + state.projectId;
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

    return settingsChange(start, currentChoice(chosenSession()), state.host.models);
  }

  async function create() {
    const text = elements.message.value;
    const images = composer.images();
    const isEmpty = !text.trim() && images.length === 0;
    if (isEmpty || !state.host || state.isCreating || !state.isConnected) {
      return;
    }
    const change = pickedChange();
    state.isCreating = true;
    elements.error.textContent = "";
    render();
    const body = withProject({ method: "create_session" });
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
    // The new session holds the text and images from here on, sent or not.
    // Text typed and images pasted while the session was being created
    // stay.
    composer.forgetDraft(HOME_DRAFT_ID, text);
    composer.clearText(text);
    composer.forgetImages(HOME_DRAFT_ID, images);
    // The first message waits for the picked settings, so the first turn
    // runs with them; when they fail, the message stays unsent.
    let failure = null;
    if (Object.keys(change).length > 0) {
      try {
        await request({ method: "set_session_settings", session_id: sessionId, ...change });
      } catch (error) {
        failure = { draft: text, error: "Could not apply the settings: " + error.message, images: images };
      }
    }
    if (!failure) {
      try {
        const body = { method: "send_message", session_id: sessionId, message: text };
        if (images.length > 0) {
          body.message = numberedText(text, images);
          body.images = await encodeImages(images);
        }
        await request(body);
      } catch (error) {
        failure = { draft: text, error: error.message, images: images };
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
    // Whether a session is being created; its requests go to the Agentty
    // home described when it started.
    isCreating: () => state.isCreating,
    // Describes the host again with `capabilities`. A page that shows
    // several Agenttys names the one in `scope`, and `projectId` names the
    // project the device picked, `null` for the host's active one, so what
    // home looked up for another is not reused.
    setCapabilities: (capabilities, scope = "", projectId = null) => {
      if (scope !== state.scope || projectId !== state.projectId) {
        state.scope = scope;
        state.projectId = projectId;
        state.host = null;
        state.mode = null;
        state.picks = {};
        render();
      }
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
