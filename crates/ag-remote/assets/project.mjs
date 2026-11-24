// Active project of the remote control client: the sidebar line with the
// project's name, its branch and how far that branch is from its
// upstream, the progress of its sync, and, with the `sync_project`
// capability, the button that syncs it. The project comes with each
// session listing; a host that does not report it shows only the name
// `get_host` gives. Names, branches and the host's messages only ever
// become text nodes. The module reaches the page only through the
// elements it is given, so the Node tests in
// crates/ag-remote/tests/web/project.test.mjs pass the fake DOM.

import { projectCounts, projectSyncLine } from "./format.mjs";

// Whether the host lets this device sync the active project.
export function offersProjectSync(capabilities) {
  return capabilities.includes("sync_project");
}

// Wires the project line. `elements` holds `line` (the whole block),
// `name`, `branch`, `counts` (the distance from upstream), `status` (the
// sync's progress), `error`, and `sync` (the button) with `syncLabel`.
// `request(body)` sends one request and resolves with its result.
//
// Returns `update(project)` for the project of a new listing (`null` when
// the host does not report it), `setHostName(name)` for the name
// `get_host` gives, `setCapabilities(list)`, and
// `setConnected(isConnected)`.
export function createProject({ elements, request }) {
  const state = {
    capabilities: [],
    // The name `get_host` gives, shown while no listing names the project.
    hostName: null,
    isConnected: false,
    // Whether a sync request waits for the host's answer.
    isRequesting: false,
    project: null,
  };

  function render() {
    const project = state.project;
    const name = project?.name ?? state.hostName;
    elements.line.hidden = !name;
    elements.name.textContent = name ?? "";
    elements.branch.textContent = project?.branch ?? "";
    elements.branch.hidden = !project?.branch;
    const counts = projectCounts(project);
    elements.counts.textContent = counts ?? "";
    elements.counts.hidden = !counts;
    renderStatus(projectSyncLine(project), project?.sync?.tone);
    const isSyncing = state.isRequesting || Boolean(project?.sync?.isRunning);
    elements.sync.hidden = !project || !offersProjectSync(state.capabilities);
    elements.sync.disabled = isSyncing || !state.isConnected;
    elements.sync.setAttribute("aria-busy", String(isSyncing));
    elements.syncLabel.textContent = isSyncing ? "Syncing…" : "Sync";
  }

  // The status is a live region: unchanged text is not written again, so
  // a refresh does not announce it once more.
  function renderStatus(line, tone) {
    const text = line ?? "";
    elements.status.hidden = !line;
    elements.status.className = "project-status" + (tone ? " status--" + tone : "");
    if (elements.status.textContent !== text) {
      elements.status.textContent = text;
    }
  }

  async function sync() {
    state.isRequesting = true;
    elements.error.textContent = "";
    render();
    try {
      await request({ method: "sync_project" });
    } catch (error) {
      elements.error.textContent = error.message;
    }
    state.isRequesting = false;
    render();
  }

  // A disabled button gets no clicks, and `sync` disables it at once, so
  // one sync waits for the host at a time.
  elements.sync.addEventListener("click", () => void sync());

  render();

  return {
    setCapabilities: (capabilities) => {
      state.capabilities = capabilities;
      render();
    },
    setConnected: (isConnected) => {
      state.isConnected = isConnected;
      render();
    },
    setHostName: (name) => {
      state.hostName = name;
      render();
    },
    update: (project) => {
      // The host's own report of a sync replaces an earlier refusal.
      if (project?.sync) {
        elements.error.textContent = "";
      }
      state.project = project;
      render();
    },
  };
}
