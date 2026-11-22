import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, createDocument } from "./fake_dom.mjs";
import {
  REASONING_LEVELS,
  RESPONSE_STYLES,
  createSettings,
  currentChoice,
  offersSettings,
  offersSpeed,
  parseModels,
  settingsChange,
  withCurrent,
} from "../../assets/settings.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

const HOST = {
  kind: "host",
  models: [
    { agent: "claude", model: "claude-opus-5-5", name: "Opus 5.5", supports_fast: true },
    { agent: "claude", model: "claude-sonnet-5", name: "Sonnet 5", supports_fast: false },
    { agent: "codex", model: "gpt-6.1", name: "GPT 6.1", supports_fast: true },
  ],
  project: "agentty",
};

const MODELS = parseModels(HOST, "claude");

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

// Mounts the sheet on the client's page. `respond(body)` answers each
// request; the requests sent are recorded.
function mountSettings({ respond = (body) => (body.method === "get_host" ? HOST : { kind: "accepted" }) } = {}) {
  const document = createDocument(INDEX_HTML);
  const byId = (id) => document.getElementById(id);
  const requests = [];
  const settings = createSettings({
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
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
  });

  return { byId, requests, settings };
}

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function choose(byId, groupId, value) {
  byId(groupId).querySelector('[data-value="' + value + '"]').click();
}

function pickModel(byId, modelId) {
  byId("settings-model").value = modelId;
  byId("settings-model").dispatchEvent(new FakeEvent("change"));
}

function pressed(byId, groupId) {
  return Array.from(byId(groupId).querySelectorAll('[aria-pressed="true"]'), (button) => button.textContent);
}

test("settings need both the settings and the host capabilities", () => {
  assert.equal(offersSettings(["settings", "host"]), true);
  assert.equal(offersSettings(["settings"]), false);
  assert.equal(offersSettings(["host"]), false);
});

test("parseModels keeps the session's agent's models and reads what the host omits", () => {
  assert.deepEqual(MODELS, [
    { id: "claude-opus-5-5", name: "Opus 5.5", supportsFast: true },
    { id: "claude-sonnet-5", name: "Sonnet 5", supportsFast: false },
  ]);
  assert.equal(parseModels(HOST).length, 3);
  assert.deepEqual(parseModels({ kind: "host", models: [{ model: "m" }, { name: "no id" }, null] }), [
    { id: "m", name: "m", supportsFast: false },
  ]);
  assert.deepEqual(parseModels({ kind: "host" }), []);
  assert.deepEqual(parseModels({ kind: "accepted", models: HOST.models }), []);
});

test("settingsChange sends only what changed, and speed only for a model that runs fast", () => {
  const current = currentChoice(session());
  assert.deepEqual(current, { model: "claude-opus-5-5", reasoning: "high", speed: "normal", style: "balanced" });
  assert.deepEqual(settingsChange(session(), current, MODELS), {});
  assert.deepEqual(settingsChange(session(), { ...current, reasoning: "max" }, MODELS), { reasoning: "max" });
  assert.deepEqual(settingsChange(session(), { ...current, style: "concise", speed: "fast" }, MODELS), {
    response_style: "concise",
    speed_mode: "fast",
  });
  assert.deepEqual(settingsChange(session(), { ...current, model: "claude-sonnet-5", speed: "fast" }, MODELS), {
    model: "claude-sonnet-5",
  });
  // Without the model list, speed is no choice.
  assert.deepEqual(settingsChange(session(), { ...current, speed: "fast" }, []), {});
  assert.equal(offersSpeed(MODELS, "claude-opus-5-5"), true);
  assert.equal(offersSpeed(MODELS, "claude-sonnet-5"), false);
  assert.equal(offersSpeed(MODELS, "unknown"), false);
});

test("a setting a newer host reports joins the choices", () => {
  assert.deepEqual(withCurrent(REASONING_LEVELS, "high"), REASONING_LEVELS);
  assert.deepEqual(withCurrent(RESPONSE_STYLES, "terse"), [...RESPONSE_STYLES, "terse"]);
  assert.deepEqual(withCurrent(RESPONSE_STYLES, null), RESPONSE_STYLES);
});

test("the sheet starts at the session's settings and lists the models by name", async () => {
  const { byId, requests, settings } = mountSettings();
  settings.open(session());
  assert.equal(byId("settings-dialog").open, true);
  // Until the host answers, the session's own model is the only one.
  assert.deepEqual(Array.from(byId("settings-model").children, (option) => option.textContent), ["Opus 5.5"]);
  assert.equal(byId("settings-model").disabled, true);
  assert.equal(byId("settings-model").getAttribute("aria-busy"), "true");
  await settle();
  assert.equal(byId("settings-model").getAttribute("aria-busy"), "false");
  assert.deepEqual(requests, [{ method: "get_host" }]);
  assert.deepEqual(
    Array.from(byId("settings-model").children, (option) => [option.value, option.textContent]),
    [
      ["claude-opus-5-5", "Opus 5.5"],
      ["claude-sonnet-5", "Sonnet 5"],
    ],
  );
  assert.equal(byId("settings-model").value, "claude-opus-5-5");
  assert.equal(byId("settings-model").disabled, false);
  assert.deepEqual(pressed(byId, "settings-reasoning"), ["high"]);
  assert.deepEqual(pressed(byId, "settings-style"), ["balanced"]);
  assert.deepEqual(pressed(byId, "settings-speed"), ["normal"]);
  assert.equal(byId("settings-speed-row").hidden, false);
  assert.equal(byId("settings-apply").disabled, true);
  assert.equal(byId("settings-note").hidden, true);
});

test("speed is offered only while the chosen model runs fast", async () => {
  const { byId, settings } = mountSettings();
  settings.open(session());
  await settle();
  pickModel(byId, "claude-sonnet-5");
  assert.equal(byId("settings-speed-row").hidden, true);
  pickModel(byId, "claude-opus-5-5");
  assert.equal(byId("settings-speed-row").hidden, false);
});

test("Apply sends only the changed settings once, then closes the sheet", async () => {
  let answer;
  const { byId, requests, settings } = mountSettings({
    respond: (body) =>
      body.method === "get_host"
        ? HOST
        : new Promise((resolve) => {
            answer = resolve;
          }),
  });
  settings.open(session());
  await settle();
  choose(byId, "settings-reasoning", "max");
  choose(byId, "settings-speed", "fast");
  pickModel(byId, "claude-sonnet-5");
  assert.equal(byId("settings-speed-row").hidden, true);
  assert.equal(byId("settings-apply").disabled, false);
  byId("settings-apply").click();
  assert.equal(byId("settings-apply").disabled, true);
  assert.equal(byId("settings-apply").textContent, "Saving…");
  assert.equal(byId("settings-dialog").open, true);
  // A second submit while the first runs sends nothing.
  byId("settings-form").requestSubmit(byId("settings-apply"));
  await settle();
  assert.deepEqual(requests.slice(1), [
    { method: "set_session_settings", model: "claude-sonnet-5", reasoning: "max", session_id: "s1" },
  ]);
  answer({ kind: "accepted" });
  await settle();
  assert.equal(byId("settings-dialog").open, false);
  assert.equal(requests.length, 2);
});

test("a refused change stays in the sheet with the host's error", async () => {
  const { byId, settings } = mountSettings({
    respond: (body) => {
      if (body.method === "get_host") {
        return HOST;
      }
      throw new Error("Unknown reasoning level");
    },
  });
  settings.open(session());
  await settle();
  choose(byId, "settings-style", "detailed");
  byId("settings-apply").click();
  await settle();
  assert.equal(byId("settings-dialog").open, true);
  assert.equal(byId("settings-error").textContent, "Unknown reasoning level");
  assert.deepEqual(pressed(byId, "settings-style"), ["detailed"]);
  assert.equal(byId("settings-apply").disabled, false);
  // `Not now` closes the sheet without sending.
  byId("settings-dialog").querySelector('button[value="keep"]').click();
  assert.equal(byId("settings-dialog").open, false);
});

test("without the model list the sheet still changes the other settings", async () => {
  const { byId, requests, settings } = mountSettings({
    respond: (body) => {
      if (body.method === "get_host") {
        throw new Error("Unsupported request");
      }

      return { kind: "accepted" };
    },
  });
  settings.open(session({ reasoning: "turbo" }));
  await settle();
  assert.equal(byId("settings-error").textContent, "Could not list the models: Unsupported request");
  assert.equal(byId("settings-speed-row").hidden, true);
  assert.equal(byId("settings-model").disabled, true);
  // A level only a newer host knows shows as chosen.
  assert.deepEqual(pressed(byId, "settings-reasoning"), ["turbo"]);
  choose(byId, "settings-reasoning", "low");
  byId("settings-apply").click();
  await settle();
  assert.deepEqual(requests.at(-1), { method: "set_session_settings", reasoning: "low", session_id: "s1" });
});

test("a newer snapshot updates the running note, and a closed sheet ignores late answers", async () => {
  let hostAnswer;
  const { byId, settings } = mountSettings({
    respond: () =>
      new Promise((resolve) => {
        hostAnswer = resolve;
      }),
  });
  settings.open(session({ status: "InProgress" }), { isRunning: true });
  assert.equal(byId("settings-note").hidden, false);
  settings.update(session(), { isRunning: false });
  assert.equal(byId("settings-note").hidden, true);
  // A snapshot of another session changes nothing.
  settings.update(session({ id: "s2" }), { isRunning: true });
  assert.equal(byId("settings-note").hidden, true);
  byId("settings-dialog").close();
  hostAnswer(HOST);
  await settle();
  assert.equal(byId("settings-model").children.length, 1);
  // Closing twice is harmless.
  settings.close();
  assert.equal(byId("settings-dialog").open, false);
});

test("a snapshot arriving during a save still lets the sheet close or show the error", async () => {
  for (const outcome of ["accepted", "refused"]) {
    let answer;
    const { byId, settings } = mountSettings({
      respond: (body) =>
        body.method === "get_host"
          ? HOST
          : new Promise((resolve, reject) => {
              answer = outcome === "accepted" ? () => resolve({ kind: "accepted" }) : () => reject(new Error("Busy"));
            }),
    });
    settings.open(session({ status: "InProgress" }), { isRunning: true });
    await settle();
    choose(byId, "settings-reasoning", "low");
    byId("settings-apply").click();
    // The running session's next snapshot is a new object for the same
    // session.
    settings.update(session({ status: "InProgress", updated_at: 2 }), { isRunning: true });
    answer();
    await settle();
    if (outcome === "accepted") {
      assert.equal(byId("settings-dialog").open, false);
    } else {
      assert.equal(byId("settings-dialog").open, true);
      assert.equal(byId("settings-error").textContent, "Busy");
    }
  }
});

test("snapshots and choices update the sheet in place, so the focus stays", async () => {
  const { byId, settings } = mountSettings();
  settings.open(session());
  await settle();
  const reasoning = byId("settings-reasoning").querySelector('[data-value="medium"]');
  const options = Array.from(byId("settings-model").children);
  reasoning.focus();
  reasoning.click();
  settings.update(session({ updated_at: 3 }), { isRunning: true });
  assert.equal(byId("settings-reasoning").querySelector('[data-value="medium"]'), reasoning);
  assert.equal(reasoning.ownerDocument.activeElement, reasoning);
  assert.equal(reasoning.isConnected, true);
  assert.equal(reasoning.getAttribute("aria-pressed"), "true");
  assert.deepEqual(Array.from(byId("settings-model").children), options);
  assert.equal(byId("settings-model").children[0], options[0]);
  // A snapshot with a level this page does not know adds a button.
  settings.update(session({ reasoning: "turbo" }), { isRunning: false });
  assert.deepEqual(
    Array.from(byId("settings-reasoning").children, (button) => button.dataset.value),
    [...REASONING_LEVELS, "turbo"],
  );
  assert.deepEqual(pressed(byId, "settings-reasoning"), ["medium"]);
});
