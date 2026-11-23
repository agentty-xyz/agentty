// Execution graphs of the open session, read only, as the terminal shows
// them: a summary bar beside the chat and a panel with the latest graph
// proposal and each graph's nodes, their details and their outputs, like
// the terminal's `w` inspector. Acting on a graph still happens by
// replying in chat. Agentty sends every label; ids, objectives, errors and
// outputs are untrusted and only ever become text nodes, directly or
// through markdown.mjs. Functions take `document` as a parameter, so the
// Node tests in crates/ag-remote/tests/web/graph.test.mjs pass the fake
// DOM.

import { keepSourceView, renderAnswer } from "./markdown.mjs";

// Glyph and tone of each graph status, in the terminal's glyph language.
const GRAPH_STATUSES = {
  blocked: ["●", "asking"],
  cancelled: ["⊘", "idle"],
  completed: ["✓", "ready"],
  failed: ["✗", "failed"],
  paused: ["‖", "idle"],
  running: ["◐", "working"],
};
// Glyph and tone of a proposal phase, shown until its graph has a status.
const PROPOSAL_PHASES = {
  approved: ["◐", "working"],
  awaiting_approval: ["●", "asking"],
  cancelled: ["⊘", "idle"],
  failed: ["✗", "failed"],
  generating: ["◐", "working"],
  needs_input: ["?", "asking"],
  unresolved: ["○", "idle"],
};
// Glyph and tone of each node status.
const NODE_STATUSES = {
  awaiting_answers: ["?", "asking"],
  cancelled: ["⊘", "idle"],
  failed: ["✗", "failed"],
  pending: ["○", "idle"],
  queued: ["◐", "working"],
  running: ["◐", "working"],
  succeeded: ["✓", "ready"],
};
// A node whose output waits for the user's approval.
const AWAITING_APPROVAL = ["●", "asking"];
// Anything a newer host sends that this page does not know.
const UNKNOWN = ["○", "idle"];
// Say where Agentty left something out to keep a response small.
const PREVIEW_CUT_NOTE = "The rest of this proposal is too large to show here.";
const OUTPUT_CUT_NOTE = "The rest of this output is too large to show here.";
const OUTPUT_OMITTED_NOTE = "This output is too large to show here.";
const NODE_CUT_NOTE = "Part of this node's text is too large to show here.";

function glyphOf(table, key) {
  return Object.hasOwn(table, key) ? table[key] : UNKNOWN;
}

// The glyph and tone of a session's graph `summary`: its graph's status
// once known, and the proposal's phase before.
export function summaryGlyph(summary) {
  return summary?.status
    ? glyphOf(GRAPH_STATUSES, summary.status)
    : glyphOf(PROPOSAL_PHASES, summary?.phase);
}

// The glyph and tone of graph `node`.
export function nodeGlyph(node) {
  return node?.awaits_approval ? AWAITING_APPROVAL : glyphOf(NODE_STATUSES, node?.status);
}

// The glyph and tone of graph `status`.
export function graphGlyph(status) {
  return glyphOf(GRAPH_STATUSES, status);
}

// Whether the panel first shows the proposal: while it is not approved,
// as the terminal's inspector does.
export function prefersProposal(response) {
  return Boolean(response?.proposal) && response.proposal.phase !== "approved";
}

// The view to show after reading `response`, given the reader's `choice`
// (`null` before the first read) and the `previous` response it showed. A
// chosen graph stays chosen. The first read shows the proposal while it is
// not approved. A proposal shown while pending moves to its graph once
// approved, or gone, as the terminal's inspector follows the approval; one
// the reader chose already approved stays until it is gone.
export function nextChoice(choice, previous, response) {
  const graph = response?.graph ? { id: String(response.graph.id), kind: "graph" } : null;
  if (choice && choice.kind !== "proposal") {
    return graph ?? choice;
  }
  const keepsProposal = choice ? !followsApproval(previous, response) : prefersProposal(response);

  return keepsProposal || !graph ? { kind: "proposal" } : graph;
}

// Whether a panel on the proposal moves on after reading `response`: the
// proposal `previous` showed pending is no longer pending, or it is gone,
// or another settled one replaced it. A settled proposal the reader
// stepped to stays.
function followsApproval(previous, response) {
  if (prefersProposal(response)) {
    return false;
  }
  const before = previous?.proposal;
  const now = response?.proposal;

  return prefersProposal(previous) || !now || before?.id !== now.id || before?.version !== now.version;
}

// The views the panel steps through, in the terminal's order: the latest
// proposal, then each graph.
export function graphChoices(response) {
  const choices = response?.proposal ? [{ kind: "proposal" }] : [];
  const graphs = Array.isArray(response?.graphs) ? response.graphs : [];

  return choices.concat(graphs.map((entry) => ({ id: String(entry.id), kind: "graph" })));
}

// Where `choice` stands among `choices`, or -1.
export function choiceIndex(choices, choice) {
  return choices.findIndex(
    (candidate) =>
      candidate.kind === choice.kind && (choice.kind === "proposal" || candidate.id === choice.id),
  );
}

// The position the panel shows: `Proposal v2` or `Graph 1/3`.
export function positionLabel(response, choice) {
  if (choice.kind === "proposal") {
    return "Proposal v" + response.proposal.version;
  }
  const graphs = Array.isArray(response?.graphs) ? response.graphs : [];
  const index = graphs.findIndex((entry) => String(entry.id) === choice.id);

  return index < 0 ? "" : "Graph " + (index + 1) + "/" + graphs.length;
}

// The title the terminal's inspector shows for `graph`.
export function graphTitle(graph) {
  return graph.id + " | revision " + graph.revision + " | " + graph.status_label;
}

// How the selector names the output at `index` of `outputs`.
export function outputLabel(outputs, index) {
  const output = outputs[index];

  return (
    "Output " + (index + 1) + "/" + outputs.length + ": " + output.name + " v" + output.version +
    " (" + output.state + ")"
  );
}

// Names the session, graph and node a `get_session_graphs` request `body`
// reads, so a later read of the same selection can name the version held.
function selectionKey(body) {
  return JSON.stringify([body.session_id, body.graph_id ?? null, body.node_id ?? null]);
}

// A node's `question` with its suggested answers, as the terminal's
// inspector shows it.
function questionText(question) {
  const options = Array.isArray(question.options) ? question.options : [];

  return String(question.text ?? "") + (options.length > 0 ? " (" + options.join(" / ") + ")" : "");
}

// Shows a session's graph `summary` in `node`, the bar beside the chat:
// `Execution graph · v2`, `1/3 done`, the phase, and each output that waits
// for approval. Hides it without a summary. Returns whether it changed.
export function renderGraphSummary(document, node, summary) {
  const key = summary ? JSON.stringify(summary) : "";
  if ((node.dataset.summary ?? "") === key) {
    return false;
  }
  node.dataset.summary = key;
  node.hidden = !summary;
  if (!summary) {
    node.replaceChildren();

    return true;
  }
  const head = element(document, "span", "graph-summary-head");
  head.append(element(document, "span", "graph-summary-title", "Execution graph · v" + summary.version));
  if (Number(summary.total) > 0) {
    head.append(
      element(document, "span", "graph-summary-count", summary.done + "/" + summary.total + " done"),
    );
  }
  const awaiting = Array.isArray(summary.awaiting) ? summary.awaiting : [];
  node.replaceChildren(
    head,
    statusRow(document, summaryGlyph(summary), String(summary.label ?? "")),
    ...awaiting.map((approval) =>
      element(document, "span", "graph-summary-awaiting", String(approval.text ?? "")),
    ),
  );

  return true;
}

// Wires the panel. `elements` holds `view` (the section that replaces the
// transcript), `prev`, `next` and `position` (the selector), `refresh`,
// `title`, `error`, `hints`, `note`, `preview`, `nodes` (the node list)
// and `detail` (the selected node). `request(body)` sends one request and
// resolves with its result; `window` supplies the clipboard for code
// blocks.
//
// Returns `open(sessionId)`, `refresh()`, which reads the graphs again
// keeping the selection, `close()`, and `isOpen()`. A refresh changes only
// what changed, so the reader keeps their place and the source view of an
// output.
export function createGraph({ document, elements, request, window = null }) {
  const state = {
    choice: null,
    // The selection the shown response answers and its `version`.
    held: null,
    loads: 0,
    nodeId: null,
    outputId: null,
    response: null,
    sessionId: null,
  };

  // Reads the graphs for the current selection. A read of the selection
  // that shows names the version it shows, and Agentty answers `unchanged`
  // when nothing differs, so what shows stays as it is.
  async function load() {
    const body = requestBody(state.sessionId);
    if (state.held?.selection === selectionKey(body)) {
      body.known = state.held.version;
    }
    const load = ++state.loads;
    let result;
    try {
      result = await request(body);
    } catch (error) {
      if (load === state.loads) {
        elements.error.textContent = error.message;
      }

      return;
    }
    if (load !== state.loads) {
      return;
    }
    elements.error.textContent = "";
    if (result?.unchanged && body.known) {
      return;
    }
    apply(result ?? {});
    // The read that shows the same again: the graph and node the response
    // settled on.
    state.held = result?.version
      ? { selection: selectionKey(requestBody(state.sessionId)), version: String(result.version) }
      : null;
  }

  function requestBody(sessionId) {
    const body = { method: "get_session_graphs", session_id: sessionId };
    if (state.choice?.kind === "graph") {
      body.graph_id = state.choice.id;
    }
    if (state.nodeId) {
      body.node_id = state.nodeId;
    }

    return body;
  }

  function apply(result) {
    state.choice = nextChoice(state.choice, state.response, result);
    state.response = result;
    state.nodeId = result.graph?.node ?? null;
    render();
  }

  function render() {
    const view = shownView();
    renderSelector();
    renderTitle(view);
    renderHints(view);
    renderPreview(view.proposal);
    elements.note.textContent = view.graph?.note ?? "";
    elements.note.hidden = !view.graph?.note;
    renderNodes(view.graph);
    renderDetail(view.graph);
  }

  // What the current choice shows: the proposal, or the graph.
  function shownView() {
    const response = state.response;
    if (state.choice.kind === "proposal" && response.proposal) {
      return { graph: null, proposal: response.proposal };
    }

    return { graph: response.graph ?? null, proposal: null };
  }

  // The terminal inspector's title, after the graph's status glyph.
  function renderTitle(view) {
    if (view.proposal) {
      elements.title.replaceChildren(
        "Proposal " + view.proposal.id + " v" + view.proposal.version,
      );
    } else if (view.graph) {
      const [glyph, tone] = graphGlyph(view.graph.status);
      elements.title.replaceChildren(
        element(document, "span", "graph-glyph tone--" + tone, glyph),
        graphTitle(view.graph),
      );
    } else {
      elements.title.replaceChildren("Execution graphs: none saved for this session");
    }
  }

  function renderSelector() {
    const choices = graphChoices(state.response);
    const index = choiceIndex(choices, state.choice);
    elements.position.textContent = index < 0 ? "" : positionLabel(state.response, state.choice);
    elements.prev.disabled = index <= 0;
    elements.next.disabled = index < 0 || index >= choices.length - 1;
  }

  // The terminal's hint lines: how to act on the pending proposal or on the
  // selected node, by replying in chat.
  function renderHints(view) {
    const node = view.graph?.nodes?.find((candidate) => candidate.id === view.graph.node);
    const hints = [view.proposal?.hint ?? node?.hint].filter(Boolean);
    elements.hints.replaceChildren(
      ...hints.map((hint) => element(document, "p", "graph-hint", hint)),
    );
  }

  // Shows the proposal preview as an answer, saying when Agentty cut it.
  // The same preview keeps its node, and with it the reader's choice of
  // source or rendered view.
  function renderPreview(proposal) {
    const node = elements.preview;
    const preview = proposal ? String(proposal.preview ?? "") : "";
    const key = JSON.stringify([preview, Boolean(proposal?.truncated)]);
    node.hidden = !proposal;
    if (node.dataset.preview === key) {
      return;
    }
    node.dataset.preview = key;
    const body = element(document, "div", "graph-preview-body");
    renderAnswer(body, preview, document, { window });
    keepSourceView(node.firstElementChild, body);
    node.replaceChildren(body);
    if (proposal?.truncated) {
      node.append(element(document, "p", "graph-cut", PREVIEW_CUT_NOTE));
    }
  }

  // Keeps the row of each node that is still listed, rebuilds it only when
  // it changed, and moves it only when the order did.
  function renderNodes(graph) {
    const nodes = Array.isArray(graph?.nodes) ? graph.nodes : [];
    const rows = new Map([...elements.nodes.children].map((row) => [row.dataset.id, row]));
    const wanted = nodes.map((node) => {
      const id = String(node.id);
      const key = JSON.stringify([node, id === graph.node]);
      const row = rows.get(id);
      rows.delete(id);
      if (row?.dataset.key === key) {
        return row;
      }
      const fresh = renderNode(node, key, id === graph.node);
      row?.replaceWith(fresh);

      return fresh;
    });
    for (const stale of rows.values()) {
      stale.remove();
    }
    wanted.forEach((row, index) => {
      const current = elements.nodes.children[index];
      if (current !== row) {
        elements.nodes.insertBefore(row, current ?? null);
      }
    });
  }

  function renderNode(node, key, isSelected) {
    const id = String(node.id);
    const item = element(document, "li", "graph-node" + (isSelected ? " is-selected" : ""));
    item.dataset.id = id;
    item.dataset.key = key;
    const [glyph, tone] = nodeGlyph(node);
    const head = element(document, "button", "graph-node-head");
    head.type = "button";
    head.setAttribute("aria-pressed", String(isSelected));
    head.append(
      element(document, "span", "graph-glyph tone--" + tone, glyph),
      element(document, "span", "graph-node-id", id),
      element(document, "span", "graph-node-label tone--" + tone, String(node.label ?? "")),
    );
    head.addEventListener("click", () => selectNode(id));
    item.append(
      head,
      element(document, "p", "graph-node-model", node.model + " [" + node.reasoning + "]"),
      element(document, "p", "graph-node-objective", String(node.objective ?? "")),
    );
    const requires = Array.isArray(node.requires) ? node.requires : [];
    if (requires.length > 0) {
      const list = element(document, "ul", "graph-requires");
      list.append(...requires.map((requirement) => requirementRow(requirement)));
      item.append(list);
    }

    return item;
  }

  function requirementRow(requirement) {
    const tone = requirement.satisfied ? "is-satisfied" : "is-pending";

    return element(document, "li", "graph-require " + tone, String(requirement.label ?? ""));
  }

  function selectNode(nodeId) {
    if (nodeId === state.nodeId) {
      return;
    }
    state.nodeId = nodeId;
    state.outputId = null;
    void load();
  }

  // The selected node's details, then its outputs. Each part is built
  // again only when it changed.
  function renderDetail(graph) {
    const node = graph?.nodes?.find((candidate) => candidate.id === graph.node);
    elements.detail.hidden = !node;
    if (!node) {
      elements.detail.replaceChildren();

      return;
    }
    const infoKey = JSON.stringify(node);
    let info = elements.detail.querySelector(".graph-detail-info");
    if (info?.dataset.key !== infoKey) {
      info = renderInfo(node);
      info.dataset.key = infoKey;
    }
    const outputs = renderOutputs(Array.isArray(graph.outputs) ? graph.outputs : []);
    elements.detail.replaceChildren(info, ...outputs);
  }

  function renderInfo(node) {
    const info = element(document, "div", "graph-detail-info");
    info.append(
      element(document, "h3", "graph-detail-id", String(node.id)),
      field("Agent", node.agent + " (" + node.agent_kind + ", " + node.model + ")"),
      field("Objective", String(node.objective ?? "")),
      ...(node.requires ?? []).map((requirement) =>
        field("Requires " + requirement.node_id, String(requirement.gate ?? "")),
      ),
      field("Status", node.label + " | Attempts: " + node.attempts),
      ...(node.blocked ?? []).map((reason) => field("Blocked", String(reason))),
    );
    if (node.error) {
      info.append(field("Error", String(node.error), "is-error"));
    }
    for (const question of node.questions ?? []) {
      info.append(field("Question", questionText(question)));
    }
    if (node.truncated) {
      info.append(element(document, "p", "graph-cut", NODE_CUT_NOTE));
    }

    return info;
  }

  function field(label, value, className = null) {
    const row = element(document, "p", className ? "graph-field " + className : "graph-field");
    row.append(element(document, "span", "graph-field-label", label + ": "), value);

    return row;
  }

  // The output selector and the selected output as an answer. The same
  // output keeps its node, and with it the reader's source view.
  function renderOutputs(outputs) {
    if (outputs.length === 0) {
      return [];
    }
    const found = outputs.findIndex((output) => output.id === state.outputId);
    const index = Math.max(found, 0);
    state.outputId = outputs[index].id;
    const bar = element(document, "div", "graph-output-bar");
    bar.append(
      stepButton("‹", "Previous output", index > 0, () => selectOutput(outputs, index - 1)),
      element(document, "span", "graph-output-label", outputLabel(outputs, index)),
      stepButton("›", "Next output", index < outputs.length - 1, () =>
        selectOutput(outputs, index + 1),
      ),
    );

    return [bar, outputBody(outputs[index])];
  }

  // The output as an answer, and what Agentty cut of it. An output that
  // got no room in the response says so instead of showing as empty.
  function outputBody(output) {
    const text = String(output.text ?? "");
    const key = JSON.stringify([output.id, text, Boolean(output.truncated)]);
    const shown = elements.detail.querySelector(".graph-output");
    if (shown?.dataset.key === key) {
      return shown;
    }
    const body = element(document, "div", "graph-output msg--assistant");
    body.dataset.key = key;
    if (text || !output.truncated) {
      renderAnswer(body, text, document, { window });
      keepSourceView(shown, body);
    }
    if (output.truncated) {
      body.append(
        element(document, "p", "graph-cut", text ? OUTPUT_CUT_NOTE : OUTPUT_OMITTED_NOTE),
      );
    }

    return body;
  }

  function selectOutput(outputs, index) {
    state.outputId = outputs[index].id;
    renderDetail(state.response.graph);
  }

  function stepButton(glyph, label, isEnabled, onClick) {
    const button = element(document, "button", "icon-btn graph-step", glyph);
    button.type = "button";
    button.setAttribute("aria-label", label);
    button.disabled = !isEnabled;
    button.addEventListener("click", onClick);

    return button;
  }

  // Steps `delta` views through the proposal and the graphs.
  function move(delta) {
    const choices = graphChoices(state.response);
    const next = choices[choiceIndex(choices, state.choice) + delta];
    if (!next) {
      return;
    }
    state.choice = next;
    state.nodeId = null;
    state.outputId = null;
    if (next.kind === "proposal") {
      render();
    } else {
      void load();
    }
  }

  function reset() {
    state.choice = null;
    state.held = null;
    state.loads += 1;
    state.nodeId = null;
    state.outputId = null;
    state.response = null;
    for (const node of [elements.hints, elements.nodes, elements.detail, elements.preview]) {
      node.replaceChildren();
    }
    delete elements.preview.dataset.preview;
    elements.preview.hidden = true;
    elements.detail.hidden = true;
    elements.note.hidden = true;
    elements.error.textContent = "";
    elements.position.textContent = "";
  }

  elements.prev.addEventListener("click", () => move(-1));
  elements.next.addEventListener("click", () => move(1));
  elements.refresh.addEventListener("click", () => {
    if (state.sessionId) {
      void load();
    }
  });

  return {
    close: () => {
      state.sessionId = null;
      reset();
      elements.view.hidden = true;
    },
    isOpen: () => state.sessionId !== null,
    open: (sessionId) => {
      reset();
      state.sessionId = sessionId;
      elements.view.hidden = false;
      elements.title.textContent = "Loading the graphs…";

      return load();
    },
    refresh: () => (state.sessionId ? load() : Promise.resolve()),
  };
}

// A status glyph and its label, in the glyph's tone.
function statusRow(document, [glyph, tone], label) {
  const row = element(document, "span", "graph-summary-status tone--" + tone);
  row.append(element(document, "span", "graph-glyph", glyph), label);

  return row;
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
