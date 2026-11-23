import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createDocument, createWindow } from "./fake_dom.mjs";
import {
  choiceIndex,
  createGraph,
  graphChoices,
  graphGlyph,
  graphTitle,
  nextChoice,
  nodeGlyph,
  outputLabel,
  positionLabel,
  prefersProposal,
  renderGraphSummary,
  summaryGlyph,
} from "../../assets/graph.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

// Element names `createGraph` takes, by the id each has in index.html.
const GRAPH_IDS = {
  detail: "graph-detail",
  error: "graph-error",
  hints: "graph-hints",
  next: "graph-next",
  nodes: "graph-nodes",
  note: "graph-note",
  position: "graph-position",
  prev: "graph-prev",
  preview: "graph-preview",
  refresh: "graph-refresh",
  title: "graph-title",
  view: "graph-view",
};

const NODES = [
  {
    agent: "Inspector",
    agent_kind: "codex",
    attempts: 1,
    awaits_approval: true,
    id: "inspect",
    label: "Awaiting your approval",
    model: "gpt-6.1-sol",
    objective: "Inspect <b>the</b> change",
    reasoning: "high",
    status: "succeeded",
  },
  {
    agent: "Fixer",
    agent_kind: "codex",
    attempts: 0,
    blocked: ["inspect: accepted report", "Paused; resume required"],
    error: "worker lost",
    hint: "reply in chat to answer",
    id: "fix",
    label: "Needs your input",
    model: "gpt-6.1-sol",
    objective: "Fix it",
    questions: [{ options: ["main", "dev"], text: "Which branch?" }],
    reasoning: "medium",
    requires: [
      { gate: "accepted report", label: "needs report accepted", node_id: "inspect" },
      { gate: "successful execution without questions", label: "after setup", node_id: "setup", satisfied: true },
    ],
    status: "awaiting_answers",
  },
];

const OUTPUTS = [
  { id: "r1", name: "report", state: "validated", text: "### Findings\n- One", version: 2 },
  { id: "r2", name: "notes", state: "accepted", text: "Plain notes", version: 1 },
];

// A pending proposal beside two graphs; `graph` answers which is selected.
function response({ graphId = "graph-a", nodeId = "inspect", phase = "awaiting_approval" } = {}) {
  return {
    graph: {
      id: graphId,
      node: nodeId,
      nodes: NODES,
      outputs: nodeId === "inspect" ? OUTPUTS : [],
      revision: 2,
      snapshot: "token",
      status: "blocked",
      status_label: "Blocked",
    },
    graphs: [
      { id: "graph-a", revision: 2 },
      { id: "graph-b", is_focused_review: true, revision: 1 },
    ],
    kind: "graphs",
    proposal: {
      hint: phase === "approved" ? undefined : "reply in chat to approve, revise, or cancel",
      id: "preview",
      phase: phase,
      preview: "## Graph proposal v4\n\n> Inspect the change",
      version: 4,
    },
  };
}

// Mounts the panel on the client's page. `respond(body)` answers each
// request; the requests sent are recorded.
function mountGraph(respond) {
  const window = createWindow(createDocument(INDEX_HTML));
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const requests = [];
  const graph = createGraph({
    document: document,
    elements: Object.fromEntries(Object.entries(GRAPH_IDS).map(([name, id]) => [name, byId(id)])),
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
    window: window,
  });

  return { byId, document, graph, requests };
}

// Lets pending promise callbacks run.
function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function texts(nodes) {
  return Array.from(nodes, (node) => node.textContent);
}

test("glyphs follow the terminal's language, and unknown states read as idle", () => {
  assert.deepEqual(summaryGlyph({ phase: "generating" }), ["◐", "working"]);
  assert.deepEqual(summaryGlyph({ phase: "approved", status: "completed" }), ["✓", "ready"]);
  assert.deepEqual(summaryGlyph({ phase: "from_a_newer_host" }), ["○", "idle"]);
  assert.deepEqual(summaryGlyph(null), ["○", "idle"]);
  assert.deepEqual(nodeGlyph({ awaits_approval: true, status: "succeeded" }), ["●", "asking"]);
  assert.deepEqual(nodeGlyph({ status: "failed" }), ["✗", "failed"]);
  assert.deepEqual(nodeGlyph(null), ["○", "idle"]);
  assert.deepEqual(graphGlyph("paused"), ["‖", "idle"]);
  // `hasOwn` keeps prototype names out.
  assert.deepEqual(graphGlyph("toString"), ["○", "idle"]);
});

test("the selector steps through the proposal, then each graph", () => {
  const shown = response();
  const choices = graphChoices(shown);
  assert.deepEqual(choices, [
    { kind: "proposal" },
    { id: "graph-a", kind: "graph" },
    { id: "graph-b", kind: "graph" },
  ]);
  assert.equal(choiceIndex(choices, { id: "graph-b", kind: "graph" }), 2);
  assert.equal(choiceIndex(choices, { id: "gone", kind: "graph" }), -1);
  assert.equal(positionLabel(shown, { kind: "proposal" }), "Proposal v4");
  assert.equal(positionLabel(shown, { id: "graph-b", kind: "graph" }), "Graph 2/2");
  assert.equal(positionLabel({}, { id: "gone", kind: "graph" }), "");
  assert.deepEqual(graphChoices({}), []);
  assert.equal(prefersProposal(shown), true);
  assert.equal(prefersProposal(response({ phase: "approved" })), false);
  assert.equal(prefersProposal({}), false);
  assert.equal(graphTitle(shown.graph), "graph-a | revision 2 | Blocked");
  assert.equal(outputLabel(OUTPUTS, 1), "Output 2/2: notes v1 (accepted)");
});

test("the summary shows the version, the count, the phase and each output awaiting approval", () => {
  const { byId, document } = mountGraph(() => ({}));
  const bar = byId("graph-summary");
  const summary = {
    awaiting: [{ node_id: "inspect", text: "inspect awaits your approval; reply in chat to accept it" }],
    done: 0,
    label: "Generating preview...",
    phase: "generating",
    total: 0,
    version: 1,
  };

  assert.equal(renderGraphSummary(document, bar, summary), true);
  assert.equal(bar.hidden, false);
  assert.deepEqual(texts(bar.children), [
    "Execution graph · v1",
    "◐Generating preview...",
    "inspect awaits your approval; reply in chat to accept it",
  ]);
  // No count before the proposal resolves, and an unchanged summary keeps
  // its nodes.
  assert.equal(bar.querySelector(".graph-summary-count"), null);
  assert.equal(bar.querySelector(".graph-summary-status").className, "graph-summary-status tone--working");
  assert.equal(renderGraphSummary(document, bar, { ...summary }), false);
  assert.equal(renderGraphSummary(document, bar, null), true);
  assert.equal(bar.hidden, true);
  assert.equal(bar.children.length, 0);
  assert.equal(renderGraphSummary(document, bar, null), false);
});

test("the panel opens on a pending proposal and steps to its graphs", async () => {
  const page = mountGraph((body) => response({ graphId: body.graph_id ?? "graph-a" }));
  await page.graph.open("s1");

  assert.equal(page.graph.isOpen(), true);
  assert.equal(page.byId("graph-view").hidden, false);
  assert.deepEqual(page.requests, [{ method: "get_session_graphs", session_id: "s1" }]);
  assert.equal(page.byId("graph-position").textContent, "Proposal v4");
  assert.equal(page.byId("graph-prev").disabled, true);
  assert.equal(page.byId("graph-next").disabled, false);
  assert.equal(page.byId("graph-title").textContent, "Proposal preview v4");
  assert.deepEqual(texts(page.byId("graph-hints").children), [
    "reply in chat to approve, revise, or cancel",
  ]);
  const preview = page.byId("graph-preview");
  assert.equal(preview.hidden, false);
  assert.equal(preview.querySelector(".md h2").textContent, "Graph proposal v4");
  assert.equal(page.byId("graph-nodes").children.length, 0);
  assert.equal(page.byId("graph-detail").hidden, true);

  page.byId("graph-next").click();
  await settle();
  assert.deepEqual(page.requests[1], {
    graph_id: "graph-a",
    method: "get_session_graphs",
    session_id: "s1",
  });
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");
  assert.equal(page.byId("graph-title").textContent, "●graph-a | revision 2 | Blocked");
  assert.equal(preview.hidden, true);
  page.byId("graph-next").click();
  await settle();
  assert.equal(page.requests[2].graph_id, "graph-b");
  assert.equal(page.byId("graph-next").disabled, true);
  // Back to the proposal needs no request.
  page.byId("graph-prev").click();
  await settle();
  page.byId("graph-prev").click();
  assert.equal(page.requests.length, 4);
  assert.equal(page.byId("graph-position").textContent, "Proposal v4");
  assert.equal(preview.hidden, false);
  page.byId("graph-prev").click();
  assert.equal(page.requests.length, 4);
});

test("an approved proposal opens on its graph with the nodes, details and outputs", async () => {
  const page = mountGraph((body) =>
    response({ nodeId: body.node_id ?? "inspect", phase: "approved" }),
  );
  await page.graph.open("s1");

  const rows = page.byId("graph-nodes").children;
  assert.deepEqual(
    Array.from(rows, (row) => row.querySelector(".graph-node-head").textContent),
    ["●inspectAwaiting your approval", "?fixNeeds your input"],
  );
  assert.equal(rows[0].classList.contains("is-selected"), true);
  assert.equal(rows[0].querySelector(".graph-node-model").textContent, "gpt-6.1-sol [high]");
  // Untrusted text stays text.
  assert.equal(rows[0].querySelector(".graph-node-objective").textContent, "Inspect <b>the</b> change");
  assert.equal(rows[0].querySelector(".graph-node-objective").children.length, 0);
  assert.deepEqual(
    Array.from(rows[1].querySelectorAll(".graph-require"), (row) => [row.className, row.textContent]),
    [
      ["graph-require is-pending", "needs report accepted"],
      ["graph-require is-satisfied", "after setup"],
    ],
  );
  const detail = page.byId("graph-detail");
  assert.equal(detail.hidden, false);
  assert.deepEqual(texts(detail.querySelectorAll(".graph-field")), [
    "Agent: Inspector (codex, gpt-6.1-sol)",
    "Objective: Inspect <b>the</b> change",
    "Status: Awaiting your approval | Attempts: 1",
  ]);
  assert.equal(detail.querySelector(".graph-output-label").textContent, "Output 1/2: report v2 (validated)");
  assert.equal(detail.querySelector(".graph-output .md h3").textContent, "Findings");

  // The reader's source view survives a refresh of the same output.
  detail.querySelector(".source-toggle").click();
  const shown = detail.querySelector(".graph-output");
  await page.graph.refresh();
  assert.equal(detail.querySelector(".graph-output"), shown);
  assert.equal(shown.querySelector(".md-source").hidden, false);
  assert.equal(page.byId("graph-nodes").children[0], rows[0]);

  // The output selector steps without a request.
  const next = detail.querySelectorAll(".graph-step")[1];
  next.click();
  assert.equal(page.requests.length, 2);
  assert.equal(detail.querySelector(".graph-output-label").textContent, "Output 2/2: notes v1 (accepted)");
  assert.equal(detail.querySelector(".graph-output .md").textContent, "Plain notes");
  detail.querySelectorAll(".graph-step")[0].click();
  assert.equal(detail.querySelector(".graph-output-label").textContent, "Output 1/2: report v2 (validated)");

  // Choosing another node reads it and shows its details and hint.
  rows[1].querySelector(".graph-node-head").click();
  await settle();
  assert.deepEqual(page.requests[2], {
    graph_id: "graph-a",
    method: "get_session_graphs",
    node_id: "fix",
    session_id: "s1",
  });
  assert.deepEqual(texts(detail.querySelectorAll(".graph-field")), [
    "Agent: Fixer (codex, gpt-6.1-sol)",
    "Objective: Fix it",
    "Requires inspect: accepted report",
    "Requires setup: successful execution without questions",
    "Status: Needs your input | Attempts: 0",
    "Blocked: inspect: accepted report",
    "Blocked: Paused; resume required",
    "Error: worker lost",
    "Question: Which branch? (main / dev)",
  ]);
  assert.equal(detail.querySelector(".graph-output-bar"), null);
  assert.deepEqual(texts(page.byId("graph-hints").children), ["reply in chat to answer"]);
  // Choosing the selected node again reads nothing.
  page.byId("graph-nodes").children[1].querySelector(".graph-node-head").click();
  assert.equal(page.requests.length, 3);
});

test("each part Agentty cut says so beside it, and an output without room says it is left out", async () => {
  const shown = response({ phase: "approved" });
  shown.proposal.truncated = true;
  shown.graph.nodes = [{ ...NODES[0], truncated: true }, NODES[1]];
  shown.graph.outputs = [
    { ...OUTPUTS[0], truncated: true },
    { ...OUTPUTS[1], text: "", truncated: true },
  ];
  const page = mountGraph(() => shown);
  await page.graph.open("s1");

  const detail = page.byId("graph-detail");
  assert.equal(
    detail.querySelector(".graph-detail-info .graph-cut").textContent,
    "Part of this node's text is too large to show here.",
  );
  assert.equal(
    detail.querySelector(".graph-output .graph-cut").textContent,
    "The rest of this output is too large to show here.",
  );
  assert.equal(detail.querySelector(".graph-output .md h3").textContent, "Findings");
  detail.querySelectorAll(".graph-step")[1].click();
  const omitted = detail.querySelector(".graph-output");
  assert.equal(omitted.querySelector(".md"), null);
  assert.equal(omitted.textContent, "This output is too large to show here.");
  // The proposal says its preview was cut once it shows.
  page.byId("graph-prev").click();
  page.byId("graph-prev").click();
  assert.equal(
    page.byId("graph-preview").querySelector(".graph-cut").textContent,
    "The rest of this proposal is too large to show here.",
  );
  assert.deepEqual(texts(page.byId("graph-hints").children), []);
});

test("a panel left on the proposal moves to its graph once the proposal is approved", async () => {
  let phase = "awaiting_approval";
  const page = mountGraph((body) => response({ graphId: body.graph_id ?? "graph-b", phase }));
  await page.graph.open("s1");
  assert.equal(page.byId("graph-position").textContent, "Proposal v4");

  phase = "approved";
  await page.graph.refresh();

  // Without a graph named, Agentty selects the approved one.
  assert.equal(page.requests[1].graph_id, undefined);
  assert.equal(page.byId("graph-position").textContent, "Graph 2/2");
  assert.equal(page.byId("graph-preview").hidden, true);
  assert.equal(page.byId("graph-nodes").children.length, 2);
  assert.deepEqual(nextChoice(null, null, {}), { kind: "proposal" });
  assert.deepEqual(nextChoice({ id: "gone", kind: "graph" }, null, {}), { id: "gone", kind: "graph" });
});

test("a proposal the reader steps to after its approval stays shown until another replaces it", async () => {
  let answer = response({ phase: "approved" });
  const page = mountGraph(() => answer);
  await page.graph.open("s1");
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");
  page.byId("graph-prev").click();
  assert.equal(page.byId("graph-position").textContent, "Proposal v4");

  await page.graph.refresh();

  assert.equal(page.byId("graph-position").textContent, "Proposal v4");
  assert.equal(page.byId("graph-preview").hidden, false);
  assert.equal(page.byId("graph-nodes").children.length, 0);
  // A newer version already settled when read moves on to the graph, and
  // so does a proposal that is gone.
  const approved = response({ phase: "approved" });
  const newer = { ...approved, proposal: { ...approved.proposal, version: 5 } };
  assert.deepEqual(nextChoice({ kind: "proposal" }, approved, newer), { id: "graph-a", kind: "graph" });
  assert.deepEqual(nextChoice({ kind: "proposal" }, approved, { ...approved, proposal: undefined }), {
    id: "graph-a",
    kind: "graph",
  });
  answer = newer;
  await page.graph.refresh();
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");
});

// Answers each read of `graphs` as Agentty does: `unchanged` when the
// request names the version it would send, else the whole response.
function versioned(graphs) {
  return (body) => {
    const shown = graphs(body);

    return body.known === shown.version ? { kind: "graphs", unchanged: true, version: shown.version } : shown;
  };
}

test("a read of the same selection names the version shown and keeps the page when unchanged", async () => {
  let revision = 2;
  const page = mountGraph(
    versioned((body) => {
      const shown = response({ graphId: body.graph_id ?? "graph-a", nodeId: body.node_id ?? "inspect", phase: "approved" });
      shown.graph.revision = revision;

      return { ...shown, version: [shown.graph.id, shown.graph.node, revision].join(":") };
    }),
  );
  await page.graph.open("s1");
  assert.equal(page.requests[0].known, undefined);
  const rows = [...page.byId("graph-nodes").children];
  const info = page.byId("graph-detail").querySelector(".graph-detail-info");
  const output = page.byId("graph-detail").querySelector(".graph-output");

  // The next read names what shows, and an unchanged answer keeps it.
  await page.graph.refresh();
  assert.deepEqual(page.requests[1], {
    graph_id: "graph-a",
    known: "graph-a:inspect:2",
    method: "get_session_graphs",
    node_id: "inspect",
    session_id: "s1",
  });
  assert.deepEqual([...page.byId("graph-nodes").children], rows);
  assert.equal(page.byId("graph-detail").querySelector(".graph-detail-info"), info);
  assert.equal(page.byId("graph-detail").querySelector(".graph-output"), output);
  assert.equal(page.byId("graph-title").textContent, "●graph-a | revision 2 | Blocked");
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");

  // Changed content comes whole, with its new version for the next read.
  revision = 3;
  page.byId("graph-refresh").click();
  await settle();
  assert.equal(page.requests[2].known, "graph-a:inspect:2");
  assert.equal(page.byId("graph-title").textContent, "●graph-a | revision 3 | Blocked");
  await page.graph.refresh();
  assert.equal(page.requests[3].known, "graph-a:inspect:3");

  // Another node or another graph never names the version of the last one.
  page.byId("graph-nodes").children[1].querySelector(".graph-node-head").click();
  await settle();
  assert.equal(page.requests[4].node_id, "fix");
  assert.equal(page.requests[4].known, undefined);
  assert.equal(page.byId("graph-detail").querySelector(".graph-field").textContent, "Agent: Fixer (codex, gpt-6.1-sol)");
  page.byId("graph-next").click();
  await settle();
  assert.equal(page.requests[5].graph_id, "graph-b");
  assert.equal(page.requests[5].known, undefined);
  await page.graph.refresh();
  assert.equal(page.requests[6].known, "graph-b:inspect:3");

  // Nor does a panel opened again.
  page.graph.close();
  await page.graph.open("s1");
  assert.equal(page.requests[7].known, undefined);
});

test("an answer without a version, from an older Agentty, is never named back", async () => {
  const page = mountGraph(() => response({ phase: "approved" }));
  await page.graph.open("s1");
  await page.graph.refresh();
  await page.graph.refresh();

  assert.deepEqual(
    page.requests.map((body) => body.known),
    [undefined, undefined, undefined],
  );
});

test("the panel shows a note, errors and an empty session, and closes", async () => {
  let answer = response({ phase: "approved" });
  answer.graph = { ...answer.graph, note: "Focused review: driven by Agentty." };
  const page = mountGraph(() => {
    if (answer instanceof Error) {
      throw answer;
    }

    return answer;
  });
  await page.graph.open("s1");
  assert.equal(page.byId("graph-note").hidden, false);
  assert.equal(page.byId("graph-note").textContent, "Focused review: driven by Agentty.");
  assert.equal(page.byId("graph-detail").querySelector(".graph-cut"), null);

  answer = new Error("Graph does not belong to this session");
  page.byId("graph-refresh").click();
  await settle();
  assert.equal(page.byId("graph-error").textContent, "Graph does not belong to this session");
  // What was shown stays.
  assert.equal(page.byId("graph-nodes").children.length, 2);

  answer = { kind: "graphs" };
  await page.graph.refresh();
  assert.equal(page.byId("graph-error").textContent, "");
  assert.equal(page.byId("graph-title").textContent, "Execution graphs: none saved for this session");
  assert.equal(page.byId("graph-position").textContent, "");
  assert.equal(page.byId("graph-next").disabled, true);
  assert.equal(page.byId("graph-nodes").children.length, 0);

  // A proposal that goes away leaves the panel on the graphs.
  answer = response();
  await page.graph.refresh();
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");
  page.byId("graph-prev").click();
  assert.equal(page.byId("graph-position").textContent, "Proposal v4");
  answer = { ...response(), proposal: undefined };
  await page.graph.refresh();
  assert.equal(page.byId("graph-position").textContent, "Graph 1/2");

  page.graph.close();
  assert.equal(page.graph.isOpen(), false);
  assert.equal(page.byId("graph-view").hidden, true);
  assert.equal(page.byId("graph-nodes").children.length, 0);
  await page.graph.refresh();
  page.byId("graph-refresh").click();
  assert.equal(page.byId("graph-nodes").children.length, 0);
});

test("an answer for an earlier open is dropped once the panel moved on", async () => {
  let release;
  const held = new Promise((resolve) => {
    release = resolve;
  });
  const page = mountGraph((body) => (body.session_id === "s1" ? held : response()));
  const first = page.graph.open("s1");
  await page.graph.open("s2");
  release(response({ graphId: "graph-b", phase: "approved" }));
  await first;

  assert.equal(page.byId("graph-position").textContent, "Proposal v4");

  // A failure for an earlier open is dropped too.
  const failing = mountGraph((body) => {
    if (body.session_id === "s1") {
      return held.then(() => {
        throw new Error("late");
      });
    }

    return response();
  });
  const late = failing.graph.open("s1");
  await failing.graph.open("s2");
  await late;
  assert.equal(failing.byId("graph-error").textContent, "");
});
