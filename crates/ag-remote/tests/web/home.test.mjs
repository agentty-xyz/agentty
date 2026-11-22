import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";
import { DRAFT_KEY_PREFIX } from "../../assets/chat.mjs";
import {
  HOME_DRAFT_ID,
  createHome,
  modeLabel,
  offersCreation,
  parseHost,
} from "../../assets/home.mjs";

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
  const home = createHome({
    document: document,
    elements: {
      chips: byId("home-chips"),
      error: byId("home-error"),
      form: byId("home-form"),
      message: byId("home-message"),
      modes: byId("home-modes"),
      send: byId("home-send"),
    },
    onCreated: (sessionId, failure) => created.push([sessionId, failure]),
    onHost: (described) => hosts.push(described),
    request: async (body) => {
      requests.push(body);
      if (body.method === "get_host") {
        if (host instanceof Error) {
          throw host;
        }

        return host;
      }

      return respond(body);
    },
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
    modes: ["regular", "harness"],
    project: "agentty",
  });
  assert.deepEqual(parseHost({ kind: "host", project: "p" }), {
    baseBranch: "",
    model: "",
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

test("the composer shows the project, base branch and model as text", async () => {
  const project = "<img src=x onerror=alert(1)>";
  const page = mountHome({ host: { ...HOST, project: project } });
  await page.home.setCapabilities(CAPABILITIES);

  assert.deepEqual(page.requests, [{ method: "get_host" }]);
  assert.equal(page.hosts[0].project, project);
  assert.equal(page.byId("home-form").hidden, false);
  // One mode needs no choice.
  assert.equal(page.byId("home-modes").hidden, true);
  const chips = page.byId("home-chips").children;
  assert.deepEqual(
    chips.map((chip) => chip.textContent),
    ["project " + project, "base main", "claude-opus-5-5"],
  );
  assert.equal(chips[0].querySelector("b").childNodes[0].nodeType, 3);
  assert.equal(page.document.querySelectorAll("img").length, 0);
});

test("a host without a base branch or model shows only its project", async () => {
  const page = mountHome({ host: { kind: "host", project: "agentty" } });
  await page.home.setCapabilities(CAPABILITIES);

  assert.deepEqual(
    page.byId("home-chips").children.map((chip) => chip.textContent),
    ["project agentty"],
  );
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
    modes.children.map((button) => [button.textContent, button.getAttribute("aria-pressed")]),
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
  assert.equal(page.byId("home-message").rows, 3);
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
