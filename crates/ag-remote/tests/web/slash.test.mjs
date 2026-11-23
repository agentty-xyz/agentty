import assert from "node:assert/strict";
import test from "node:test";

import { FakeEvent, createDocument } from "./fake_dom.mjs";
import { parseModels } from "../../assets/settings.mjs";
import {
  SLASH_COMMANDS,
  createSlashCommands,
  offersGraph,
  slashCommands,
  slashQuery,
  slashScore,
  slashValues,
  toggledSpeed,
} from "../../assets/slash.mjs";
import { createSuggestions } from "../../assets/suggest.mjs";

const HOST = {
  kind: "host",
  models: [
    { agent: "claude", model: "claude-opus-5-5", name: "Opus 5.5", supports_fast: true },
    { agent: "claude", model: "claude-sonnet-5", name: "Sonnet 5", supports_fast: false },
    { agent: "codex", model: "gpt-6.1", name: "GPT 6.1", supports_fast: true },
  ],
};

function session(overrides = {}) {
  return {
    agent: "claude",
    id: "s1",
    model: "Opus 5.5",
    model_id: "claude-opus-5-5",
    reasoning: "high",
    response_style: "balanced",
    speed_mode: "normal",
    status: "Review",
    ...overrides,
  };
}

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// Mounts the slash commands on a text field. `respond(body)` answers each
// request; the requests sent and the errors shown are recorded. `context`
// is what the provider reads, and tests change it in place. `onGraph`,
// when given, arms graph mode and offers `/graph`.
function mountSlash({
  onGraph = null,
  respond = (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }),
} = {}) {
  const document = createDocument(
    "<!DOCTYPE html><html><body><form><textarea id=\"field\"></textarea></form></body></html>",
  );
  const input = document.getElementById("field");
  const suggestions = createSuggestions({ document: document, id: "slash", input: input });
  const context = { capabilities: ["host", "settings"], isConnected: true, session: session() };
  const requests = [];
  const errors = [];
  const texts = [];
  const slash = createSlashCommands({
    context: () => context,
    input: input,
    onGraph: onGraph,
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
    setText: (text) => {
      texts.push(text);
      input.value = text;
    },
    showError: (message) => errors.push(message),
    suggestions: suggestions,
  });
  input.addEventListener("keydown", (event) => suggestions.handleKey(event));
  const type = (text) => {
    input.value = text;
    return slash.update();
  };
  const press = (key) => input.dispatchEvent(new FakeEvent("keydown", { key: key }));
  const shown = () =>
    Array.from(document.getElementById("slash").children, (option) => option.dataset.key);

  return { context, errors, input, press, requests, shown, slash, suggestions, texts, type };
}

test("slashQuery reads a command being typed at the start, with the caret at the end", () => {
  assert.equal(slashQuery("/"), "");
  assert.equal(slashQuery("/Mo"), "mo");
  assert.equal(slashQuery("/model x"), null);
  assert.equal(slashQuery("/mo\n"), null);
  assert.equal(slashQuery("hi /mo"), null);
  assert.equal(slashQuery(""), null);
  assert.equal(slashQuery("/mo", 1), null);
  assert.equal(slashQuery("/mo", 0, 3), null);
  assert.equal(slashQuery("/mo", 3, 3), "mo");
});

test("slashCommands matches fuzzily as the terminal does, and lists /fast only when it can run", () => {
  const names = (list) => list.map((command) => command.name);

  assert.deepEqual(names(slashCommands("")), ["/model", "/reasoning", "/style"]);
  assert.deepEqual(names(slashCommands("", { canFast: true })), ["/model", "/reasoning", "/style", "/fast"]);
  assert.deepEqual(names(slashCommands("re")), ["/reasoning"]);
  assert.deepEqual(names(slashCommands("f", { canFast: true })), ["/fast"]);
  assert.deepEqual(names(slashCommands("f")), []);
  // A prefix ranks above the same letters inside another name.
  assert.deepEqual(names(slashCommands("st", { canFast: true })), ["/style", "/fast"]);
  // Inside a name, an earlier match ranks first; equal scores keep the
  // terminal's order.
  assert.deepEqual(names(slashCommands("e", { canFast: true })), ["/reasoning", "/model", "/style"]);
  // Letters in order, with gaps, still match.
  assert.deepEqual(names(slashCommands("rsn")), ["/reasoning"]);
  assert.deepEqual(names(slashCommands("ml")), ["/model"]);
  assert.deepEqual(names(slashCommands("lm")), []);
  assert.deepEqual(names(slashCommands("apply", { canFast: true })), []);
  assert.deepEqual(names(slashCommands("graph", { canFast: true })), []);
  assert.deepEqual(names(slashCommands("remote", { canFast: true })), []);
  assert.equal(SLASH_COMMANDS[1].description, "Choose an agent and model for this session.");
});

test("slashCommands lists /graph first, as the terminal does, only when it may arm", () => {
  const names = (list) => list.map((command) => command.name);

  assert.deepEqual(names(slashCommands("", { canFast: true, canGraph: true })), [
    "/graph",
    "/model",
    "/reasoning",
    "/style",
    "/fast",
  ]);
  assert.deepEqual(names(slashCommands("g", { canGraph: true })), ["/graph", "/reasoning"]);
  assert.deepEqual(names(slashCommands("gr", { canGraph: true })), ["/graph"]);
  assert.deepEqual(names(slashCommands("", { canGraph: true, canSettings: false })), ["/graph"]);
  assert.deepEqual(names(slashCommands("", { canFast: true, canSettings: false })), []);
  assert.deepEqual(SLASH_COMMANDS[0], {
    description: "Discuss a task with its execution graph beside the chat.",
    name: "/graph",
  });
});

test("offersGraph needs the capability and a session that is not busy", () => {
  assert.equal(offersGraph(["graph_proposal"], session()), true);
  assert.equal(offersGraph(["graph_proposal"], session({ status: "Draft" })), true);
  assert.equal(offersGraph(["graph_proposal"], session({ status: "InProgress" })), false);
  assert.equal(offersGraph(["graph_proposal"], session({ status: "Rebasing" })), false);
  assert.equal(offersGraph(["graph_proposal"], null), false);
  assert.equal(offersGraph(["graphs", "graph_actions"], session()), false);
});

test("slashScore ranks like the terminal's slash_command_fuzzy_score", () => {
  assert.equal(slashScore("/model", ""), 0);
  assert.equal(slashScore("/model", "model"), 0);
  assert.equal(slashScore("/model", "/model"), 0);
  assert.equal(slashScore("/model", "mo"), 1);
  assert.equal(slashScore("/fast", "st"), 102);
  assert.equal(slashScore("/model", "ml"), 203);
  assert.equal(slashScore("/reasoning", "rsn"), 203);
  assert.equal(slashScore("/style", "yl"), 102);
  assert.equal(slashScore("/style", "se"), 203);
  assert.equal(slashScore("/style", "x"), null);
  assert.equal(slashScore("/style", "styles"), null);
});

test("toggledSpeed switches between normal and fast", () => {
  assert.equal(toggledSpeed(session()), "fast");
  assert.equal(toggledSpeed(session({ speed_mode: "fast" })), "normal");
  assert.equal(toggledSpeed(session({ speed_mode: null })), "fast");
});

test("slashValues lists the choices with the current one marked", () => {
  const models = parseModels(HOST, "claude");

  assert.deepEqual(slashValues("/model", session(), models), [
    { detail: "claude-opus-5-5", isCurrent: true, key: "claude-opus-5-5", label: "Opus 5.5" },
    { detail: "claude-sonnet-5", isCurrent: false, key: "claude-sonnet-5", label: "Sonnet 5" },
  ]);
  // The session's own model stays listed while the host's list lacks it.
  assert.deepEqual(slashValues("/model", session({ model: null, model_id: "custom" }), []), [
    { detail: "", isCurrent: true, key: "custom", label: "custom" },
  ]);
  assert.deepEqual(slashValues("/model", session({ model_id: null }), []), []);
  assert.deepEqual(
    slashValues("/reasoning", session(), models).map((item) => [item.key, item.isCurrent]),
    [
      ["low", false],
      ["medium", false],
      ["high", true],
      ["xhigh", false],
      ["max", false],
    ],
  );
  // A value this page does not know is listed last.
  assert.deepEqual(
    slashValues("/style", session({ response_style: "terse" }), models).map((item) => item.key),
    ["concise", "balanced", "detailed", "terse"],
  );
});

test("typing / opens the commands, filters them, and closes on other text", async () => {
  const { requests, shown, suggestions, type } = mountSlash();

  assert.equal(type("/"), true);
  assert.deepEqual(shown(), ["/model", "/reasoning", "/style"]);
  assert.deepEqual(requests, [{ method: "get_host" }]);
  await settle();
  // The session's model runs fast, so /fast is offered once the host says so.
  assert.deepEqual(shown(), ["/model", "/reasoning", "/style", "/fast"]);
  type("/sty");
  assert.deepEqual(shown(), ["/style"]);
  type("/st");
  assert.deepEqual(shown(), ["/style", "/fast"]);
  assert.equal(type("/x"), false);
  assert.equal(suggestions.isOpen(), false);
  type("/s");
  assert.equal(type("/style now"), false);
  assert.equal(type("hello"), false);
  // The models load once per session.
  type("/");
  assert.equal(requests.length, 1);
});

test("the caret away from the end closes the menu", () => {
  const { input, slash, suggestions, type } = mountSlash();
  type("/mo");
  input.setSelectionRange(1);

  assert.equal(slash.update(), false);
  assert.equal(suggestions.isOpen(), false);
});

test("/fast stays hidden for a model that cannot run fast", async () => {
  const { context, shown, slash, type } = mountSlash();
  context.session = session({ model: "Sonnet 5", model_id: "claude-sonnet-5" });
  type("/");
  await settle();

  assert.deepEqual(shown(), ["/model", "/reasoning", "/style"]);
  // Switching to a fast model offers it on the next snapshot.
  context.session = session();
  slash.refresh();
  assert.deepEqual(shown(), ["/model", "/reasoning", "/style", "/fast"]);
});

test("the menu needs the settings and host capabilities and a connection", () => {
  for (const capabilities of [[], ["settings"], ["host"]]) {
    const { context, requests, suggestions, type } = mountSlash();
    context.capabilities = capabilities;
    assert.equal(type("/"), false);
    assert.equal(suggestions.isOpen(), false);
    assert.deepEqual(requests, []);
  }
  const { context, suggestions, slash, type } = mountSlash();
  context.isConnected = false;
  assert.equal(type("/"), false);
  context.isConnected = true;
  context.session = null;
  assert.equal(type("/"), false);
  // Losing the capability closes an open menu.
  context.session = session();
  type("/");
  context.capabilities = ["host"];
  slash.refresh();
  assert.equal(suggestions.isOpen(), false);
});

test("/model lists the session agent's models, and a pick sends the model and clears the text", async () => {
  const { errors, input, press, requests, shown, suggestions, texts, type } = mountSlash();
  type("/mo");
  await settle();
  press("Enter");

  assert.equal(input.value, "/model");
  assert.deepEqual(shown(), ["claude-opus-5-5", "claude-sonnet-5"]);
  assert.equal(suggestions.element.querySelector(".suggest-title").textContent, "Model");
  assert.equal(suggestions.element.querySelector(".is-current").dataset.key, "claude-opus-5-5");
  press("ArrowDown");
  press("Enter");
  await settle();
  assert.equal(suggestions.isOpen(), false);
  assert.deepEqual(texts, ["/model", ""]);
  assert.deepEqual(requests.slice(1), [
    { method: "set_session_settings", model: "claude-sonnet-5", session_id: "s1" },
  ]);
  assert.deepEqual(errors, [""]);
});

test("picking the current value sends nothing and still clears the text", async () => {
  const { errors, input, press, requests, type } = mountSlash();
  type("/mo");
  await settle();
  press("Enter");
  press("Enter");
  await settle();

  assert.equal(input.value, "");
  assert.deepEqual(requests, [{ method: "get_host" }]);
  assert.deepEqual(errors, []);
});

test("/model shows the session's own model until the host's list arrives", async () => {
  let answer;
  const { press, shown, type } = mountSlash({
    respond: (body) =>
      body.method === "get_host" ? new Promise((resolve) => (answer = resolve)) : { kind: "accepted" },
  });
  type("/mo");
  press("Tab");

  assert.deepEqual(shown(), ["claude-opus-5-5"]);
  answer(HOST);
  await settle();
  assert.deepEqual(shown(), ["claude-opus-5-5", "claude-sonnet-5"]);
});

test("/reasoning and /style list their values and send the pick", async () => {
  const { press, requests, shown, type } = mountSlash();
  type("/r");
  press("Enter");
  assert.deepEqual(shown(), ["low", "medium", "high", "xhigh", "max"]);
  press("ArrowUp");
  press("Enter");
  type("/st");
  press("Enter");
  assert.deepEqual(shown(), ["concise", "balanced", "detailed"]);
  press("Enter");
  await settle();

  assert.deepEqual(requests.slice(1), [
    { method: "set_session_settings", reasoning: "max", session_id: "s1" },
    { method: "set_session_settings", response_style: "concise", session_id: "s1" },
  ]);
});

test("/fast toggles the speed at once", async () => {
  const { context, input, press, requests, suggestions, type } = mountSlash();
  type("/f");
  await settle();
  press("Enter");
  await settle();
  assert.equal(suggestions.isOpen(), false);
  assert.equal(input.value, "");
  context.session = session({ speed_mode: "fast" });
  type("/fa");
  press("Enter");
  await settle();

  assert.deepEqual(requests.slice(1), [
    { method: "set_session_settings", session_id: "s1", speed_mode: "fast" },
    { method: "set_session_settings", session_id: "s1", speed_mode: "normal" },
  ]);
});

test("a refused change shows the host's error while its session is open", async () => {
  const { context, errors, press, type } = mountSlash({
    respond: (body) => {
      if (body.method === "get_host") {
        return HOST;
      }
      throw new Error("Session is busy");
    },
  });
  type("/r");
  press("Enter");
  press("Enter");
  await settle();
  assert.deepEqual(errors, ["", "Session is busy"]);

  // An answer for a session no longer open is dropped.
  type("/r");
  press("Enter");
  press("Enter");
  context.session = session({ id: "s2" });
  await settle();
  assert.deepEqual(errors, ["", "Session is busy", ""]);
});

test("a failed model list says so, and the next menu tries again", async () => {
  let fails = true;
  const { errors, requests, shown, type, slash } = mountSlash({
    respond: (body) => {
      if (fails) {
        throw new Error("offline");
      }

      return body.method === "get_host" ? HOST : { kind: "accepted" };
    },
  });
  type("/");
  await settle();
  assert.deepEqual(errors, ["Could not list the models: offline"]);
  assert.deepEqual(shown(), ["/model", "/reasoning", "/style"]);

  fails = false;
  slash.close();
  type("/");
  await settle();
  assert.equal(requests.length, 2);
  assert.deepEqual(shown(), ["/model", "/reasoning", "/style", "/fast"]);
});

test("Escape keeps the menu closed until the text changes", () => {
  const { press, slash, suggestions, type } = mountSlash();
  type("/m");
  press("Escape");

  assert.equal(suggestions.isOpen(), false);
  assert.equal(slash.update(), false);
  assert.equal(type("/mo"), true);
  // Escape in a value list closes the menu too.
  press("Enter");
  press("Escape");
  assert.equal(slash.update(), false);
  assert.equal(type("/mode"), true);
});

test("the same text keeps the value list, and a new text returns to the commands", () => {
  const { press, shown, slash, type } = mountSlash();
  type("/r");
  press("Enter");

  assert.equal(slash.update(), true);
  assert.deepEqual(shown(), ["low", "medium", "high", "xhigh", "max"]);
  type("/reasonin");
  assert.deepEqual(shown(), ["/reasoning"]);
});

test("reset forgets the models and drops a late answer", async () => {
  let answer;
  const { context, requests, shown, slash, type } = mountSlash({
    respond: (body) => (body.method === "get_host" ? new Promise((resolve) => (answer = resolve)) : {}),
  });
  type("/");
  slash.reset();
  answer(HOST);
  await settle();
  context.session = session({ id: "s2" });
  type("/");

  assert.deepEqual(shown(), ["/model", "/reasoning", "/style"]);
  assert.equal(requests.length, 2);
});

test("a late failure of a forgotten model list shows nothing", async () => {
  let refuse;
  const { errors, slash, type } = mountSlash({
    respond: () => new Promise((_resolve, reject) => (refuse = reject)),
  });
  type("/");
  slash.reset();
  refuse(new Error("offline"));
  await settle();

  assert.deepEqual(errors, []);
});

test("/graph arms graph mode with an empty field and sends nothing", async () => {
  let armed = 0;
  const { context, press, requests, shown, suggestions, texts, type } = mountSlash({
    onGraph: () => {
      armed += 1;
    },
  });
  context.capabilities = ["host", "settings", "graph_proposal"];
  type("/");
  await settle();

  assert.deepEqual(shown(), ["/graph", "/model", "/reasoning", "/style", "/fast"]);
  type("/gr");
  assert.deepEqual(shown(), ["/graph"]);
  press("Enter");

  assert.equal(armed, 1);
  assert.deepEqual(texts, [""]);
  assert.equal(suggestions.isOpen(), false);
  assert.deepEqual(requests, [{ method: "get_host" }]);
  // An armed composer takes its task literally, so `/` opens nothing.
  context.isGraphArmed = true;
  assert.equal(type("/"), false);
});

test("/graph alone needs neither the settings nor the models, and hides while the session is busy", async () => {
  const { context, requests, shown, slash, suggestions, type } = mountSlash({ onGraph: () => {} });
  context.capabilities = ["graph_proposal"];
  type("/");
  await settle();

  assert.deepEqual(shown(), ["/graph"]);
  assert.deepEqual(requests, []);
  context.session = session({ status: "InProgress" });
  slash.refresh();
  assert.equal(suggestions.isOpen(), false);
  assert.equal(type("/"), false);
  // Without a way to arm, as in home's composer, the command is not offered.
  const home = mountSlash();
  home.context.capabilities = ["graph_proposal"];
  assert.equal(home.type("/"), false);
});

test("a value list closes once only /graph stays offered", () => {
  const { context, press, shown, slash, suggestions, type } = mountSlash({ onGraph: () => {} });
  context.capabilities = ["host", "settings", "graph_proposal"];
  type("/sty");
  press("Enter");
  assert.deepEqual(shown(), ["concise", "balanced", "detailed"]);
  context.capabilities = ["graph_proposal"];
  slash.refresh();

  assert.equal(suggestions.isOpen(), false);
});

test("a pick after the session stops offering settings only closes the menu", async () => {
  const { context, input, press, requests, suggestions, type } = mountSlash();
  type("/r");
  context.isConnected = false;
  press("Enter");
  await settle();

  assert.equal(suggestions.isOpen(), false);
  assert.equal(input.value, "/r");
  assert.equal(requests.length, 1);
});
