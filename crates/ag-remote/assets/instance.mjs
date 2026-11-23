// One Agentty in the sidebar of the remote control client: a heading with
// its name and connection state, its project line with the project's sync,
// `New session`, the element its session groups render into, the line
// shown when it has no sessions, and its error line. With one Agentty the
// heading stays out of the page, and the project line and `New session`
// sit above the filter, as on a page that shows only that Agentty. Names
// only ever become text nodes. Functions take `document` as a parameter,
// so the Node tests in crates/ag-remote/tests/web/instance.test.mjs pass
// the fake DOM.

// The status tone of each connection state; see the status component.
const CONNECTION_TONES = {
  connected: "ready",
  connecting: "working",
  offline: "idle",
  reconnecting: "working",
  rejected: "failed",
  waiting: "asking",
};

// Builds the view from a copy of `prototype`, the hidden
// `#instance-prototype` of index.html, whose parts carry `data-part`.
// Returns its parts: `root`, `list`, `empty`, `error`, `newSession`, and
// `project`, the elements project.mjs takes, without the shared dialog;
// and `setName(name)`, `setState(kind, label)` for a state of
// `CONNECTION_TONES` and its label, and `mount(isAlone, slot)`, which lays
// the view out alone or among others and returns the nodes that go into
// the session list.
export function createInstanceView({ document, prototype }) {
  const root = document.createElement("section");
  root.className = "instance";
  root.append(...Array.from(prototype.childNodes, (node) => node.cloneNode(true)));
  const part = (name) => root.querySelector('[data-part="' + name + '"]');
  const head = part("head");
  const top = part("top");
  const list = part("list");
  const empty = part("empty");
  const error = part("error");
  const connection = part("connection");
  const connectionLabel = part("connection-label");
  const name = part("name");
  const newSession = part("new-session");
  const project = {
    branch: part("project-branch"),
    counts: part("project-counts"),
    error: part("project-error"),
    line: part("project"),
    name: part("project-name"),
    status: part("project-status"),
    sync: part("project-sync"),
    syncLabel: part("project-sync-label"),
  };

  return {
    empty: empty,
    error: error,
    list: list,
    mount: (isAlone, slot) => {
      if (isAlone) {
        slot.replaceChildren(top);

        return [list, empty, error];
      }
      root.replaceChildren(head, top, list, empty, error);

      return [root];
    },
    newSession: newSession,
    project: project,
    root: root,
    setName: (text) => {
      name.textContent = text;
      root.setAttribute("aria-label", text);
    },
    setState: (kind, label) => {
      connection.className = "status status--" + (CONNECTION_TONES[kind] ?? "idle");
      if (connectionLabel.textContent !== label) {
        connectionLabel.textContent = label;
      }
    },
  };
}

// The name an Agentty goes by when nothing names it: the start of its
// instance identifier.
export function fallbackName(instanceId) {
  return "Agentty " + instanceId.slice(0, 8);
}
