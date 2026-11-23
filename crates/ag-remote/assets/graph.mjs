// Execution graphs of the open session as the terminal shows them: a
// summary bar beside the chat and a panel with the latest graph proposal
// and each graph's nodes, their details and their outputs, like the
// terminal's `w` inspector. With the `graph_actions` capability the panel
// also offers the inspector's actions where Agentty lists them: accepting
// the selected output, retrying or replying to the selected node, and
// resuming, adding provider calls to, or cancelling the graph. Each action
// names the snapshot the panel last showed, so Agentty refuses it once the
// graph changed. Proposals stay read only; chat replies act on them.
// Agentty sends every label; ids, objectives, errors and outputs are
// untrusted and only ever become text nodes, directly or through
// markdown.mjs. Functions take `document` as a parameter, so the Node tests
// in crates/ag-remote/tests/web/graph.test.mjs pass the fake DOM.

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
// The button of each action, by the name Agentty lists it under, and the
// order the graph's and a node's rows show them in.
const ACTION_LABELS = {
  accept: "Accept",
  resume: "Resume",
  extend_budget: "Add provider calls",
  cancel: "Cancel graph",
  retry: "Retry",
  reply: "Reply",
};
const GRAPH_ACTIONS = ["resume", "extend_budget", "cancel"];
const NODE_ACTIONS = ["retry", "reply"];

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

// Whether `item`, a graph, node or output of a `graphs` response, offers
// `action`, given the server's `capabilities`.
export function offers(capabilities, item, action) {
  return (
    capabilities.includes("graph_actions") &&
    Array.isArray(item?.actions) &&
    item.actions.includes(action)
  );
}

// The number of provider calls `text` asks to add, or `null` unless it is
// a whole number from 1 to `max`.
export function providerCalls(text, max) {
  const trimmed = String(text).trim();
  const calls = Number(trimmed);

  return /^\d+$/.test(trimmed) && calls >= 1 && calls <= max ? calls : null;
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
// `title`, `error`, `hints`, `note`, `actions` (the graph's actions),
// `preview`, `nodes` (the node list), `detail` (the selected node) and
// `cancelDialog` (the confirmation before cancelling a graph).
// `request(body)` sends one request and resolves with its result; `window`
// supplies the clipboard for code blocks.
//
// Returns `open(sessionId)`, `refresh()`, which reads the graphs again
// keeping the selection, `close()`, `isOpen()`, and
// `setCapabilities(list)`. A refresh changes only what changed, so the
// reader keeps their place, the source view of an output, and a reply or
// number being typed while the panel still shows the node or spent budget
// it is for.
export function createGraph({ document, elements, request, window = null }) {
  const state = {
    capabilities: [],
    choice: null,
    // Open text boxes by the `areaKey` of the part of the graph they act
    // on, each kept until it is sent or discarded, or the panel no longer
    // shows that part.
    editors: new Map(),
    // The last refused action: the `areaKey` of the part it acted on and
    // why.
    failure: null,
    // Counts the panel's opens and closes, so an action that finishes
    // after one changes nothing.
    generation: 0,
    // The selection the shown response answers and its `version`.
    held: null,
    loads: 0,
    nodeId: null,
    outputId: null,
    // Whether an action is on its way to Agentty.
    pending: false,
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
    dropStale(view.graph);
    renderSelector();
    renderTitle(view);
    renderHints(view);
    renderPreview(view.proposal);
    elements.note.textContent = view.graph?.note ?? "";
    elements.note.hidden = !view.graph?.note;
    renderGraphActions(view.graph);
    renderNodes(view.graph);
    renderDetail(view.graph);
  }

  // Closes each text box, and drops a refusal, that is not about the
  // shown `graph`, or, for the number box, about the spent budget it
  // shows, so neither carries over to another graph and no box to another
  // budget.
  function dropStale(graph) {
    const shownBudget = graph ? budgetArea(graph) : null;
    for (const area of state.editors.keys()) {
      if (!isShownArea(area, graph, shownBudget)) {
        state.editors.delete(area);
      }
    }
    const refused = state.failure?.area;
    if (refused && !isShownArea(refused, graph, shownBudget)) {
      state.failure = null;
    }
  }

  // The graph's own actions, an open number box, and a refusal of either.
  function renderGraphActions(graph) {
    const parts = graph ? graphParts(graph) : [];
    placeChildren(elements.actions, parts);
    elements.actions.hidden = parts.length === 0;
  }

  function graphParts(graph) {
    const area = graphRowArea(graph);
    const box = budgetArea(graph);
    const row = actionRow(area, graph, GRAPH_ACTIONS, (action) => graphAction(area, graph, action));

    return [row, editorFor(box), failureFor(box), failureFor(area)].filter(Boolean);
  }

  function graphAction(area, graph, action) {
    if (action === "resume") {
      void act(area, "resume");
    } else if (action === "extend_budget") {
      const box = budgetArea(graph);
      openEditor(box, () => budgetEditor(box, area, graph.budget));
    } else {
      elements.cancelDialog.dataset.graphId = String(graph.id);
      elements.cancelDialog.returnValue = "";
      elements.cancelDialog.showModal();
    }
  }

  // A row of buttons for the `actions` of `item` that Agentty offers, or
  // `null` when it offers none.
  function actionRow(area, item, actions, onAction) {
    const offered = actions.filter((action) => offers(state.capabilities, item, action));
    if (offered.length === 0) {
      return null;
    }
    const row = element(document, "div", "graph-action-row");
    row.append(...offered.map((action) => actionButton(area, action, onAction)));

    return row;
  }

  function actionButton(area, action, onAction) {
    const tone = action === "cancel" ? " btn--danger" : "";
    const button = element(document, "button", "btn graph-action" + tone, ACTION_LABELS[action]);
    button.type = "button";
    button.dataset.action = action;
    button.disabled = state.pending;
    button.addEventListener("click", () => onAction(action));

    return button;
  }

  // The refusal of the last action on `area`, if there was one.
  function failureFor(area) {
    return state.failure?.area === area
      ? element(document, "p", "error graph-action-error", state.failure.message)
      : null;
  }

  // The open text box of `area`, if any.
  function editorFor(area) {
    const editor = state.editors.get(area);
    if (!editor) {
      return null;
    }
    syncEditor(editor);

    return editor.form;
  }

  // A text box takes no edits while an action is on its way, so what was
  // sent is what it shows, and sends unless it is empty.
  function syncEditor(editor) {
    editor.input.disabled = state.pending;
    editor.discard.disabled = state.pending;
    editor.send.disabled = state.pending || !editor.input.value.trim();
  }

  // Opens the text box `create()` builds for `area`, or keeps the one
  // already open there, and puts the cursor in it.
  function openEditor(area, create) {
    if (!state.editors.has(area)) {
      state.editors.set(area, create());
    }
    state.failure = null;
    render();
    state.editors.get(area)?.input.focus();
  }

  function closeEditor(area) {
    state.editors.delete(area);
    state.failure = null;
    render();
  }

  // The number box `area` that adds provider calls to the spent `budget`,
  // the one it names. A count out of range is refused beside the box;
  // Agentty's refusal shows on the graph's row `rowArea`, so it stays when
  // the reload closes the box because the budget changed.
  function budgetEditor(area, rowArea, budget) {
    const max = Number(budget?.max_additional_calls) || 1;
    const editor = editorForm(area, "extend_budget", "Add", () => {
      const calls = providerCalls(editor.input.value, max);
      if (calls === null) {
        refuse(area, "Enter a whole number of provider calls from 1 to " + max);
      } else {
        const fields = { additional_calls: calls, budget_id: budget?.id };
        void act(area, "extend_budget", fields, rowArea);
      }
    });
    const input = element(document, "input", "field graph-editor-input");
    input.type = "number";
    for (const [name, value] of [["min", "1"], ["max", String(max)], ["step", "1"], ["inputmode", "numeric"]]) {
      input.setAttribute(name, value);
    }
    const label = element(document, "label", "graph-editor-label");
    const name = budget?.id ? String(budget.id) : "the spent budget";
    label.append(
      element(document, "span", "", "Provider calls to add to " + name + ", from 1 to " + max),
      input,
    );

    return withInput(editor, label, input);
  }

  // The reply box that sends guidance to `nodeId`. An empty reply is not
  // sent.
  function replyEditor(area, nodeId) {
    const editor = editorForm(area, "reply", "Send", () => {
      if (editor.input.value.trim()) {
        void act(area, "reply", { message: editor.input.value, node_id: nodeId });
      }
    });
    const input = element(document, "textarea", "field graph-editor-input");
    input.rows = 3;
    input.setAttribute("placeholder", "Guidance that revises " + nodeId + "'s output");
    input.setAttribute("aria-label", "Reply to " + nodeId);

    return withInput(editor, input, input);
  }

  // A form for `action` on `area` with Discard and a `sendLabel` button
  // that runs `onSend`; `withInput` adds its field.
  function editorForm(area, action, sendLabel, onSend) {
    const form = element(document, "form", "graph-editor");
    const discard = element(document, "button", "btn btn--ghost", "Discard");
    discard.type = "button";
    discard.addEventListener("click", () => closeEditor(area));
    const send = element(document, "button", "btn btn--primary", sendLabel);
    send.type = "submit";
    const buttons = element(document, "div", "graph-editor-actions");
    buttons.append(discard, send);
    form.append(buttons);
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      onSend();
    });

    return { action, area, discard, form, input: null, send };
  }

  function withInput(editor, field, input) {
    editor.input = input;
    editor.form.prepend(field);
    input.addEventListener("input", () => syncEditor(editor));

    return editor;
  }

  function refuse(area, message) {
    state.failure = { area, message };
    render();
  }

  // Sends `action` on the shown graph with `fields`, bound to the snapshot
  // the panel last showed, then reads the graphs again either way, so the
  // reader sees what changed. A refusal shows by `refusedArea`, `area`
  // unless the caller names another, and keeps the text box of `area`;
  // success closes it. Once the panel closed or opened again meanwhile,
  // the answer changes nothing.
  async function act(area, action, fields = {}, refusedArea = area) {
    const graph = state.response?.graph;
    if (state.pending || !graph) {
      return;
    }
    const generation = state.generation;
    state.pending = true;
    state.failure = null;
    render();
    let refusal = null;
    try {
      await request({
        method: "act_on_session_graph",
        action: action,
        graph_id: graph.id,
        session_id: state.sessionId,
        snapshot: graph.snapshot,
        ...fields,
      });
    } catch (error) {
      refusal = { area: refusedArea, message: error.message };
    }
    if (generation !== state.generation) {
      return;
    }
    if (refusal) {
      state.failure = refusal;
    } else if (state.editors.get(area)?.action === action) {
      state.editors.delete(area);
    }
    state.pending = false;
    render();
    // A refused text box takes edits again; the cursor returns to it.
    if (refusal) {
      state.editors.get(area)?.input.focus();
    }
    await load();
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
    const outputs = renderOutputs(
      graph.id,
      String(node.id),
      Array.isArray(graph.outputs) ? graph.outputs : [],
    );
    placeChildren(elements.detail, [info, ...nodeActions(graph.id, node), ...outputs]);
  }

  // The actions of `node`, the selected node of graph `graphId`, its open
  // reply box, and a refusal.
  function nodeActions(graphId, node) {
    const nodeId = String(node.id);
    const area = areaKey("node", graphId, nodeId);
    const row = actionRow(area, node, NODE_ACTIONS, (action) => {
      if (action === "retry") {
        void act(area, "retry", { node_id: nodeId });
      } else {
        openEditor(area, () => replyEditor(area, nodeId));
      }
    });

    return [row, editorFor(area), failureFor(area)].filter(Boolean);
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

  // The output selector, the selected output's action and its refusal,
  // and the selected output as an answer, for `nodeId` of graph
  // `graphId`. The same output keeps its node, and with it the reader's
  // source view.
  function renderOutputs(graphId, nodeId, outputs) {
    if (outputs.length === 0) {
      return [];
    }
    const found = outputs.findIndex((output) => output.id === state.outputId);
    const index = Math.max(found, 0);
    const output = outputs[index];
    state.outputId = output.id;
    const area = areaKey("output", graphId, output.id);
    const bar = element(document, "div", "graph-output-bar");
    bar.append(
      stepButton("‹", "Previous output", index > 0, () => selectOutput(outputs, index - 1)),
      element(document, "span", "graph-output-label", outputLabel(outputs, index)),
      stepButton("›", "Next output", index < outputs.length - 1, () =>
        selectOutput(outputs, index + 1),
      ),
    );
    if (offers(state.capabilities, output, "accept")) {
      bar.append(
        actionButton(area, "accept", () =>
          act(area, "accept", { node_id: nodeId, output_id: output.id }),
        ),
      );
    }

    return [bar, failureFor(area), outputBody(output)].filter(Boolean);
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

  // Steps `delta` views through the proposal and the graphs. The view it
  // steps to is not on screen yet, so its next read is a full one.
  function move(delta) {
    const choices = graphChoices(state.response);
    const next = choices[choiceIndex(choices, state.choice) + delta];
    if (!next) {
      return;
    }
    state.choice = next;
    state.held = null;
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
    state.editors.clear();
    state.failure = null;
    state.generation += 1;
    state.held = null;
    state.loads += 1;
    state.nodeId = null;
    state.outputId = null;
    state.pending = false;
    state.response = null;
    for (const node of [
      elements.hints,
      elements.actions,
      elements.nodes,
      elements.detail,
      elements.preview,
    ]) {
      node.replaceChildren();
    }
    delete elements.preview.dataset.preview;
    elements.preview.hidden = true;
    elements.actions.hidden = true;
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
  // Cancelling is permanent, so it waits for the reader to confirm, and
  // only on the graph they confirmed it for.
  elements.cancelDialog.addEventListener("close", () => {
    const graphId = elements.cancelDialog.dataset.graphId;
    delete elements.cancelDialog.dataset.graphId;
    const shown = state.choice?.kind === "graph" ? state.response?.graph : null;
    if (elements.cancelDialog.returnValue === "confirm" && shown && String(shown.id) === graphId) {
      void act(graphRowArea(shown), "cancel");
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
    // Offers the actions only once the server lists `graph_actions`.
    setCapabilities: (capabilities) => {
      state.capabilities = Array.isArray(capabilities) ? capabilities : [];
      if (state.response) {
        render();
      }
    },
  };
}

// Names the part of graph `graphId` a text box or a refusal is about: the
// graph's own row, the number box for its spent budget, a node, or an
// output, by `kind` and the id of that `part`.
function areaKey(kind, graphId, part) {
  return JSON.stringify([kind, String(graphId), part ?? null]);
}

// The graph's own row of `graph`.
function graphRowArea(graph) {
  return areaKey("graph", graph.id);
}

// The number box of `graph` for the spent budget it shows.
function budgetArea(graph) {
  return areaKey("budget", graph.id, graph.budget?.id);
}

// Whether `area` is about the shown `graph`, whose number box would be
// `shownBudget`.
function isShownArea(area, graph, shownBudget) {
  const [kind, graphId] = JSON.parse(area);

  return kind === "budget" ? area === shownBudget : Boolean(graph) && graphId === String(graph.id);
}

// Makes `wanted` the children of `parent` in order, moving, adding, or
// removing only what differs, so a field being typed in keeps its focus.
function placeChildren(parent, wanted) {
  const kept = new Set(wanted);
  // `children` is live, so the stale ones are removed once found.
  const stale = [];
  for (const child of parent.children) {
    if (!kept.has(child)) {
      stale.push(child);
    }
  }
  for (const child of stale) {
    child.remove();
  }
  wanted.forEach((child, index) => {
    const current = parent.children[index];
    if (current !== child) {
      parent.insertBefore(child, current ?? null);
    }
  });
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
