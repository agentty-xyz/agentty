// Settings sheet of the remote control client: with the `settings` and
// `host` capabilities, the settings line under the message box opens a
// sheet that changes the open session's model, reasoning level, answer
// style and speed. The models come from `get_host`, and their names only
// ever become text nodes. Only the settings the user changed are sent.
// Functions take `document` as a parameter, so the Node tests in
// crates/ag-remote/tests/web/settings.test.mjs pass the fake DOM.

// Choices the host accepts, as `ag_contracts` names them, in the
// terminal's order.
export const REASONING_LEVELS = ["low", "medium", "high", "xhigh", "max"];
export const RESPONSE_STYLES = ["concise", "balanced", "detailed"];
export const SPEED_MODES = ["normal", "fast"];

// Whether the host lets this device change session settings. The model
// list comes from `get_host`, so both capabilities are needed.
export function offersSettings(capabilities) {
  return capabilities.includes("settings") && capabilities.includes("host");
}

// The models in `get_host`'s answer that a session on `agent` can switch
// to, as `{ id, name, supportsFast }`. A session switches only among its
// own agent's models; without an agent, every model is listed.
export function parseModels(result, agent = null) {
  if (result?.kind !== "host" || !Array.isArray(result.models)) {
    return [];
  }

  return result.models
    .filter((model) => text(model?.model) && (!agent || model.agent === agent))
    .map((model) => ({
      id: model.model,
      name: text(model.name) || model.model,
      supportsFast: model.supports_fast === true,
    }));
}

// What the sheet starts with for `session`: its current settings.
export function currentChoice(session) {
  return {
    model: text(session?.model_id),
    reasoning: text(session?.reasoning),
    speed: text(session?.speed_mode),
    style: text(session?.response_style),
  };
}

// Whether speed is a choice for model `modelId`: only a model `models`
// lists as able to run fast offers it.
export function offersSpeed(models, modelId) {
  return models.some((model) => model.id === modelId && model.supportsFast);
}

// The fields of `set_session_settings` that `choice` changes from
// `session`'s settings, named as the request names them. Speed counts only
// while the chosen model offers it. An empty object means nothing changed.
export function settingsChange(session, choice, models) {
  const current = currentChoice(session);
  const change = {};
  if (choice.model && choice.model !== current.model) {
    change.model = choice.model;
  }
  if (choice.reasoning && choice.reasoning !== current.reasoning) {
    change.reasoning = choice.reasoning;
  }
  if (choice.style && choice.style !== current.style) {
    change.response_style = choice.style;
  }
  if (choice.speed && choice.speed !== current.speed && offersSpeed(models, choice.model)) {
    change.speed_mode = choice.speed;
  }

  return change;
}

// `values` with `current` added at the end when the host reports a value
// this page does not know.
export function withCurrent(values, current) {
  return current && !values.includes(current) ? [...values, current] : values;
}

// Wires the settings sheet. `elements` holds `dialog`, `form`, `model`,
// `reasoning`, `style`, `speedRow`, `speed`, `note`, `error` and `apply`,
// the nodes with the ids named in index.html. `request(body)` sends one
// request and resolves with its result.
//
// Returns `open(session, { isRunning })`, `update(session, { isRunning })`
// for a newer snapshot of the open session, and `close()`.
export function createSettings({ document, elements, request }) {
  const state = {
    choice: currentChoice(null),
    isApplying: false,
    // Whether the model list is on its way.
    isLoading: false,
    // Counts model loads, so an answer for an older one is dropped.
    loads: 0,
    models: [],
    session: null,
  };

  function render() {
    const session = state.session;
    if (!session) {
      return;
    }
    renderModels();
    renderChoices(elements.reasoning, withCurrent(REASONING_LEVELS, currentChoice(session).reasoning), "reasoning");
    renderChoices(elements.style, withCurrent(RESPONSE_STYLES, currentChoice(session).style), "style");
    elements.speedRow.hidden = !offersSpeed(state.models, state.choice.model);
    renderChoices(elements.speed, SPEED_MODES, "speed");
    renderApply();
  }

  // The session's own model stays listed, by its display name, while the
  // host's list is loading or when it lacks that model.
  function renderModels() {
    const options = state.models.map((model) => ({ id: model.id, name: model.name }));
    const current = currentChoice(state.session).model;
    if (current && !options.some((option) => option.id === current)) {
      options.unshift({ id: current, name: text(state.session.model) || current });
    }
    elements.model.replaceChildren(
      ...options.map((option) => {
        const node = document.createElement("option");
        node.value = option.id;
        node.textContent = option.name;

        return node;
      }),
    );
    elements.model.value = state.choice.model ?? "";
    elements.model.disabled = options.length < 2 || state.isApplying;
    elements.model.setAttribute("aria-busy", String(state.isLoading));
  }

  function renderChoices(group, values, key) {
    group.replaceChildren(
      ...values.map((value) => {
        const button = document.createElement("button");
        button.type = "button";
        button.textContent = value;
        button.dataset.value = value;
        button.disabled = state.isApplying;
        button.setAttribute("aria-pressed", String(value === state.choice[key]));
        button.addEventListener("click", () => {
          state.choice[key] = value;
          render();
        });

        return button;
      }),
    );
  }

  function renderApply() {
    const change = settingsChange(state.session, state.choice, state.models);
    elements.apply.disabled = state.isApplying || Object.keys(change).length === 0;
    elements.apply.textContent = state.isApplying ? "Saving…" : "Apply";
  }

  function renderNote(isRunning) {
    elements.note.hidden = !isRunning;
  }

  async function loadModels(session) {
    const load = ++state.loads;
    let models = [];
    try {
      models = parseModels(await request({ method: "get_host" }), text(session.agent));
    } catch (error) {
      if (load === state.loads) {
        elements.error.textContent = "Could not list the models: " + error.message;
      }
    }
    if (load !== state.loads) {
      return;
    }
    state.models = models;
    state.isLoading = false;
    render();
  }

  async function apply() {
    const session = state.session;
    const change = session ? settingsChange(session, state.choice, state.models) : {};
    if (!session || state.isApplying || Object.keys(change).length === 0) {
      return;
    }
    state.isApplying = true;
    elements.error.textContent = "";
    render();
    try {
      await request({ method: "set_session_settings", session_id: session.id, ...change });
      if (state.session === session) {
        close();
      }
    } catch (error) {
      if (state.session === session) {
        elements.error.textContent = error.message;
      }
    } finally {
      state.isApplying = false;
      render();
    }
  }

  function open(session, { isRunning = false } = {}) {
    state.session = session;
    state.choice = currentChoice(session);
    state.models = [];
    state.isApplying = false;
    state.isLoading = true;
    elements.error.textContent = "";
    renderNote(isRunning);
    render();
    elements.dialog.showModal();
    void loadModels(session);
  }

  function close() {
    state.session = null;
    state.loads += 1;
    if (elements.dialog.open) {
      elements.dialog.close("");
    }
  }

  elements.model.addEventListener("change", () => {
    state.choice.model = elements.model.value;
    render();
  });
  elements.form.addEventListener("submit", (event) => {
    if (event.submitter?.value !== "apply") {
      return;
    }
    event.preventDefault();
    void apply();
  });
  // Closing the sheet any other way, such as with `Esc`, forgets it.
  elements.dialog.addEventListener("close", () => {
    state.session = null;
    state.loads += 1;
  });

  return {
    close,
    open,
    update: (session, { isRunning = false } = {}) => {
      if (state.session?.id === session?.id) {
        state.session = session;
        renderNote(isRunning);
        render();
      }
    },
  };
}

// `value` when it is a non-empty string, otherwise `null`.
function text(value) {
  return typeof value === "string" && value !== "" ? value : null;
}
