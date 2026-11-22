import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createDocument } from "./fake_dom.mjs";
import { parseProject } from "../../assets/format.mjs";
import { createProject, offersProjectSync } from "../../assets/project.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

// The project a listing names, with `fields` over `agentty` on `main`,
// two commits behind its upstream.
function listed(fields = {}) {
  return parseProject({
    project: { ahead: 0, behind: 2, branch: "main", name: "agentty", ...fields },
  });
}

// Mounts the project line on the client's page, connected and with
// `capabilities`. `respond(body)` answers each request; the requests sent
// are recorded.
function mountProject({ capabilities = ["sync_project"], respond = () => ({ kind: "accepted" }) } = {}) {
  const document = createDocument(INDEX_HTML);
  const byId = (id) => document.getElementById(id);
  const requests = [];
  const project = createProject({
    elements: {
      branch: byId("project-branch"),
      counts: byId("project-counts"),
      dialog: byId("project-sync-dialog"),
      dialogBranch: byId("project-sync-branch"),
      dialogTarget: byId("project-sync-target"),
      error: byId("project-error"),
      line: byId("project"),
      name: byId("project-name"),
      status: byId("project-status"),
      sync: byId("project-sync"),
      syncLabel: byId("project-sync-label"),
    },
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
  });
  project.setCapabilities(capabilities);
  project.setConnected(true);

  return { byId, project, requests };
}

// Lets the promises a click started run to their end.
function settle() {
  return new Promise((resolve) => setImmediate(resolve));
}

// Opens the confirmation and answers it with the button of `value`.
function confirmSync(byId, value = "confirm") {
  byId("project-sync").click();
  byId("project-sync-dialog").querySelector('button[value="' + value + '"]').click();
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
  assert.equal(byId("project-sync-target").textContent, "agentty/main");
  assert.equal(byId("project-sync-branch").textContent, "main");

  // A project without a branch names itself in the confirmation.
  project.update(listed({ branch: undefined }));
  assert.equal(byId("project-branch").hidden, true);
  assert.equal(byId("project-branch").textContent, "");
  assert.equal(byId("project-counts").hidden, true);
  assert.equal(byId("project-sync-target").textContent, "agentty");
  assert.equal(byId("project-sync-branch").textContent, "its branch");

  // The host's name returns when a listing stops naming the project.
  project.update(null);
  assert.equal(byId("project-name").textContent, "from-host");
  assert.equal(byId("project-sync").hidden, true);
  assert.equal(byId("project-sync-target").textContent, "the project");
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

test("Sync asks first and sends sync_project only when confirmed", async () => {
  const { byId, project, requests } = mountProject();
  project.update(listed());

  byId("project-sync").click();
  assert.equal(byId("project-sync-dialog").open, true);
  byId("project-sync-dialog").querySelector('button[value="keep"]').click();
  await settle();
  assert.deepEqual(requests, []);

  confirmSync(byId);
  assert.equal(byId("project-sync").disabled, true);
  assert.equal(byId("project-sync").getAttribute("aria-busy"), "true");
  assert.equal(byId("project-sync-label").textContent, "Syncing…");
  await settle();
  assert.deepEqual(requests, [{ method: "sync_project" }]);
  assert.equal(byId("project-sync").disabled, false);
  assert.equal(byId("project-error").textContent, "");
});

test("a confirmation answered after the button went off sends nothing", async () => {
  const { byId, project, requests } = mountProject();
  project.update(listed());

  byId("project-sync").click();
  project.setConnected(false);
  byId("project-sync-dialog").querySelector('button[value="confirm"]').click();
  await settle();

  assert.deepEqual(requests, []);
});

test("a refused sync shows the host's reason until a sync is reported", async () => {
  const { byId, project } = mountProject({
    respond: () => {
      throw new Error("Sync is blocked: a merge is active or queued");
    },
  });
  project.update(listed());

  confirmSync(byId);
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
