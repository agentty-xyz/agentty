import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import { DRAFT_KEY_PREFIX } from "../../assets/composer.mjs";
import {
  HOME_DRAFT_ID,
  createHome,
  modeLabel,
  newSession,
  offersCreation,
  parseHost,
} from "../../assets/home.mjs";
import { createSettings } from "../../assets/settings.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const CAPABILITIES = ["host", "create_session"];
const HOST = {
  base_branch: "main",
  default_model: "claude-opus-5-5",
  kind: "host",
  project: "agentty",
  session_modes: ["regular"],
};

// Mounts home on the client's page. `respond(body)` answers each request;
// the requests sent, the host descriptions and the opened sessions are
// recorded.
function mountHome({
  host = HOST,
  isConnected = true,
  respond = () => ({ kind: "accepted" }),
  sessionStorage = new MemoryStorage(),
} = {}) {
  const window = createWindow(createDocument(INDEX_HTML), "http://127.0.0.1/", {
    matches: (query) => query === "(pointer: fine)",
  });
  window.sessionStorage = sessionStorage;
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const requests = [];
  const hosts = [];
  const created = [];
  const request = async (body) => {
    requests.push(body);
    if (body.method === "get_host") {
      if (host instanceof Error) {
        throw host;
      }

      return host;
    }

    return respond(body);
  };
  const home = createHome({
    document: document,
    elements: {
      error: byId("home-error"),
      form: byId("home-form"),
      hint: byId("home-composer-hint"),
      message: byId("home-message"),
      meta: byId("home-composer-meta"),
      modes: byId("home-modes"),
      send: byId("home-send"),
      settingsButton: byId("home-composer-settings"),
    },
    onCreated: (sessionId, failure) => created.push([sessionId, failure]),
    onHost: (described) => hosts.push(described),
    request: request,
    settings: createSettings({
      document: document,
      elements: {
        apply: byId("settings-apply"),
        dialog: byId("settings-dialog"),
        error: byId("settings-error"),
        form: byId("settings-form"),
        model: byId("settings-model"),
        note: byId("settings-note"),
        reasoning: byId("settings-reasoning"),
        speed: byId("settings-speed"),
        speedRow: byId("settings-speed-row"),
        style: byId("settings-style"),
      },
      request: request,
    }),
    window: window,
  });
  home.setConnected(isConnected);

  return { byId, created, document, home, hosts, requests, window };
}

function type(page, text) {
  page.byId("home-message").value = text;
  page.byId("home-message").dispatchEvent(new FakeEvent("input"));
}

// Lets pending promise callbacks run.
function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

test("creation needs both the host and create_session capabilities", () => {
  assert.equal(offersCreation(CAPABILITIES), true);
  assert.equal(offersCreation(["create_session"]), false);
  assert.equal(offersCreation(["host"]), false);
  assert.equal(offersCreation([]), false);
});

test("parseHost keeps what home shows and refuses other answers", () => {
  assert.deepEqual(parseHost({ ...HOST, session_modes: ["regular", "harness", 3] }), {
    baseBranch: "main",
    model: "claude-opus-5-5",
    modeModels: {},
    modes: ["regular", "harness"],
    project: "agentty",
  });
  assert.deepEqual(parseHost({ kind: "host", project: "p" }), {
    baseBranch: "",
    model: "",
    modeModels: {},
    modes: [],
    project: "p",
  });
  assert.equal(parseHost({ kind: "accepted" }), null);
  assert.equal(parseHost({ kind: "host" }), null);
  assert.equal(parseHost(null), null);
  assert.equal(modeLabel("regular"), "Regular");
  assert.equal(modeLabel("harness"), "Harness");
  assert.equal(modeLabel("swarm"), "swarm");
});

test("without the capabilities home asks nothing and offers no composer", async () => {
  const page = mountHome();
  await page.home.setCapabilities(["create_session"]);

  assert.deepEqual(page.requests, []);
  assert.deepEqual(page.hosts, [null]);
  assert.equal(page.home.host(), null);
  assert.equal(page.byId("home-form").hidden, true);
  assert.equal(page.byId("home-modes").hidden, true);
});

test("the composer is the session's, with the new session's settings as text", async () => {
  const name = "<img src=x onerror=alert(1)>";
  const page = mountHome({
    host: {
      ...HOST,
      mode_models: [
        {
          agent: "claude",
          mode: "regular",
          model: "claude-opus-5-5",
          name: name,
          reasoning: "high",
          response_style: "concise",
          speed_mode: "fast",
        },
      ],
    },
  });
  await page.home.setCapabilities(CAPABILITIES);

  assert.deepEqual(page.requests, [{ method: "get_host" }]);
  assert.equal(page.byId("home-form").hidden, false);
  // One mode needs no choice.
  assert.equal(page.byId("home-modes").hidden, true);
  const message = page.byId("home-message");
  assert.equal(message.placeholder, "Describe the task");
  assert.equal(message.rows, 1);
  assert.equal(page.byId("home-send").getAttribute("aria-label"), "Start session");
  // Without the settings capability the line is text, as in a session.
  assert.equal(page.byId("home-composer-settings").hidden, true);
  const meta = page.byId("home-composer-meta");
  assert.equal(meta.hidden, false);
  assert.equal(meta.textContent, name + " [high]·concise·fast speed");
  assert.equal(meta.querySelector("b").childNodes[0].nodeType, 3);
  assert.equal(page.document.querySelectorAll("img").length, 0);
  assert.equal(page.byId("home-composer-hint").textContent, "Enter sends · Shift+Enter adds a line");

  page.home.setConnected(false);
  assert.equal(message.placeholder, "Offline. Your draft is kept.");
  assert.equal(page.byId("home-send").disabled, true);
});

test("a host that names no model for the mode shows the key hint in the line", async () => {
  const page = mountHome({ host: { kind: "host", project: "agentty" } });
  await page.home.setCapabilities(CAPABILITIES);

  assert.equal(page.byId("home-composer-meta").textContent, "Enter sends · Shift+Enter adds a line");
  assert.equal(page.byId("home-composer-hint").textContent, "");
});

test("several modes offer a choice, and the chosen one is sent", async () => {
  const page = mountHome({
    host: { ...HOST, session_modes: ["regular", "harness"] },
    respond: (body) => (body.method === "create_session" ? { kind: "created", session_id: "new" } : { kind: "accepted" }),
  });
  await page.home.setCapabilities(CAPABILITIES);
  const modes = page.byId("home-modes");
  assert.equal(modes.hidden, false);
  assert.deepEqual(
    Array.from(modes.children, (button) => [button.textContent, button.getAttribute("aria-pressed")]),
    [
      ["Regular", "true"],
      ["Harness", "false"],
    ],
  );

  modes.querySelector('[data-mode="harness"]').click();
  assert.equal(modes.querySelector('[data-mode="harness"]').getAttribute("aria-pressed"), "true");
  // A reload of the host keeps the choice while it is still offered.
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(page.requests.slice(2, 3), [{ method: "create_session", mode: "harness" }]);
});

test("sending creates the session, sends the first message, then opens it", async () => {
  const sessionStorage = new MemoryStorage();
  const page = mountHome({
    respond: (body) => (body.method === "create_session" ? { kind: "created", session_id: "new" } : { kind: "accepted" }),
    sessionStorage: sessionStorage,
  });
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay\nand test it");
  assert.equal(sessionStorage.getItem(DRAFT_KEY_PREFIX + HOME_DRAFT_ID), "Fix the relay\nand test it");

  page.byId("home-message").dispatchEvent(new FakeEvent("keydown", { key: "Enter" }));
  assert.equal(page.byId("home-send").disabled, true);
  await settle();

  assert.deepEqual(page.requests.slice(1), [
    { method: "create_session", mode: "regular" },
    { method: "send_message", message: "Fix the relay\nand test it", session_id: "new" },
  ]);
  assert.deepEqual(page.created, [["new", null]]);
  assert.equal(page.byId("home-message").value, "");
  assert.equal(page.byId("home-message").rows, 1);
  assert.equal(sessionStorage.getItem(DRAFT_KEY_PREFIX + HOME_DRAFT_ID), null);
  assert.equal(page.byId("home-send").disabled, false);
  assert.equal(page.byId("home-error").textContent, "");
});

test("text typed while the session is created stays on home, and in storage", async () => {
  const sessionStorage = new MemoryStorage();
  let release;
  const page = mountHome({
    respond: (body) =>
      body.method === "create_session"
        ? new Promise((resolve) => {
            release = () => resolve({ kind: "created", session_id: "new" });
          })
        : { kind: "accepted" },
    sessionStorage: sessionStorage,
  });
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();
  type(page, "Then the docs");
  release();
  await settle();

  assert.deepEqual(page.created, [["new", null]]);
  assert.equal(page.byId("home-message").value, "Then the docs");
  assert.equal(sessionStorage.getItem(DRAFT_KEY_PREFIX + HOME_DRAFT_ID), "Then the docs");
});

test("a failed first message still opens the new session with the text kept", async () => {
  const page = mountHome({
    respond: (body) => {
      if (body.method === "send_message") {
        throw new Error("Session is busy");
      }

      return { kind: "created", session_id: "new" };
    },
  });
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(page.created, [["new", { draft: "Fix the relay", error: "Session is busy" }]]);
  assert.equal(page.byId("home-message").value, "");
});

test("a refused creation keeps the text on home and says why", async () => {
  const page = mountHome({
    respond: () => {
      throw new Error("No active project");
    },
  });
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(page.created, []);
  assert.equal(page.byId("home-message").value, "Fix the relay");
  assert.equal(page.byId("home-error").textContent, "No active project");
  assert.equal(page.byId("home-send").disabled, false);
});

test("blank text, a second send and an offline page create nothing", async () => {
  const page = mountHome({ isConnected: false });
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  assert.equal(page.byId("home-send").disabled, true);
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));

  page.home.setConnected(true);
  type(page, "   ");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  // Shift+Enter adds a line instead of sending.
  type(page, "Fix the relay");
  page.byId("home-message").dispatchEvent(new FakeEvent("keydown", { key: "Enter", shiftKey: true }));
  await settle();

  assert.deepEqual(page.requests, [{ method: "get_host" }]);
});

test("home's composer has no earlier prompts, so Up and Down stay the browser's", async () => {
  const page = mountHome();
  await page.home.setCapabilities(CAPABILITIES);
  type(page, "Fix the relay");
  for (const key of ["ArrowUp", "ArrowDown"]) {
    const event = new FakeEvent("keydown", { key: key });
    page.byId("home-message").dispatchEvent(event);
    assert.equal(event.defaultPrevented, false, key);
  }
  assert.equal(page.byId("home-message").value, "Fix the relay");
});

test("the unsent first message survives a reload of the tab", () => {
  const sessionStorage = new MemoryStorage();
  sessionStorage.setItem(DRAFT_KEY_PREFIX + HOME_DRAFT_ID, "one\ntwo\nthree\nfour");
  const page = mountHome({ sessionStorage: sessionStorage });

  assert.equal(page.byId("home-message").value, "one\ntwo\nthree\nfour");
  assert.equal(page.byId("home-message").rows, 4);
});

test("a host that cannot describe its project offers no composer and says why", async () => {
  const page = mountHome({ host: new Error("No active project") });
  await page.home.setCapabilities(CAPABILITIES);

  assert.deepEqual(page.hosts, [null]);
  assert.equal(page.byId("home-form").hidden, true);
  assert.equal(page.byId("home-error").textContent, "Could not describe the project: No active project");
  page.home.focus();
});

test("only the latest host load counts, and focus goes to the composer", async () => {
  const page = mountHome();
  const first = page.home.setCapabilities(CAPABILITIES);
  const second = page.home.setCapabilities([]);
  await Promise.all([first, second]);

  assert.deepEqual(page.hosts, [null]);
  await page.home.setCapabilities(CAPABILITIES);
  page.home.focus();
  assert.equal(page.document.activeElement, page.byId("home-message"));
});

// A host that starts regular sessions on Claude and harness ones on Kimi,
// with two Claude models and one harness model to pick from.
const PICKING_HOST = {
  ...HOST,
  mode_models: [
    {
      agent: "claude",
      mode: "regular",
      model: "claude-opus-5-5",
      name: "Opus 5.5",
      reasoning: "high",
      response_style: "concise",
      speed_mode: "normal",
    },
    { agent: "harness", mode: "harness", model: "kimi-k3", name: "Kimi K3", reasoning: "high", response_style: "concise" },
    { agent: "claude", mode: "draft", model: "claude-opus-5-5", name: "Opus 5.5" },
    { agent: "claude", mode: "regular", model: "" },
    null,
  ],
  models: [
    { agent: "claude", model: "claude-opus-5-5", name: "Opus 5.5", supports_fast: true },
    { agent: "claude", model: "claude-sonnet-5-5", name: "Sonnet 5.5" },
    { agent: "codex", model: "gpt-6.1-sol", name: "gpt-6.1-sol" },
    { agent: "harness", model: "kimi-k3", name: "Kimi K3" },
  ],
  session_modes: ["regular", "harness"],
};
const PICKING_CAPABILITIES = [...CAPABILITIES, "settings"];

function createdThen(respond = () => ({ kind: "accepted" })) {
  return (body) => (body.method === "create_session" ? { kind: "created", session_id: "new" } : respond(body));
}

// Picks `value` in the settings sheet's `groupId` buttons, or the model
// picker when `groupId` is `settings-model`.
function choose(page, groupId, value) {
  const group = page.byId(groupId);
  if (groupId === "settings-model") {
    group.value = value;
    group.dispatchEvent(new FakeEvent("change"));
  } else {
    Array.from(group.children)
      .find((button) => button.dataset.value === value)
      .click();
  }
}

// Opens the settings sheet from the line, picks `choices`, and applies.
async function pick(page, choices) {
  page.byId("home-composer-settings").click();
  await settle();
  for (const [groupId, value] of Object.entries(choices)) {
    choose(page, groupId, value);
  }
  page.byId("settings-apply").click();
  await settle();
}

function settingsLine(page) {
  return page.byId("home-composer-settings").textContent;
}

test("parseHost keeps each mode's starting settings and the models of its agent", () => {
  const host = parseHost(PICKING_HOST);

  assert.deepEqual(host.modeModels, {
    harness: {
      agent: "harness",
      id: "kimi-k3",
      name: "Kimi K3",
      options: [{ id: "kimi-k3", name: "Kimi K3", supportsFast: false }],
      reasoning: "high",
      speed: null,
      style: "concise",
    },
    regular: {
      agent: "claude",
      id: "claude-opus-5-5",
      name: "Opus 5.5",
      options: [
        { id: "claude-opus-5-5", name: "Opus 5.5", supportsFast: true },
        { id: "claude-sonnet-5-5", name: "Sonnet 5.5", supportsFast: false },
      ],
      reasoning: "high",
      speed: "normal",
      style: "concise",
    },
  });
});

test("newSession describes the session a mode would start, with the picks applied", () => {
  const host = parseHost(PICKING_HOST);

  assert.deepEqual(newSession(host, "regular"), {
    agent: "claude",
    id: "new:regular",
    model: "Opus 5.5",
    model_id: "claude-opus-5-5",
    reasoning: "high",
    response_style: "concise",
    speed_mode: "normal",
    status: "Draft",
  });
  assert.deepEqual(
    newSession(host, "regular", { model: "claude-sonnet-5-5", reasoning: "max", response_style: "", speed_mode: "fast" }),
    {
      agent: "claude",
      id: "new:regular",
      model: "Sonnet 5.5",
      model_id: "claude-sonnet-5-5",
      reasoning: "max",
      response_style: "concise",
      speed_mode: "fast",
      status: "Draft",
    },
  );
  // A model the host does not list shows by its id.
  assert.equal(newSession(host, "regular", { model: "claude-haiku-5-5" }).model, "claude-haiku-5-5");
  assert.equal(newSession(host, "draft"), null);
  assert.equal(newSession(null, "regular"), null);
});

test("the settings line opens the session's settings sheet and keeps the picks on home", async () => {
  const page = mountHome({ host: PICKING_HOST });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  const line = page.byId("home-composer-settings");
  assert.equal(line.hidden, false);
  assert.equal(page.byId("home-composer-meta").hidden, true);
  assert.equal(settingsLine(page), "Opus 5.5 [high]·concise·normal speed");

  line.click();
  await settle();
  assert.equal(page.byId("settings-dialog").open, true);
  // Only the starting agent's models are offered, by name.
  assert.deepEqual(
    Array.from(page.byId("settings-model").children, (option) => [option.value, option.textContent]),
    [
      ["claude-opus-5-5", "Opus 5.5"],
      ["claude-sonnet-5-5", "Sonnet 5.5"],
    ],
  );
  choose(page, "settings-reasoning", "max");
  choose(page, "settings-style", "detailed");
  choose(page, "settings-speed", "fast");
  page.byId("settings-apply").click();
  await settle();

  // Nothing is sent before the session exists.
  assert.deepEqual(
    page.requests.map((body) => body.method),
    ["get_host", "get_host"],
  );
  assert.equal(page.byId("settings-dialog").open, false);
  assert.equal(settingsLine(page), "Opus 5.5 [max]·detailed·fast speed");

  // Harness sessions start on another agent: the model and speed picks
  // stay behind, the reasoning and style ones carry over.
  page.byId("home-modes").querySelector('[data-mode="harness"]').click();
  assert.equal(settingsLine(page), "Kimi K3 [max]·detailed");
  page.byId("home-modes").querySelector('[data-mode="regular"]').click();
  assert.equal(settingsLine(page), "Opus 5.5 [max]·detailed·normal speed");

  // Offline, the line cannot open the sheet.
  page.home.setConnected(false);
  assert.equal(line.disabled, true);
});

test("without the settings capability the line is text and opens nothing", async () => {
  const page = mountHome({ host: PICKING_HOST });
  await page.home.setCapabilities(CAPABILITIES);

  assert.equal(page.byId("home-composer-settings").hidden, true);
  assert.equal(page.byId("home-composer-meta").textContent, "Opus 5.5 [high]·concise·normal speed");
  type(page, "/");
  await settle();
  assert.equal(page.byId("home-message-suggestions").children.length, 0);
});

test("slash commands pick the new session's settings, and @ lists no files without project_files", async () => {
  const page = mountHome({ host: PICKING_HOST });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  const message = page.byId("home-message");
  const suggested = () =>
    Array.from(page.byId("home-message-suggestions").children, (option) => option.dataset.key);

  type(page, "/");
  await settle();
  assert.equal(message.getAttribute("aria-expanded"), "true");
  assert.deepEqual(suggested(), ["/model", "/reasoning", "/style", "/fast"]);
  type(page, "/mo");
  message.dispatchEvent(new FakeEvent("keydown", { key: "Enter" }));
  assert.deepEqual(suggested(), ["claude-opus-5-5", "claude-sonnet-5-5"]);
  message.dispatchEvent(new FakeEvent("keydown", { key: "ArrowDown" }));
  message.dispatchEvent(new FakeEvent("keydown", { key: "Enter" }));
  await settle();

  assert.equal(message.value, "");
  assert.equal(settingsLine(page), "Sonnet 5.5 [high]·concise·normal speed");
  // Only `get_host` went out: home and the menu each asked for the models.
  assert.deepEqual(
    page.requests.map((body) => body.method),
    ["get_host", "get_host"],
  );

  // The host does not offer the project checkout, so there is nothing to
  // list.
  type(page, "Fix @src");
  await settle();
  assert.equal(message.getAttribute("aria-expanded"), "false");
  assert.equal(page.requests.length, 2);
});

test("@ lists the project checkout's files for the session about to start", async () => {
  const page = mountHome({
    respond: (body) =>
      body.method === "list_project_files"
        ? {
            files: [
              { is_dir: true, path: "src" },
              { is_dir: false, path: "src/lib.rs" },
            ],
            kind: "session_files",
          }
        : { kind: "accepted" },
  });
  await page.home.setCapabilities([...CAPABILITIES, "project_files"]);
  const message = page.byId("home-message");

  type(page, "Fix @lib");
  await settle();

  assert.equal(message.getAttribute("aria-expanded"), "true");
  assert.deepEqual(
    Array.from(page.byId("home-message-suggestions").children, (option) => option.dataset.key),
    ["src/lib.rs"],
  );
  assert.deepEqual(page.requests.slice(1), [{ method: "list_project_files" }]);
  message.dispatchEvent(new FakeEvent("keydown", { key: "Enter" }));
  assert.equal(message.value, "Fix @src/lib.rs ");
});

test("home described for another Agentty drops what it looked up for the first", async () => {
  // Arrange
  const page = mountHome({
    respond: (body) =>
      body.method === "list_project_files"
        ? { files: [{ is_dir: false, path: "lib.rs" }], kind: "session_files" }
        : { kind: "accepted" },
  });
  const capabilities = [...CAPABILITIES, "project_files"];
  await page.home.setCapabilities(capabilities, "first");
  type(page, "@lib");
  await settle();

  // Act
  const reload = page.home.setCapabilities(capabilities, "second");
  const hostWhileLoading = page.home.host();
  await reload;
  type(page, "@li");
  await settle();
  type(page, "@lib");
  await settle();

  // Assert
  assert.equal(hostWhileLoading, null);
  assert.equal(page.home.host().scope, "second");
  assert.equal(newSession(page.home.host(), "regular"), null);
  assert.equal(page.home.isCreating(), false);
  // Each Agentty's project files are looked up once.
  assert.equal(page.requests.filter((body) => body.method === "list_project_files").length, 2);
});

test("newSession ids name the Agentty a host was described for", () => {
  const host = parseHost({
    ...HOST,
    mode_models: [{ agent: "codex", mode: "regular", model: "gpt-6.1-sol" }],
  });

  assert.equal(newSession(host, "regular").id, "new:regular");
  assert.equal(newSession({ ...host, scope: "abc" }, "regular").id, "new:abc:regular");
});

test("picked settings are applied before the first message is sent", async () => {
  const page = mountHome({ host: PICKING_HOST, respond: createdThen() });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  await pick(page, { "settings-model": "claude-sonnet-5-5", "settings-reasoning": "low" });
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  // The settings stay as picked while the session is created.
  assert.equal(page.byId("home-composer-settings").disabled, true);
  await settle();

  assert.deepEqual(page.requests.slice(2), [
    { method: "create_session", mode: "regular" },
    { method: "set_session_settings", model: "claude-sonnet-5-5", reasoning: "low", session_id: "new" },
    { method: "send_message", message: "Fix the relay", session_id: "new" },
  ]);
  assert.deepEqual(page.created, [["new", null]]);
  assert.equal(page.byId("home-composer-settings").disabled, false);
});

test("keeping the starting settings sends no settings change", async () => {
  const page = mountHome({ host: PICKING_HOST, respond: createdThen() });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  await pick(page, { "settings-model": "claude-sonnet-5-5" });
  await pick(page, { "settings-model": "claude-opus-5-5" });
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(
    page.requests.filter((body) => body.method !== "get_host").map((body) => body.method),
    ["create_session", "send_message"],
  );
});

test("failed settings open the session with the message unsent", async () => {
  const page = mountHome({
    host: PICKING_HOST,
    respond: createdThen((body) => {
      if (body.method === "set_session_settings") {
        throw new Error("Model `claude-sonnet-5-5` is not available");
      }

      return { kind: "accepted" };
    }),
  });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  await pick(page, { "settings-model": "claude-sonnet-5-5" });
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(
    page.requests.filter((body) => body.method !== "get_host").map((body) => body.method),
    ["create_session", "set_session_settings"],
  );
  assert.deepEqual(page.created, [
    [
      "new",
      { draft: "Fix the relay", error: "Could not apply the settings: Model `claude-sonnet-5-5` is not available" },
    ],
  ]);
});

test("settings picked while the capability was offered are dropped once it is not", async () => {
  const page = mountHome({ host: PICKING_HOST, respond: createdThen() });
  await page.home.setCapabilities(PICKING_CAPABILITIES);
  await pick(page, { "settings-reasoning": "low" });
  await page.home.setCapabilities(CAPABILITIES);
  assert.equal(page.byId("home-composer-meta").textContent, "Opus 5.5 [high]·concise·normal speed");
  type(page, "Fix the relay");
  page.byId("home-form").dispatchEvent(new FakeEvent("submit"));
  await settle();

  assert.deepEqual(
    page.requests.filter((body) => body.method !== "get_host").map((body) => body.method),
    ["create_session", "send_message"],
  );
});
