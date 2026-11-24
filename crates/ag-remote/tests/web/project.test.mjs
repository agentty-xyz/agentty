import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createDocument } from "./fake_dom.mjs";
import { parseProject } from "../../assets/format.mjs";
import { createInstanceView } from "../../assets/instance.mjs";
import { createProject, offersProjectSync } from "../../assets/project.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
// The parts of an Agentty's view in instance.mjs, by the names these tests
// use.
const PARTS = {
  project: "line",
  "project-branch": "branch",
  "project-counts": "counts",
  "project-error": "error",
  "project-name": "name",
  "project-status": "status",
  "project-sync": "sync",
  "project-sync-label": "syncLabel",
};

// The project a listing names, with `fields` over `agentty` on `main`,
// two commits behind its upstream.
function listed(fields = {}) {
  return parseProject({
    project: { ahead: 0, behind: 2, branch: "main", name: "agentty", ...fields },
  });
}

// Mounts the project line of an Agentty's view on the client's page,
// connected and with `capabilities`. `respond(body)` answers each request;
// the requests sent are recorded. `byId` finds the view's parts by the
// names in `PARTS` and the page's elements by id.
function mountProject({
  capabilities = ["sync_project"],
  document = createDocument(INDEX_HTML),
  respond = () => ({ kind: "accepted" }),
} = {}) {
  const view = createInstanceView({
    document: document,
    prototype: document.getElementById("instance-prototype"),
  });
  document.getElementById("sessions").append(view.root);
  const byId = (id) => (Object.hasOwn(PARTS, id) ? view.project[PARTS[id]] : document.getElementById(id));
  const requests = [];
  const project = createProject({
    elements: view.project,
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
  });
  project.setCapabilities(capabilities);
  project.setConnected(true);

  return { byId, document, project, requests };
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
