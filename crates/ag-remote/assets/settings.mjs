// Settings sheet of the remote control client: with the `settings` and
// `host` capabilities, the settings line under the message box opens a
// sheet that changes the model, reasoning level, answer style and speed of
// the open session, or of the session home is about to start. With the
// `settings_agent` capability too, it also changes the agent that runs the
// model, as the terminal's `/model` does. The agents and models come from
// `get_host`, and their names only ever become text nodes. Only the
// settings the user changed are sent.
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

// Whether the host lets a session switch to another agent's models: it
// then takes the `agent` of `set_session_settings`.
export function offersAgentChoice(capabilities) {
  return capabilities.includes("settings_agent");
}

// The models in `get_host`'s answer, as `{ agent, id, name, supportsFast }`.
// With `agent`, only that agent's models are listed, for a host that
// switches a session only among its own agent's models.
export function parseModels(result, agent = null) {
  if (result?.kind !== "host" || !Array.isArray(result.models)) {
    return [];
  }

  return result.models
    .filter((model) => text(model?.model) && (!agent || model.agent === agent))
    .map((model) => ({
      agent: text(model.agent),
      id: model.model,
      name: text(model.name) || model.model,
      supportsFast: model.supports_fast === true,
    }));
}

// The agents that run `models`, in the order `get_host` lists them.
export function modelAgents(models) {
  return [...new Set(models.map((model) => model.agent).filter(Boolean))];
}

// The entries of `models` that `agent` runs; all of them without an
// agent.
export function agentModels(models, agent) {
  return agent ? models.filter((model) => model.agent === agent) : models;
}

// What the sheet starts with for `session`: its current settings.
export function currentChoice(session) {
  return {
    agent: text(session?.agent),
    model: text(session?.model_id),
    reasoning: text(session?.reasoning),
    speed: text(session?.speed_mode),
    style: text(session?.response_style),
  };
}

// Whether speed is a choice for model `modelId` on `agent`: only a model
// `models` lists as able to run fast offers it. Agents share model ids,
// so the agent counts when given.
export function offersSpeed(models, modelId, agent = null) {
  return agentModels(models, agent).some((model) => model.id === modelId && model.supportsFast);
}

// The fields of `set_session_settings` that `choice` changes from
// `session`'s settings, named as the request names them. Another agent is
// sent together with its model, even one with the same id, since agents
// share model ids. Speed counts only while the chosen model offers it. An
// empty object means nothing changed.
export function settingsChange(session, choice, models) {
  const current = currentChoice(session);
  const change = {};
  const agent = choice.agent ?? current.agent;
  if (choice.model && agent && agent !== current.agent) {
    change.agent = agent;
    change.model = choice.model;
  } else if (choice.model && choice.model !== current.model) {
    change.model = choice.model;
  }
  if (choice.reasoning && choice.reasoning !== current.reasoning) {
    change.reasoning = choice.reasoning;
  }
  if (choice.style && choice.style !== current.style) {
    change.response_style = choice.style;
  }
  if (choice.speed && choice.speed !== current.speed && offersSpeed(models, choice.model, agent)) {
    change.speed_mode = choice.speed;
  }

  return change;
}

// `choice` for a newer snapshot of the session: each setting the user left
// at its `before` value follows the snapshot to `after`, so applying the
// sheet never undoes a change made meanwhile, such as a queued agent
// switch. The agent and the model move together, since a model belongs to
// its agent.
export function refreshedChoice(choice, before, after) {
  const refreshed = { ...choice };
  if (choice.agent === before.agent && choice.model === before.model) {
    refreshed.agent = after.agent;
    refreshed.model = after.model;
  }
  for (const key of ["reasoning", "speed", "style"]) {
    if (choice[key] === before[key]) {
      refreshed[key] = after[key];
    }
  }

  return refreshed;
}

// `values` with `current` added at the end when the host reports a value
// this page does not know.
export function withCurrent(values, current) {
  return current && !values.includes(current) ? [...values, current] : values;
}

// Wires the settings sheet. `elements` holds `dialog`, `form`, `agentRow`,
// `agent`, `model`, `reasoning`, `style`, `speedRow`, `speed`, `note`,
// `error` and `apply`, the nodes with the ids named in index.html.
// `request(body)` sends one request and resolves with its result.
//
// Returns `open(session, { isRunning, apply, canChooseAgent })`,
// `update(session, { isRunning })` for a newer snapshot of the open
// session, and `close()`. `apply(change)` applies the
// `set_session_settings` change and resolves once it has; without it, the
// sheet sends the request. With `canChooseAgent`, the sheet lists every
// agent's models and offers the agent as a choice too.
export function createSettings({ document, elements, request }) {
  const state = {
    apply: null,
    // Whether the host takes another agent for the session.
    canChooseAgent: false,
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
    renderAgents();
    renderModels();
    renderChoices(elements.reasoning, withCurrent(REASONING_LEVELS, currentChoice(session).reasoning), "reasoning");
    renderChoices(elements.style, withCurrent(RESPONSE_STYLES, currentChoice(session).style), "style");
    elements.speedRow.hidden = !offersSpeed(state.models, state.choice.model, chosenAgent());
    renderChoices(elements.speed, SPEED_MODES, "speed");
    renderApply();
  }

  // The agent the chosen model runs on: the picked one, or the session's.
  function chosenAgent() {
    return state.choice.agent ?? currentChoice(state.session).agent;
  }

  // The agents the host runs models on, with the session's own one, while
  // the sheet may change the agent and there is more than one.
  function renderAgents() {
    const agents = state.canChooseAgent
      ? withCurrent(modelAgents(state.models), currentChoice(state.session).agent)
      : [];
    elements.agentRow.hidden = agents.length < 2;
    renderChoices(elements.agent, agents.length < 2 ? [] : agents, "agent", chooseAgent);
  }

  // Picks `agent` and a model it runs: the chosen one when it runs that id
  // too, the session's own on the session's agent, otherwise its first.
  // Every other agent listed runs at least one model.
  function chooseAgent(agent) {
    const own = currentChoice(state.session);
    const models = agentModels(state.models, agent);
    state.choice.agent = agent;
    if (!models.some((model) => model.id === state.choice.model)) {
      state.choice.model = agent === own.agent ? own.model : models[0].id;
    }
  }

  // The chosen agent's models. The session's own model stays listed, by
  // its display name, on the session's agent while the host's list is
  // loading or when it lacks that model.
  function renderModels() {
    const agent = chosenAgent();
    const options = agentModels(state.models, agent).map((model) => ({ id: model.id, name: model.name }));
    const own = currentChoice(state.session);
    const current = agent === own.agent ? own.model : null;
    if (current && !options.some((option) => option.id === current)) {
      options.unshift({ id: current, name: text(state.session.model) || current });
    }
    // The options are rebuilt only when they change, so an open picker
    // and the focus stay where they are.
    const listed = Array.from(elements.model.children, (node) => [node.value, node.textContent]);
    const wanted = options.map((option) => [option.id, option.name]);
    if (JSON.stringify(listed) !== JSON.stringify(wanted)) {
      elements.model.replaceChildren(
        ...options.map((option) => {
          const node = document.createElement("option");
          node.value = option.id;
          node.textContent = option.name;

          return node;
        }),
      );
    }
    if (elements.model.value !== (state.choice.model ?? "")) {
      elements.model.value = state.choice.model ?? "";
    }
    elements.model.disabled = options.length < 2 || state.isApplying;
    elements.model.setAttribute("aria-busy", String(state.isLoading));
  }

  // Shows `values` as toggle buttons for `key`, which a click sets, or
  // `pick(value)` when given. The buttons are rebuilt only when the values
  // change; otherwise they are updated in place, so the focused one keeps
  // its focus.
  function renderChoices(group, values, key, pick = null) {
    const shown = Array.from(group.children, (button) => button.dataset.value);
    if (shown.join("\u0000") !== values.join("\u0000")) {
      group.replaceChildren(
        ...values.map((value) => {
          const button = document.createElement("button");
          button.type = "button";
          button.textContent = value;
          button.dataset.value = value;
          button.addEventListener("click", () => {
            if (pick) {
              pick(value);
            } else {
              state.choice[key] = value;
            }
            render();
          });

          return button;
        }),
      );
    }
    for (const button of group.children) {
      button.disabled = state.isApplying;
      button.setAttribute("aria-pressed", String(button.dataset.value === state.choice[key]));
    }
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
      const agent = state.canChooseAgent ? null : text(session.agent);
      models = parseModels(await request({ method: "get_host" }), agent);
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
      await state.apply(change);
      if (state.session?.id === session.id) {
        close();
      }
    } catch (error) {
      if (state.session?.id === session.id) {
        elements.error.textContent = error.message;
      }
    } finally {
      state.isApplying = false;
      render();
    }
  }

  function open(session, { isRunning = false, apply = null, canChooseAgent = false } = {}) {
    state.session = session;
    state.canChooseAgent = canChooseAgent;
    state.apply =
      apply ?? ((change) => request({ method: "set_session_settings", session_id: session.id, ...change }));
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
        state.choice = refreshedChoice(state.choice, currentChoice(state.session), currentChoice(session));
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
