// Home composer of the remote control client: with the `host` and
// `create_session` capabilities, home asks what the agent should work on,
// creates a session in the host's active project, sends it the first
// message, and opens it. The project name, base branch and model come from
// `get_host` and only ever become text nodes. Functions take `document`
// and `window` as parameters, so the Node tests in
// crates/ag-remote/tests/web/home.test.mjs pass the fake DOM.

import { composerKeyAction, composerRows, createDrafts } from "./chat.mjs";

// Where the unsent first message waits, among the per-session drafts.
export const HOME_DRAFT_ID = "home";
// What each session mode `get_host` lists is called on the page.
const MODE_LABELS = { harness: "Harness", regular: "Regular" };

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

  return {
    baseBranch: typeof result.base_branch === "string" ? result.base_branch : "",
    model: typeof result.default_model === "string" ? result.default_model : "",
    modes: modes,
    project: result.project,
  };
}

// The label of session mode `mode`; a mode from a newer host shows as it
// is named.
export function modeLabel(mode) {
  return Object.hasOwn(MODE_LABELS, mode) ? MODE_LABELS[mode] : mode;
}

// Wires the home composer. `elements` holds `form`, `message`, `chips`,
// `modes`, `send` and `error`, the nodes with the ids named in index.html.
// `request(body)` sends one request and resolves with its result.
// `onHost(host)` runs whenever the host description changes, `null` when
// home cannot create sessions. `onCreated(sessionId, failure)` opens a new
// session; `failure` is `null` when the first message went through, and
// otherwise `{ draft, error }` with the text to keep and why it failed.
//
// Returns `setCapabilities(list)`, which loads the host when it is
// offered, `setConnected(isConnected)`, `focus()`, and `host()`.
export function createHome({ document, window, elements, request, onHost, onCreated }) {
  const drafts = createDrafts(window);
  const state = {
    capabilities: [],
    host: null,
    isConnected: false,
    isCreating: false,
    // Counts host loads, so an answer for an older one is dropped.
    loads: 0,
    mode: null,
  };

  function isFinePointer() {
    return Boolean(window.matchMedia?.("(pointer: fine)").matches);
  }

  function render() {
    const host = state.host;
    elements.form.hidden = !host;
    elements.modes.hidden = !host || host.modes.length < 2;
    elements.send.disabled = !state.isConnected || state.isCreating;
    if (!host) {
      return;
    }
    const chips = [chip("project", host.project), chip("base", host.baseBranch)];
    if (host.model) {
      chips.push(chip(null, host.model));
    }
    elements.chips.replaceChildren(...chips.filter(Boolean));
    elements.modes.replaceChildren(
      ...(host.modes.length < 2 ? [] : host.modes.map((mode) => modeButton(mode))),
    );
  }

  // A chip such as `project agentty`, or `null` without a value.
  function chip(label, value) {
    if (!value) {
      return null;
    }
    const node = element(document, "span", "chip");
    if (label) {
      node.append(label + " ");
    }
    node.append(element(document, "b", null, value));

    return node;
  }

  function modeButton(mode) {
    const button = element(document, "button", null, modeLabel(mode));
    button.type = "button";
    button.dataset.mode = mode;
    button.setAttribute("aria-pressed", String(mode === state.mode));
    button.addEventListener("click", () => {
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

  async function create() {
    const text = elements.message.value;
    if (!text.trim() || !state.host || state.isCreating || !state.isConnected) {
      return;
    }
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
    // The new session holds the text from here on, sent or not.
    drafts.clear(HOME_DRAFT_ID);
    if (elements.message.value === text) {
      elements.message.value = "";
      resize();
    }
    let failure = null;
    try {
      await request({ method: "send_message", session_id: sessionId, message: text });
    } catch (error) {
      failure = { draft: text, error: error.message };
    }
    state.isCreating = false;
    render();
    onCreated(sessionId, failure);
  }

  function resize() {
    elements.message.rows = Math.max(3, composerRows(elements.message.value));
  }

  elements.message.value = drafts.get(HOME_DRAFT_ID);
  resize();
  elements.message.addEventListener("input", () => {
    drafts.set(HOME_DRAFT_ID, elements.message.value);
    resize();
  });
  elements.message.addEventListener("keydown", (event) => {
    if (composerKeyAction(event, { isFinePointer: isFinePointer() }) === "send") {
      event.preventDefault();
      void create();
    }
  });
  elements.form.addEventListener("submit", (event) => {
    event.preventDefault();
    void create();
  });

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
