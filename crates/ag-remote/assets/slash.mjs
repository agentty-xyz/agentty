// Slash commands of the remote control client's composer. As in the
// terminal, `/` at the start of the message lists the commands that
// change the open session's settings; `/model`, `/reasoning` and `/style`
// then list their values, and `/fast` toggles the speed. With the
// `settings_agent` capability and more than one agent, `/model` first
// lists the agents, then the chosen one's models. Every pick makes
// one `set_session_settings` change, held for a session not yet created,
// so those commands need the same `settings` and `host` capabilities as
// the settings sheet. `/graph`, offered with the `graph_proposal`
// capability, arms the composer to ask for a graph proposal instead of
// sending a message. Model names
// from `get_host` only ever become text nodes, through suggest.mjs.
// The Node tests in crates/ag-remote/tests/web/slash.test.mjs and
// chat.test.mjs pass the fake DOM.

import {
  REASONING_LEVELS,
  RESPONSE_STYLES,
  agentModels,
  currentChoice,
  modelAgents,
  offersAgentChoice,
  offersSettings,
  offersSpeed,
  parseModels,
  settingsChange,
  withCurrent,
} from "./settings.mjs";

// The commands a remote device can run, in the terminal's order and with
// its descriptions. `/apply` needs an operation the host does not offer
// remotely.
export const SLASH_COMMANDS = [
  { description: "Discuss a task with its execution graph beside the chat.", name: "/graph" },
  { description: "Choose an agent and model for this session.", name: "/model" },
  { description: "Override the reasoning level for this session.", name: "/reasoning" },
  { description: "Control how concise or detailed responses are.", name: "/style" },
  { description: "Toggle between normal and fast responses for this session.", name: "/fast" },
];

// Names the menu's lists in the popup it shares with the `@` file lookup.
const OWNER = "slash";
// Statuses in which the terminal hides `/graph`: the session runs a turn
// or a sync, and its composer queues messages.
const BUSY_STATUSES = new Set(["InProgress", "Rebasing"]);

// What each command with a value list chooses, as `currentChoice` names
// it, and the list's title.
const VALUE_COMMANDS = {
  "/agent": { field: "agent", title: "Agent" },
  "/model": { field: "model", title: "Model" },
  "/reasoning": { field: "reasoning", title: "Reasoning" },
  "/style": { field: "style", title: "Style" },
};

// The command-name prefix typed in `text`, lowercased, when the text is a
// slash command being typed: `/` and an optional prefix without
// whitespace, with the caret at the end and nothing selected. Otherwise
// `null`.
export function slashQuery(text, selectionStart = text.length, selectionEnd = selectionStart) {
  if (!text.startsWith("/") || /\s/.test(text)) {
    return null;
  }
  if (selectionStart !== text.length || selectionEnd !== text.length) {
    return null;
  }

  return text.slice(1).toLowerCase();
}

// Whether `/graph` is offered for `session` with the host's
// `capabilities`: the host generates graph proposals for devices, and the
// session is not busy, as the terminal lists it.
export function offersGraph(capabilities, session) {
  return capabilities.includes("graph_proposal") && Boolean(session) && !BUSY_STATUSES.has(session.status);
}

// The commands whose name matches `query` fuzzily, best match first, as
// `prompt_slash_commands` in crates/agentty/src/domain/composer.rs ranks
// them; see `slashScore`. The setting commands are listed only when
// `canSettings`, `/fast` only when `canFast` too: the session's model can
// run fast. `/graph` is listed only when `canGraph`.
export function slashCommands(query, { canFast = false, canGraph = false, canSettings = true } = {}) {
  return SLASH_COMMANDS.filter((command) => isListed(command.name, { canFast, canGraph, canSettings }))
    .map((command) => ({ command: command, score: slashScore(command.name, query) }))
    .filter((entry) => entry.score !== null)
    .sort((first, second) => first.score - second.score)
    .map((entry) => entry.command);
}

// Whether `slashCommands` lists command `name`.
function isListed(name, { canFast, canGraph, canSettings }) {
  if (name === "/graph") {
    return canGraph;
  }

  return canSettings && (name !== "/fast" || canFast);
}

// How well command `name` matches lowercase `query`, lower being better,
// or `null` when it does not: the whole name or no query, then a prefix,
// then the query anywhere in the name, earlier first, then its characters
// in order, earlier and closer together first. `slash_command_fuzzy_score`
// in crates/agentty/src/domain/composer.rs owns the same rule.
export function slashScore(name, query) {
  const command = name.replace(/^\/+/, "");
  const wanted = query.replace(/^\/+/, "");
  if (wanted === "" || command === wanted) {
    return 0;
  }
  if (command.startsWith(wanted)) {
    return 1;
  }
  const at = command.indexOf(wanted);
  if (at !== -1) {
    return 100 + at;
  }
  let next = 0;
  let first = -1;
  let last = -1;
  let gaps = 0;
  for (let index = 0; index < command.length && next < wanted.length; index += 1) {
    if (command[index] !== wanted[next]) {
      continue;
    }
    if (last !== -1) {
      gaps += index - last - 1;
    }
    if (first === -1) {
      first = index;
    }
    last = index;
    next += 1;
  }

  return next < wanted.length ? null : 200 + first * 2 + gaps;
}

// The speed `/fast` switches `session` to.
export function toggledSpeed(session) {
  return currentChoice(session).speed === "fast" ? "normal" : "fast";
}

// The agents `/model` lets `session` choose among `models`, with its own
// one; empty when there is only one to choose.
export function slashAgents(session, models) {
  const agents = withCurrent(modelAgents(models), currentChoice(session).agent);

  return agents.length < 2 ? [] : agents;
}

// The choices `command` lists for `session`, as popup items with the
// current one marked. Models are `parseModels` entries; `/model` lists
// those of `agent`, the session's own by default, `/agent` the agents
// `slashAgents` offers. The session's own model stays listed, by its
// display name, on its own agent when `models` lacks it.
export function slashValues(command, session, models, agent = null) {
  const current = currentChoice(session);
  if (command === "/agent") {
    return slashAgents(session, models).map((value) => ({
      detail: "",
      isCurrent: value === current.agent,
      key: value,
      label: value,
    }));
  }
  if (command === "/model") {
    const chosen = agent ?? current.agent;
    const options = agentModels(models, chosen).map((model) => ({ id: model.id, name: model.name }));
    const own = chosen === current.agent ? current.model : null;
    if (own && !options.some((option) => option.id === own)) {
      options.unshift({ id: own, name: session.model || own });
    }

    return options.map((option) => ({
      detail: option.name === option.id ? "" : option.id,
      isCurrent: option.id === own,
      key: option.id,
      label: option.name,
    }));
  }
  const field = VALUE_COMMANDS[command].field;
  const values = withCurrent(command === "/reasoning" ? REASONING_LEVELS : RESPONSE_STYLES, current[field]);

  return values.map((value) => ({ detail: "", isCurrent: value === current[field], key: value, label: value }));
}

// Wires the slash commands of text field `input` to popup `suggestions`
// from suggest.mjs. `context()` returns the open `session`, the host's
// `capabilities`, `isConnected`, and `isGraphArmed`, while `/graph` has
// armed the composer, which then offers no commands, as in the terminal;
// `request(body)` sends one request and resolves with its result;
// `applySettings(session, change)` applies a `set_session_settings`
// change, by sending it unless given; `onGraph()` arms the composer for a
// graph request, and `/graph` is offered only when it is given;
// `setText(text)` replaces the field's text and its saved draft;
// `showError(message)` shows a failure, or clears it when empty.
//
// Returns `update()`, for the field's input, caret and focus changes,
// which opens, filters or closes the menu and says whether it shows;
// `refresh()` for a newer snapshot of the session; `close()`; and
// `reset()` for another session.
export function createSlashCommands({
  input,
  suggestions,
  request,
  context,
  setText,
  showError,
  applySettings = (session, change) =>
    request({ method: "set_session_settings", session_id: session.id, ...change }),
  onGraph = null,
}) {
  const state = {
    // The agent `/model` lists the models of, once picked among several.
    agent: null,
    // Text the user closed the menu on with `Escape`; it stays closed
    // until the text changes.
    dismissed: null,
    // Counts model loads, so an answer for an older one is dropped.
    loads: 0,
    models: [],
    // The session the models were loaded for, with whether they span
    // every agent, and the load.
    modelsFor: null,
    modelsLoad: null,
    // What the menu shows: `commands`, a command with a value list, or
    // `null` when it shows nothing.
    stage: null,
    // The field's text the stage was shown for.
    text: null,
  };

  // What the menu may offer now: the open `session`, whether this device
  // may change its settings (`canSettings`) and their agent
  // (`canChooseAgent`), and whether `/graph` may arm the composer
  // (`canGraph`). `null` when it may offer nothing.
  function offer() {
    const { capabilities = [], isConnected = false, isGraphArmed = false, session = null } =
      context() ?? {};
    if (!session || !isConnected || isGraphArmed) {
      return null;
    }
    const canSettings = offersSettings(capabilities);
    const canChooseAgent = canSettings && offersAgentChoice(capabilities);
    const canGraph = onGraph !== null && offersGraph(capabilities, session);

    return canSettings || canGraph ? { canChooseAgent, canGraph, canSettings, session } : null;
  }

  function close() {
    state.agent = null;
    if (state.stage !== null) {
      state.stage = null;
      state.text = null;
      suggestions.close(OWNER);
    }
  }

  function loadModels(session, canChooseAgent) {
    const key = session.id + (canChooseAgent ? "\u0000*" : "");
    if (state.modelsFor === key) {
      return state.modelsLoad;
    }
    const load = ++state.loads;
    state.modelsFor = key;
    state.modelsLoad = (async () => {
      let models = [];
      try {
        const agent = canChooseAgent ? null : session.agent;
        models = parseModels(await request({ method: "get_host" }), agent);
      } catch (error) {
        if (load !== state.loads) {
          return;
        }
        // The next menu tries again.
        state.modelsFor = null;
        showError("Could not list the models: " + error.message);
      }
      if (load === state.loads) {
        state.models = models;
        refresh();
      }
    })();

    return state.modelsLoad;
  }

  // Lists what `stage` offers under `current`, an `offer()`. A stage with
  // nothing to list yet stays current, closed, so the models from the host
  // can still fill it.
  // `/model` lists the agents first while there are several to choose and
  // none is chosen yet.
  function show(stage, current) {
    const { canChooseAgent, canGraph, canSettings, session } = current;
    if (stage === "commands") {
      state.agent = null;
    }
    state.stage = stage;
    state.text = input.value;
    const listsAgents =
      stage === "/model" && state.agent === null && canChooseAgent && slashAgents(session, state.models).length > 0;
    const listed = listsAgents ? "/agent" : stage;
    const options = { onDismiss: dismiss, onPick: (item) => pick(listed, item.key), owner: OWNER };
    if (stage === "commands") {
      const query = slashQuery(input.value) ?? "";
      const own = currentChoice(session);
      const canFast = offersSpeed(state.models, own.model, own.agent);
      suggestions.show({
        items: slashCommands(query, { canFast, canGraph, canSettings }).map((command) => ({
          detail: command.description,
          key: command.name,
          label: command.name,
        })),
        title: "Commands",
        ...options,
      });
    } else {
      suggestions.show({
        items: slashValues(listed, session, state.models, state.agent),
        title: VALUE_COMMANDS[listed].title,
        ...options,
      });
    }
  }

  function dismiss() {
    state.dismissed = input.value;
    state.stage = null;
    state.text = null;
  }

  function update() {
    const current = offer();
    const text = input.value;
    if (!current || (state.dismissed !== null && text === state.dismissed)) {
      close();

      return false;
    }
    state.dismissed = null;
    if (slashQuery(text, input.selectionStart, input.selectionEnd) === null) {
      close();

      return false;
    }
    // The same text keeps what the menu shows, such as a value list.
    if (state.stage === null || state.text !== text) {
      if (current.canSettings) {
        void loadModels(current.session, current.canChooseAgent);
      }
      show("commands", current);
    }

    return suggestions.isOpen(OWNER);
  }

  // Shows what is on screen again for a newer snapshot, or closes the menu
  // once the session no longer allows it.
  function refresh() {
    const current = offer();
    // Text changed outside the menu, as when a message is sent, ends it,
    // and a value list needs the settings.
    const isOver =
      !current ||
      (state.stage !== null && input.value !== state.text) ||
      (state.stage !== "commands" && !current.canSettings);
    if (isOver) {
      close();
    } else if (state.stage !== null) {
      show(state.stage, current);
    }
  }

  function pick(stage, key) {
    const current = offer();
    if (stage === "commands" && key === "/graph" && current?.canGraph) {
      // As in the terminal, the command leaves an empty composer for the
      // task.
      close();
      setText("");
      onGraph();

      return;
    }
    if (!current?.canSettings) {
      close();

      return;
    }
    const session = current.session;
    if (stage === "/agent") {
      // As in the terminal, the chosen agent's models follow.
      state.agent = key;
      show("/model", current);

      return;
    }
    if (stage === "commands" && key !== "/fast") {
      // The models from `get_host` replace the session's own one once
      // they arrive.
      setText(key);
      show(key, current);

      return;
    }
    const choice = currentChoice(session);
    if (stage === "commands") {
      choice.speed = toggledSpeed(session);
    } else {
      choice[VALUE_COMMANDS[stage].field] = key;
    }
    if (stage === "/model" && state.agent) {
      choice.agent = state.agent;
    }
    void apply(session, settingsChange(session, choice, state.models));
  }

  async function apply(session, change) {
    close();
    setText("");
    if (Object.keys(change).length === 0) {
      return;
    }
    showError("");
    try {
      await applySettings(session, change);
    } catch (error) {
      if (context()?.session?.id === session.id) {
        showError(error.message);
      }
    }
  }

  function reset() {
    close();
    state.dismissed = null;
    state.loads += 1;
    state.models = [];
    state.modelsFor = null;
    state.modelsLoad = null;
  }

  return { close, refresh, reset, update };
}
