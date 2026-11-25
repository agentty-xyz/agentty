import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument } from "./fake_dom.mjs";
import { parseProject } from "../../assets/format.mjs";
import { createInstanceView } from "../../assets/instance.mjs";
import {
  PROJECT_PICKS_KEY,
  createProject,
  offersProjectSync,
  offersProjects,
  parseProjects,
  pickProject,
  projectPicks,
} from "../../assets/project.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
// The parts of an Agentty's view in instance.mjs, by the names these tests
// use.
const PARTS = {
  project: "line",
  "project-branch": "branch",
  "project-counts": "counts",
  "project-error": "error",
  "project-name": "name",
  "project-select": "select",
  "project-status": "status",
  "project-sync": "sync",
  "project-sync-label": "syncLabel",
};
// The projects a host lists: `agentty`, which the terminal has open, and
// `website`.
const PROJECTS = parseProjects({
  kind: "projects",
  projects: [
    { active_sessions: 2, id: "1", is_active: true, name: "agentty" },
    { id: "2", name: "website" },
  ],
});
// What the line says when the project picked here is no longer listed.
const FALLBACK_NOTE = "The project picked here is no longer listed, so this shows the terminal's project.";

// The project a listing names, with `fields` over `agentty` on `main`,
// two commits behind its upstream.
function listed(fields = {}) {
  return parseProject({
    project: { ahead: 0, behind: 2, branch: "main", name: "agentty", ...fields },
  });
}

// Mounts the project line of an Agentty's view on the client's page,
// connected and with `capabilities`. `respond(body)` answers each request;
// the requests sent and the projects picked are recorded, and picks are
// kept in `storage` for the Agentty `instanceId`. `byId` finds the view's
// parts by the names in `PARTS` and the page's elements by id.
function mountProject({
  capabilities = ["sync_project"],
  document = createDocument(INDEX_HTML),
  instanceId = "",
  respond = () => ({ kind: "accepted" }),
  storage = new MemoryStorage(),
} = {}) {
  const view = createInstanceView({
    document: document,
    prototype: document.getElementById("instance-prototype"),
  });
  document.getElementById("sessions").append(view.root);
  const byId = (id) => (Object.hasOwn(PARTS, id) ? view.project[PARTS[id]] : document.getElementById(id));
  const requests = [];
  const picked = [];
  const project = createProject({
    elements: view.project,
    onSelect: (projectId) => picked.push(projectId),
    picks: projectPicks(storage, instanceId),
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
  });
  project.setCapabilities(capabilities);
  project.setConnected(true);

  return { byId, document, picked, project, requests, storage };
}

// Picks the project `projectId` in the line's select, as the user does.
function choose(page, projectId) {
  page.byId("project-select").value = projectId;
  page.byId("project-select").dispatchEvent(new FakeEvent("change"));
}

// The select's options as `[value, label]`.
function options(page) {
  return Array.from(page.byId("project-select").children, (option) => [option.value, option.textContent]);
}

// Lets the promises a click started run to their end.
function settle() {
  return new Promise((resolve) => setImmediate(resolve));
}

test("offersProjectSync needs the sync_project capability", () => {
  assert.equal(offersProjectSync(["sync_project"]), true);
  assert.equal(offersProjectSync(["sync"]), false);
});

test("the line stays hidden until a listing or the host names the project", () => {
  const { byId, project } = mountProject();
  assert.equal(byId("project").hidden, true);

  // An older host names the project only through `get_host`.
  project.setHostName("agentty");
  assert.equal(byId("project").hidden, false);
  assert.equal(byId("project-name").textContent, "agentty");
  assert.equal(byId("project-branch").hidden, true);
  assert.equal(byId("project-counts").hidden, true);
  assert.equal(byId("project-sync").hidden, true);
  assert.equal(byId("project-status").hidden, true);

  project.setHostName(null);
  assert.equal(byId("project").hidden, true);
});

test("a listed project shows its branch, its distance from upstream and the sync button", () => {
  const { byId, project } = mountProject();
  project.setHostName("from-host");

  project.update(listed());
  assert.equal(byId("project-name").textContent, "agentty");
  assert.equal(byId("project-branch").hidden, false);
  assert.equal(byId("project-branch").textContent, "main");
  assert.equal(byId("project-counts").hidden, false);
  assert.equal(byId("project-counts").textContent, "↓2 ↑0");
  assert.equal(byId("project-sync").hidden, false);
  assert.equal(byId("project-sync").disabled, false);
  assert.equal(byId("project-sync-label").textContent, "Sync");

  project.update(listed({ branch: undefined }));
  assert.equal(byId("project-branch").hidden, true);
  assert.equal(byId("project-branch").textContent, "");
  assert.equal(byId("project-counts").hidden, true);

  // The host's name returns when a listing stops naming the project.
  project.update(null);
  assert.equal(byId("project-name").textContent, "from-host");
  assert.equal(byId("project-sync").hidden, true);
});

test("each Agentty's Sync syncs only its own project", async () => {
  // Arrange
  const first = mountProject();
  const second = mountProject({ document: first.document });
  first.project.update(listed());
  second.project.update(listed({ branch: "dev", name: "docs" }));

  // Act
  second.byId("project-sync").click();
  await settle();

  // Assert
  assert.deepEqual(first.requests, []);
  assert.deepEqual(second.requests, [{ method: "sync_project" }]);
});

test("the sync button needs the capability and a connection", () => {
  const plain = mountProject({ capabilities: [] });
  plain.project.update(listed());
  assert.equal(plain.byId("project-sync").hidden, true);

  const { byId, project } = mountProject();
  project.update(listed());
  project.setConnected(false);
  assert.equal(byId("project-sync").hidden, false);
  assert.equal(byId("project-sync").disabled, true);
});

test("Sync sends sync_project at once and waits for the host before another", async () => {
  const { byId, project, requests } = mountProject();
  project.update(listed());

  byId("project-sync").click();
  assert.equal(byId("project-sync").disabled, true);
  assert.equal(byId("project-sync").getAttribute("aria-busy"), "true");
  assert.equal(byId("project-sync-label").textContent, "Syncing…");
  byId("project-sync").click();
  await settle();
  assert.deepEqual(requests, [{ method: "sync_project" }]);
  assert.equal(byId("project-sync").disabled, false);
  assert.equal(byId("project-error").textContent, "");
});

test("a refused sync shows the host's reason until a sync is reported", async () => {
  const { byId, project } = mountProject({
    respond: () => {
      throw new Error("Sync is blocked: a merge is active or queued");
    },
  });
  project.update(listed());

  byId("project-sync").click();
  await settle();
  assert.equal(byId("project-error").textContent, "Sync is blocked: a merge is active or queued");
  assert.equal(byId("project-sync").disabled, false);

  // A listing without a sync keeps the reason; one that reports a sync
  // replaces it.
  project.update(listed());
  assert.equal(byId("project-error").textContent, "Sync is blocked: a merge is active or queued");
  project.update(listed({ sync: { branch: "main", phase: "running" } }));
  assert.equal(byId("project-error").textContent, "");
});

test("the status line follows each phase of the sync in its tone", () => {
  const { byId, project } = mountProject();
  const status = byId("project-status");

  project.update(listed({ sync: { branch: "main", phase: "running" } }));
  assert.equal(status.hidden, false);
  assert.equal(status.textContent, "Syncing agentty/main...");
  assert.equal(status.className, "project-status status--working");
  assert.equal(byId("project-sync").disabled, true);
  assert.equal(byId("project-sync-label").textContent, "Syncing…");

  project.update(listed({ sync: { branch: "main", conflicts: 2, phase: "resolving_conflicts" } }));
  assert.equal(status.textContent, "Resolving 2 conflicts for agentty/main...");
  assert.equal(byId("project-sync").disabled, true);

  project.update(
    listed({ ahead: 0, behind: 0, sync: { branch: "main", phase: "complete", pulled_commits: 2 } }),
  );
  assert.equal(status.textContent, "Synced agentty/main: 2 pulled");
  assert.equal(status.className, "project-status status--ready");
  assert.equal(byId("project-counts").textContent, "✓");
  assert.equal(byId("project-sync").disabled, false);

  project.update(listed({ sync: { branch: "main", message: "push rejected", phase: "failed" } }));
  assert.equal(status.textContent, "Sync failed for agentty/main: push rejected");
  assert.equal(status.className, "project-status status--failed");

  // The host stops showing a finished sync after a while.
  project.update(listed());
  assert.equal(status.hidden, true);
  assert.equal(status.textContent, "");
  assert.equal(status.className, "project-status");
});

test("an unchanged status is not written again", () => {
  const { byId, project } = mountProject();
  const status = byId("project-status");
  project.update(listed({ sync: { branch: "main", phase: "running" } }));
  const text = status.firstChild;

  project.update(listed({ sync: { branch: "main", phase: "running" } }));

  assert.equal(status.firstChild, text);
});

test("offersProjects needs the projects capability", () => {
  assert.equal(offersProjects(["projects"]), true);
  assert.equal(offersProjects(["host"]), false);
});

test("parseProjects keeps the listed projects in order and drops what it cannot use", () => {
  assert.deepEqual(PROJECTS, [
    { activeSessions: 2, id: "1", isActive: true, name: "agentty" },
    { activeSessions: 0, id: "2", isActive: false, name: "website" },
  ]);
  assert.deepEqual(
    parseProjects({
      kind: "projects",
      projects: [{ id: 3, name: "n" }, { id: "", name: "n" }, { id: "4" }, null, { active_sessions: -1, id: "5", name: "e" }],
    }),
    [{ activeSessions: 0, id: "5", isActive: false, name: "e" }],
  );
  assert.deepEqual(parseProjects({ kind: "sessions", projects: [] }), []);
  assert.deepEqual(parseProjects(null), []);
});

test("pickProject keeps a listed pick and otherwise starts on the terminal's project", () => {
  assert.equal(pickProject(PROJECTS, "2"), "2");
  assert.equal(pickProject(PROJECTS, "9"), "1");
  assert.equal(pickProject(PROJECTS, null), "1");
  assert.equal(pickProject([PROJECTS[1]], null), "2");
  assert.equal(pickProject([], "2"), null);
});

test("projectPicks keeps one pick per Agentty and survives unusable storage", () => {
  // Arrange
  const storage = new MemoryStorage();
  const first = projectPicks(storage, "");
  const second = projectPicks(storage, "B");

  // Act
  first.save("2");
  second.save("7");
  const saved = JSON.parse(storage.getItem(PROJECT_PICKS_KEY));
  second.save(null);
  storage.setItem(PROJECT_PICKS_KEY, "[1]");
  const fromArray = first.load();
  storage.setItem(PROJECT_PICKS_KEY, "{");
  const fromBroken = first.load();
  storage.setItem(PROJECT_PICKS_KEY, '{"": 2}');
  const fromNumber = first.load();
  const failing = projectPicks(new MemoryStorage({ failing: true }), "");
  failing.save("2");

  // Assert
  assert.deepEqual(saved, { "": "2", B: "7" });
  assert.equal(second.load(), null);
  assert.equal(fromArray, null);
  assert.equal(fromBroken, null);
  assert.equal(fromNumber, null);
  assert.equal(failing.load(), null);
});

test("the picker shows only with the projects capability and more than one project", () => {
  // Arrange
  const plain = mountProject();
  const picking = mountProject({ capabilities: ["projects", "sync_project"] });

  // Act
  plain.project.setProjects(PROJECTS);
  picking.project.setProjects([PROJECTS[0]]);
  const alone = picking.byId("project-select").hidden;
  picking.project.setProjects(PROJECTS);

  // Assert: without the capability the line follows the terminal.
  assert.equal(plain.byId("project-select").hidden, true);
  assert.equal(plain.project.projectId(), null);
  assert.equal(plain.project.awaitsProjects(), false);
  assert.equal(alone, true);
  assert.equal(picking.byId("project-select").hidden, false);
  assert.equal(picking.byId("project-name").hidden, true);
  assert.equal(picking.byId("project").hidden, false);
  assert.deepEqual(options(picking), [
    ["1", "agentty · terminal"],
    ["2", "website"],
  ]);
  assert.equal(picking.byId("project-select").value, "1");
  // Names only ever become text.
  picking.project.setProjects([...PROJECTS, { activeSessions: 0, id: "3", isActive: false, name: "<b>x</b>" }]);
  assert.equal(options(picking)[2][1], "<b>x</b>");
  assert.equal(picking.byId("project-select").querySelectorAll("b").length, 0);
});

test("the line starts on the terminal's project and keeps to it when the terminal switches", () => {
  // Arrange
  const page = mountProject({ capabilities: ["projects"] });
  const switched = PROJECTS.map((project) => ({ ...project, isActive: !project.isActive }));

  // Act
  const awaited = page.project.awaitsProjects();
  page.project.setProjects(PROJECTS);
  const started = page.project.projectId();
  page.project.setProjects(switched);

  // Assert
  assert.equal(awaited, true);
  assert.equal(page.project.awaitsProjects(), false);
  assert.equal(started, "1");
  assert.equal(page.project.projectId(), "1");
  assert.deepEqual(options(page), [
    ["1", "agentty"],
    ["2", "website · terminal"],
  ]);
  assert.equal(page.byId("project-error").textContent, "");
  // Nothing was picked, so nothing is remembered.
  assert.equal(page.storage.getItem(PROJECT_PICKS_KEY), null);
});

test("a pick is remembered for its Agentty and names the project in a sync", async () => {
  // Arrange
  const storage = new MemoryStorage();
  const page = mountProject({ capabilities: ["projects", "sync_project"], instanceId: "A", storage: storage });
  page.project.setProjects(PROJECTS);
  page.project.update(listed());
  page.byId("project-error").textContent = "Sync is blocked";

  // Act
  choose(page, "2");
  const afterPick = {
    branchHidden: page.byId("project-branch").hidden,
    error: page.byId("project-error").textContent,
    syncHidden: page.byId("project-sync").hidden,
  };
  choose(page, "2");
  choose(page, "9");
  page.project.update(listed({ ahead: undefined, behind: undefined, branch: "site", id: "2", name: "website" }));
  page.byId("project-sync").click();
  await settle();
  const reloaded = mountProject({ capabilities: ["projects"], instanceId: "A", storage: storage });
  reloaded.project.setProjects(PROJECTS);
  const elsewhere = mountProject({ capabilities: ["projects"], instanceId: "B", storage: storage });
  elsewhere.project.setProjects(PROJECTS);

  // Assert: the line shows nothing of the project it left.
  assert.deepEqual(afterPick, { branchHidden: true, error: "", syncHidden: true });
  // Picking the project shown, or one not listed, changes nothing.
  assert.deepEqual(page.picked, ["2"]);
  assert.equal(page.project.projectId(), "2");
  assert.equal(page.byId("project-select").value, "2");
  // A project the host has not compared yet shows its branch without
  // counts.
  assert.equal(page.byId("project-branch").textContent, "site");
  assert.equal(page.byId("project-counts").hidden, true);
  assert.deepEqual(page.requests, [{ method: "sync_project", project_id: "2" }]);
  assert.equal(reloaded.project.projectId(), "2");
  assert.equal(elsewhere.project.projectId(), "1");
  assert.deepEqual(JSON.parse(storage.getItem(PROJECT_PICKS_KEY)), { A: "2" });
});

test("a pick that is no longer listed falls back to the terminal's project and says so", () => {
  // Arrange
  const storage = new MemoryStorage();
  storage.setItem(PROJECT_PICKS_KEY, JSON.stringify({ "": "7" }));
  const stale = mountProject({ capabilities: ["projects"], storage: storage });
  const shown = mountProject({ capabilities: ["projects"] });
  shown.project.setProjects(PROJECTS);
  choose(shown, "2");

  // Act
  stale.project.setProjects(PROJECTS);
  shown.project.setProjects([PROJECTS[0], { ...PROJECTS[1], id: "3" }]);

  // Assert
  assert.equal(stale.project.projectId(), "1");
  assert.equal(stale.byId("project-error").textContent, FALLBACK_NOTE);
  assert.equal(storage.getItem(PROJECT_PICKS_KEY), "{}");
  assert.equal(shown.project.projectId(), "1");
  assert.equal(shown.byId("project-error").textContent, FALLBACK_NOTE);
  assert.equal(shown.byId("project-select").value, "1");
});

test("a host that stops offering projects returns the line to the terminal's project", () => {
  // Arrange
  const page = mountProject({ capabilities: ["projects"] });
  page.project.setProjects(PROJECTS);
  choose(page, "2");

  // Act
  page.project.setCapabilities(["sync_project"]);
  const afterCapabilities = page.project.projectId();
  page.project.setProjects(PROJECTS);

  // Assert
  assert.equal(afterCapabilities, null);
  assert.equal(page.project.projectId(), null);
  assert.equal(page.byId("project-select").hidden, true);
  assert.equal(page.byId("project-select").children.length, 0);
  // Reconnecting with the capability keeps the project picked.
  const kept = mountProject({ capabilities: ["projects"] });
  kept.project.setProjects(PROJECTS);
  choose(kept, "2");
  kept.project.setCapabilities(["projects"]);
  assert.equal(kept.project.projectId(), "2");
  assert.equal(kept.project.awaitsProjects(), false);
});
