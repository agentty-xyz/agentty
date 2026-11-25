// Project line of the remote control client: the sidebar line with the
// project's name, its branch and how far that branch is from its
// upstream, the progress of its sync, and, with the `sync_project`
// capability, the button that syncs it. The project comes with each
// session listing; a host that does not report it shows only the name
// `get_host` gives.
//
// With the `projects` capability the device picks its own project among
// those the host lists, in a select on the line, instead of following the
// one the terminal has open. The page starts on the terminal's project,
// or on the one this browser picked last for the same Agentty, and keeps
// to it by id when the terminal switches. Requests about the project then
// name it in `project_id`. Without the capability, or with one project,
// the line follows the terminal's project as before.
//
// Names, branches and the host's messages only ever become text nodes.
// The module reaches the page only through the elements it is given, so
// the Node tests in crates/ag-remote/tests/web/project.test.mjs pass the
// fake DOM.

import { projectCounts, projectSyncLine } from "./format.mjs";

// Where this browser keeps the project it picked on each Agentty: a JSON
// object from the Agentty's instance id, `""` without a relay, to the id
// of the project.
export const PROJECT_PICKS_KEY = "agentty.remote.projects";
// What the line says when the project shown or picked here is no longer
// listed.
const FALLBACK_NOTE = "The project shown here is no longer listed, so this shows the terminal's project.";

// Whether the host lets this device sync the project it shows.
export function offersProjectSync(capabilities) {
  return capabilities.includes("sync_project");
}

// Whether the host lets this device pick one of its projects.
export function offersProjects(capabilities) {
  return capabilities.includes("projects");
}

// The projects a `list_projects` answer lists, in its order, as
// `{ id, name, isActive, activeSessions }`; entries without an id or a
// name are left out.
export function parseProjects(result) {
  if (result?.kind !== "projects" || !Array.isArray(result.projects)) {
    return [];
  }

  return result.projects
    .filter(
      (entry) =>
        typeof entry?.id === "string" && entry.id !== "" && typeof entry.name === "string" && entry.name !== "",
    )
    .map((entry) => ({
      activeSessions: Number.isInteger(entry.active_sessions) && entry.active_sessions > 0 ? entry.active_sessions : 0,
      id: entry.id,
      isActive: entry.is_active === true,
      name: entry.name,
    }));
}

// The project to show among `projects`: the one with id `wanted` while it
// is listed, otherwise the terminal's, otherwise the first; `null` when
// none is listed.
export function pickProject(projects, wanted) {
  const listed = projects.find((project) => project.id === wanted);

  return (listed ?? projects.find((project) => project.isActive) ?? projects[0] ?? null)?.id ?? null;
}

// Reads and writes the project this browser picked on the Agentty with
// `instanceId` in `storage`, which may throw, as `localStorage` can in a
// private window; the pick then lasts only for this page.
export function projectPicks(storage, instanceId) {
  function read() {
    try {
      const picks = JSON.parse(storage.getItem(PROJECT_PICKS_KEY) ?? "{}");

      return picks && typeof picks === "object" && !Array.isArray(picks) ? picks : {};
    } catch (_error) {
      return {};
    }
  }

  return {
    load: () => {
      const pick = read()[instanceId];

      return typeof pick === "string" ? pick : null;
    },
    save: (projectId) => {
      const picks = read();
      if (projectId === null) {
        delete picks[instanceId];
      } else {
        picks[instanceId] = projectId;
      }
      try {
        storage.setItem(PROJECT_PICKS_KEY, JSON.stringify(picks));
      } catch (_error) {
        // Storage can be unavailable; the pick then lasts for this page.
      }
    },
  };
}

// Wires the project line. `elements` holds `line` (the whole block),
// `name`, `select` (the project picker), `branch`, `counts` (the distance
// from upstream), `status` (the sync's progress), `error`, and `sync` (the
// button) with `syncLabel`. `request(body)` sends one request and resolves
// with its result. `picks` is `projectPicks` for this Agentty, or `null`
// to remember nothing. `onSelect(projectId)` runs when the user picks
// another project.
//
// Returns `update(project)` for the project of a new listing (`null` when
// the host does not report it), `setHostName(name)` for the name
// `get_host` gives, `setProjects(list)` for the projects `parseProjects`
// read, `projectId()` for the id requests name the project by (`null` for
// the terminal's project), `listFailed(message)` when the host could not
// list them, `awaitsProjects()`, whether the host offers `projects` but has
// not answered a listing yet, `setCapabilities(list)`, and
// `setConnected(isConnected)`.
export function createProject({ elements, request, picks = null, onSelect = () => {} }) {
  const state = {
    capabilities: [],
    // The name `get_host` gives, shown while no listing names the project.
    hostName: null,
    isConnected: false,
    // Whether the host listed its projects, or failed to, since it offered
    // `projects`.
    isListed: false,
    // What the line says about the last listing that failed, until one
    // goes through.
    listError: null,
    project: null,
    // The id of the project this device shows, once the host listed its
    // projects; `null` follows the terminal's.
    projectId: null,
    // The projects the host lists, while it offers `projects`.
    projects: [],
    // The sync request waiting for the host's answer, with the project it
    // is for, or `null`.
    syncing: null,
  };

  function render() {
    const project = state.project;
    const name = project?.name ?? state.hostName;
    const isPicking = offersProjects(state.capabilities) && state.projects.length > 1;
    elements.line.hidden = !name && !isPicking;
    elements.name.textContent = name ?? "";
    elements.name.hidden = isPicking;
    elements.select.hidden = !isPicking;
    renderOptions(isPicking ? state.projects : []);
    elements.branch.textContent = project?.branch ?? "";
    elements.branch.hidden = !project?.branch;
    const counts = projectCounts(project);
    elements.counts.textContent = counts ?? "";
    elements.counts.hidden = !counts;
    renderStatus(projectSyncLine(project), project?.sync?.tone);
    const isRequesting = state.syncing !== null && state.syncing.projectId === state.projectId;
    const isSyncing = isRequesting || Boolean(project?.sync?.isRunning);
    elements.sync.hidden = !project || !offersProjectSync(state.capabilities);
    elements.sync.disabled = isSyncing || !state.isConnected;
    elements.sync.setAttribute("aria-busy", String(isSyncing));
    elements.syncLabel.textContent = isSyncing ? "Syncing…" : "Sync";
  }

  // One option per listed project, the terminal's marked, rebuilt only
  // when the list changes so an open picker stays as it is.
  function renderOptions(projects) {
    const labels = projects.map((project) => [project.id, project.name + (project.isActive ? " · terminal" : "")]);
    const shown = Array.from(elements.select.children, (option) => [option.value, option.textContent]);
    if (JSON.stringify(labels) !== JSON.stringify(shown)) {
      elements.select.replaceChildren(
        ...labels.map(([id, label]) => {
          const option = elements.select.ownerDocument.createElement("option");
          option.value = id;
          option.textContent = label;

          return option;
        }),
      );
    }
    elements.select.value = state.projectId ?? "";
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

  // The sync waits for the host for the project it was sent for; its
  // answer shows only while the line still shows that project.
  async function sync() {
    const syncing = { projectId: state.projectId };
    state.syncing = syncing;
    elements.error.textContent = "";
    render();
    const body = { method: "sync_project" };
    if (syncing.projectId !== null) {
      body.project_id = syncing.projectId;
    }
    try {
      await request(body);
    } catch (error) {
      if (state.projectId === syncing.projectId) {
        elements.error.textContent = error.message;
      }
    }
    if (state.syncing === syncing) {
      state.syncing = null;
    }
    render();
  }

  // Keeps to the project this device shows while the host lists it. The
  // first list starts on the project picked here last, or else the
  // terminal's; when the project shown or picked drops out of the list,
  // the line falls back to the terminal's and says so, and forgets a pick
  // that is gone.
  function setProjects(projects) {
    if (!offersProjects(state.capabilities)) {
      forgetProjects();

      return;
    }
    const stored = picks?.load() ?? null;
    const wanted = state.projectId ?? stored;
    // A listing that went through replaces the earlier failure's note.
    if (state.listError !== null && elements.error.textContent === state.listError) {
      elements.error.textContent = "";
    }
    state.listError = null;
    state.isListed = true;
    state.projects = projects;
    state.projectId = pickProject(projects, wanted);
    if (wanted !== null && state.projectId !== wanted) {
      if (stored === wanted) {
        picks?.save(null);
      }
      elements.error.textContent = FALLBACK_NOTE;
    }
    render();
  }

  // The host could not list its projects: the line keeps the project it
  // shows, the terminal's until one was listed, and says why.
  function listFailed(message) {
    state.isListed = true;
    state.listError = "Could not list the projects: " + message;
    elements.error.textContent = state.listError;
    render();
  }

  // Follows the terminal's project again, as for a host that does not
  // offer `projects`.
  function forgetProjects() {
    state.isListed = false;
    state.projects = [];
    state.projectId = null;
    render();
  }

  // A disabled button gets no clicks, and `sync` disables it at once, so
  // one sync waits for the host at a time.
  elements.sync.addEventListener("click", () => void sync());

  elements.select.addEventListener("change", () => {
    const projectId = elements.select.value;
    if (projectId === state.projectId || !state.projects.some((project) => project.id === projectId)) {
      render();

      return;
    }
    state.projectId = projectId;
    // Another project's listing replaces this one's.
    state.project = null;
    elements.error.textContent = "";
    picks?.save(projectId);
    render();
    onSelect(projectId);
  });

  render();

  return {
    awaitsProjects: () => offersProjects(state.capabilities) && !state.isListed,
    listFailed: listFailed,
    projectId: () => state.projectId,
    setCapabilities: (capabilities) => {
      state.capabilities = capabilities;
      if (offersProjects(capabilities)) {
        render();
      } else {
        forgetProjects();
      }
    },
    setConnected: (isConnected) => {
      state.isConnected = isConnected;
      render();
    },
    setHostName: (name) => {
      state.hostName = name;
      render();
    },
    setProjects: setProjects,
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
