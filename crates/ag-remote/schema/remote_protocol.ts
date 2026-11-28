// Generated from the `ag-remote` protocol types; do not edit. Regenerate with:
//   AGENTTY_REMOTE_SCHEMA_UPDATE=1 cargo test --locked -p ag-remote --lib protocol::tests::schema

/**
 * One JSON text frame of the Agentty remote-control protocol, which a
 * device and a host exchange inside their encrypted stream: a
 * `ClientFrame` the device sends or a `ServerFrame` the host sends.
 * `ClientFrame` describes what a host accepts, so a field with a default
 * is optional; `ServerFrame` describes what a host sends, so a field it
 * may leave out is optional and is never null.
 *
 * A host speaks exactly `x-protocol-version` and rejects a `hello` or
 * `pair` frame that names another. A newer host of the same version may
 * add fields, frame types, response kinds, and enum values: a client
 * ignores fields it does not know, skips a frame of an unknown `type`,
 * treats a response of an unknown `kind` as an answer it cannot use, and
 * shows an unknown enum value as unknown instead of failing. A client
 * sends a request that names a capability only to a host whose
 * `capabilities` lists it. The schema lists only what this version
 * sends, so a client that rejects incoming frames failing it breaks
 * with a newer host.
 */
export type ProtocolVersion = 1;

/** A frame sent by a remote client. */
export type ClientFrame =
  /**
   * Authenticates a previously paired device by the key its encrypted
   * stream proves.
   */
  | {
      type: "hello";
      /** Protocol version the client speaks. */
      protocol_version: number;
    }
  /** Pairs a new device with a one-time code shown in the terminal. */
  | {
      type: "pair";
      /** Pairing code, matched ignoring case, dashes, and whitespace. */
      code: string;
      /** Name the terminal shows for this device. */
      device_name: string;
      /** Protocol version the client speaks. */
      protocol_version: number;
    }
  /** Invokes one session operation after authentication. */
  | {
      type: "request";
      /** Client-chosen identifier echoed in the response. */
      id: number;
      /** Operation to run. */
      request: RemoteRequest;
    };

/**
 * An optional request a host offers by listing it in the `capabilities`
 * of `welcome` and `paired`; the requests of the first protocol version
 * need none. A newer host may list capabilities this version does not
 * name, so `capabilities` stays a list of strings.
 */
export type RemoteCapability =
  | "host"
  | "create_session"
  | "merge"
  | "diff"
  | "files"
  | "project_files"
  | "stop_turn"
  | "sync"
  | "settings"
  | "settings_agent"
  | "publish"
  | "merge_review_request"
  | "sync_project"
  | "graphs"
  | "graph_actions"
  | "graph_proposal"
  | "images"
  | "projects";

/** One file a session's diff changes. */
export type RemoteDiffFile = {
  /** Lines the file's patch adds. */
  added_lines: number;
  /** Lines the file's patch removes. */
  deleted_lines: number;
  /** Path before a rename. */
  old_path?: string;
  /** Path after the change; a deleted file keeps its old path. */
  path: string;
  /** How the change affects the file. */
  status: RemoteDiffFileStatus;
};

/** How a session's diff changes one file. */
export type RemoteDiffFileStatus =
  /** The file is new. */
  | "added"
  /** The file is removed. */
  | "deleted"
  /** The file's contents or mode changed. */
  | "modified"
  /** The file moved, possibly with changes. */
  | "renamed";

/** One file or directory in a session's worktree. */
export type RemoteFileEntry = {
  /** Whether the entry is a directory. */
  is_dir: boolean;
  /** Path relative to the worktree root. */
  path: string;
};

/** Outcome of a session's latest focused review. */
export type RemoteFocusedReview = {
  /** Whether the review finished or failed. */
  state: RemoteFocusedReviewState;
  /**
   * The review's Markdown when it is `ready`, or the plain-text failure
   * message when it `failed`.
   */
  text: string;
};

/** State of a focused review a remote client can show. */
export type RemoteFocusedReviewState =
  /** The review could not be produced. */
  | "failed"
  /** The review finished. */
  | "ready";

/** The selected graph of a `graphs` response. */
export type RemoteGraph = {
  /**
   * What `act_on_session_graph` can do to the graph as a whole now:
   * `cancel`, `extend_budget`, or `resume`, as the terminal's inspector
   * offers them. Absent when it can do none.
   */
  actions?: RemoteGraphAction[];
  /**
   * The spent budget `extend_budget` adds provider calls to, present
   * while `actions` offers it: the selected node's, or else the first
   * one a pending node waits for, as the terminal's inspector shows it.
   */
  budget?: RemoteGraphBudget;
  /** Graph identity. */
  id: string;
  /** Whether the graph is a focused review that Agentty drives. */
  is_focused_review?: boolean;
  /** The selected node, absent while the graph has no progress. */
  node?: string;
  /** Every node in declaration order. */
  nodes: RemoteGraphNode[];
  /**
   * What Agentty says about how the graph is driven, when it says
   * anything.
   */
  note?: string;
  /** Outputs of the selected node's latest execution. */
  outputs?: RemoteGraphOutput[];
  /** Current revision. */
  revision: number;
  /**
   * Opaque token of the shown graph state; it changes whenever the
   * graph's revision, status, or node progress does.
   */
  snapshot: string;
  /** Live status, absent when Agentty has no progress for the graph. */
  status?: RemoteGraphStatus;
  /** `status` as the terminal shows it. */
  status_label: string;
};

/** An action of the terminal's graph inspector. */
export type RemoteGraphAction =
  /** Accept a validated output, releasing the nodes that wait for it. */
  | "accept"
  /** Cancel the graph for good. */
  | "cancel"
  /** Add provider calls to the graph's spent budget. */
  | "extend_budget"
  /** Send guidance that revises a settled node's output. */
  | "reply"
  /** Restart a paused graph. */
  | "resume"
  /** Run a failed or cancelled node again. */
  | "retry";

/** One execution graph action, sent as an `act_on_session_graph` request. */
export type RemoteGraphActionRequest = {
  /**
   * What to do, one of the actions the `graphs` response lists on the
   * graph, the node, or the output it applies to.
   */
  action: RemoteGraphAction;
  /**
   * Provider calls `extend_budget` adds, from one to the graph's
   * `budget.max_additional_calls`.
   */
  additional_calls?: number | null;
  /**
   * The spent budget `extend_budget` adds provider calls to: the graph's
   * `budget.id` in the `graphs` response the action was chosen from.
   */
  budget_id?: string | null;
  /** Graph to act on, as the `graphs` response names it. */
  graph_id: string;
  /** Guidance `reply` sends; it must not be blank. */
  message?: string | null;
  /** Node `accept`, `reply`, and `retry` apply to. */
  node_id?: string | null;
  /** Output of `node_id` that `accept` accepts. */
  output_id?: string | null;
  /** Session that owns the graph. */
  session_id: string;
  /**
   * The graph's `snapshot` in the `graphs` response the action was
   * chosen from.
   */
  snapshot: string;
};

/** One output that waits for the user's approval. */
export type RemoteGraphApproval = {
  /** Node that produced the output. */
  node_id: string;
  /** Whether a chat reply may revise the output instead of accepting it. */
  revisable?: boolean;
  /** Line telling the user how to act on it, as the terminal says it. */
  text: string;
};

/** A graph's spent provider-call budget. */
export type RemoteGraphBudget = {
  /** Budget name. */
  id: string;
  /** Most provider calls one `extend_budget` action adds. */
  max_additional_calls: number;
};

/** One graph of a session in a `graphs` response. */
export type RemoteGraphEntry = {
  /** Graph identity, as `get_session_graphs` accepts it. */
  id: string;
  /** Whether the graph is a focused review that Agentty drives. */
  is_focused_review?: boolean;
  /** Current revision. */
  revision: number;
};

/** One node of a graph. */
export type RemoteGraphNode = {
  /**
   * What `act_on_session_graph` can do to the node now: `reply` or
   * `retry`, as the terminal's inspector offers them with the node
   * selected. Absent when it can do neither.
   */
  actions?: RemoteGraphAction[];
  /** Name of the agent definition the node runs. */
  agent: string;
  /** Agent provider, such as `codex`. */
  agent_kind: string;
  /** Executions admitted so far, retries included. */
  attempts: number;
  /** Whether a dependent waits for the user to accept the node's output. */
  awaits_approval?: boolean;
  /** Why the pending node cannot dispatch, each as the terminal says it. */
  blocked?: string[];
  /** The latest failure, if any. */
  error?: string;
  /** How the user acts on the node while it waits for them. */
  hint?: string;
  /** Node identity, as `get_session_graphs` accepts it. */
  id: string;
  /** The node's status as the terminal shows it. */
  label: string;
  /** Model identifier the node runs. */
  model: string;
  /** What the node should achieve. */
  objective: string;
  /** Questions from the node's latest settled response. */
  questions?: RemoteQuestion[];
  /** Reasoning effort the node runs with, such as `high`. */
  reasoning: string;
  /** The node's dependencies in declaration order. */
  requires?: RemoteGraphRequirement[];
  /** Live state of the node's latest execution. */
  status: RemoteGraphNodeStatus;
  /**
   * Whether the server cut `objective`, `error`, or a question, at a
   * line boundary, to keep the response small.
   */
  truncated?: boolean;
};

/** Live state of a graph node. */
export type RemoteGraphNodeStatus =
  /** It waits for answers to its questions. */
  | "awaiting_answers"
  /** It or its graph was cancelled. */
  | "cancelled"
  /** It failed and needs a retry. */
  | "failed"
  /** It has not been admitted. */
  | "pending"
  /** It waits for a worker. */
  | "queued"
  /** A worker runs it. */
  | "running"
  /** It settled without questions. */
  | "succeeded";

/** One output of the selected graph node. */
export type RemoteGraphOutput = {
  /**
   * What `act_on_session_graph` can do to the output now: `accept`, as
   * the terminal's inspector offers it with the output selected. Absent
   * when it can do nothing.
   */
  actions?: RemoteGraphAction[];
  /** Output identity. */
  id: string;
  /** Output name the node declares. */
  name: string;
  /** Whether the output was validated or accepted. */
  state: RemoteGraphOutputState;
  /**
   * The output as Markdown, cut at a line boundary when it is too large;
   * empty when earlier outputs left it no room.
   */
  text: string;
  /** Whether the server cut the end of `text` to keep the response small. */
  truncated?: boolean;
  /** Version within the node's named output. */
  version: number;
};

/** Evidence state of a graph output. */
export type RemoteGraphOutputState =
  /** The user accepted it. */
  | "accepted"
  /** It was saved but not validated. */
  | "produced"
  /** It was validated and can be accepted. */
  | "validated";

/** A session's latest graph proposal. */
export type RemoteGraphProposal = {
  /** Graph the approved version launched, absent before approval. */
  graph_id?: string;
  /**
   * How the user acts on the pending proposal, absent once it is
   * approved or cancelled.
   */
  hint?: string;
  /** Proposal identity. */
  id: string;
  /** Where the proposal stands. */
  phase: RemoteGraphProposalPhase;
  /**
   * The proposal's details as Markdown, including the captured request
   * and each proposed node's captured instructions.
   */
  preview: string;
  /**
   * Clarification questions the version asks before it resolves. Agentty
   * bounds them with the size of the proposal's response.
   */
  questions?: RemoteQuestion[];
  /**
   * Whether the server cut the end of `preview`, at a line boundary, to
   * keep the response small.
   */
  truncated?: boolean;
  /** Version number. */
  version: number;
};

/** Where a graph proposal stands. */
export type RemoteGraphProposalPhase =
  /** The user approved the version and its graph launched. */
  | "approved"
  /** The version resolved to a graph waiting for the user's approval. */
  | "awaiting_approval"
  /** The proposal was discarded. */
  | "cancelled"
  /** Generation failed. */
  | "failed"
  /** The model is generating the version. */
  | "generating"
  /** The model asked clarification questions. */
  | "needs_input"
  /** The model proposed a graph Agentty could not resolve. */
  | "unresolved";

/** One dependency of a graph node. */
export type RemoteGraphRequirement = {
  /** What the dependency waits for, as the terminal's inspector says it. */
  gate: string;
  /** The dependency as the terminal's node timeline shows it. */
  label: string;
  /** Node the dependency waits for. */
  node_id: string;
  /** Whether the dependency no longer blocks the node. */
  satisfied?: boolean;
};

/** Live status of a graph. */
export type RemoteGraphStatus =
  /** Prerequisites, acceptance, clarification, or resources hold it. */
  | "blocked"
  /** It was cancelled. */
  | "cancelled"
  /** Every node settled successfully. */
  | "completed"
  /** A node needs a retry. */
  | "failed"
  /** No driver runs it; it needs a resume. */
  | "paused"
  /** Work is queued or running. */
  | "running";

/** One image sent with a `send_message` request. */
export type RemoteImage = {
  /** The PNG or JPEG file, in standard base64 with padding. */
  data: string;
};

/** One chat-visible transcript message. */
export type RemoteMessage = {
  /** Message text. */
  content: string;
  /** Display category. */
  kind: RemoteMessageKind;
};

/** Display category of one transcript message. */
export type RemoteMessageKind =
  /** Tool and skill usage summary. */
  | "activity"
  /** Agent answer. */
  | "assistant"
  /** Workflow notice. */
  | "notice"
  /** User prompt. */
  | "user";

/**
 * The agent, model and settings a new session of one mode starts with, as
 * `get_host` lists it. A device picks other settings for the new session,
 * and another model among the `models` of the same `agent`.
 */
export type RemoteModeModel = {
  /** Agent that runs the session, as `models` names it. */
  agent: string;
  /** Creation mode, one of `session_modes`. */
  mode: RemoteSessionMode;
  /** Model identifier, as `models` lists it. */
  model: string;
  /** Display name of the model. */
  name: string;
  /** Reasoning level the new session starts with, such as `high`. */
  reasoning?: string;
  /** Style of the new session's answers, such as `concise`. */
  response_style?: string;
  /**
   * Speed the new session starts with, `normal` or `fast`. Absent when
   * the agent has no speed control.
   */
  speed_mode?: string;
};

/** One model a session can switch to, as `get_host` lists it. */
export type RemoteModelOption = {
  /**
   * Agent that runs the model. A session switches to the models of
   * another agent by naming it in `set_session_settings`, which a server
   * accepts when it offers `settings_agent`.
   */
  agent: string;
  /** Short description of the model. */
  description: string;
  /**
   * Model identifier, as a session snapshot reports `model_id` and
   * `set_session_settings` accepts it as `model`.
   */
  model: string;
  /** Display name of the model, as a session snapshot reports `model`. */
  name: string;
  /** Whether the model can run in fast mode. */
  supports_fast: boolean;
};

/**
 * One project as the session list shows it: the host's active project, or
 * the one a `list_sessions` request named.
 */
export type RemoteProject = {
  /**
   * Commits the branch is ahead of its upstream, as of the host's latest
   * fetch. Absent while the host has not compared them.
   */
  ahead?: number;
  /**
   * Commits the branch is behind its upstream, as of the host's latest
   * fetch. Absent while the host has not compared them.
   */
  behind?: number;
  /** Branch the project's checkout is on. Absent when it has none. */
  branch?: string;
  /**
   * Identifier of the project, as `list_projects` names it, when the
   * host read the sessions and this status together. Absent otherwise
   * and from older servers, so a client that asked for a project treats
   * a listing without it as not that project's.
   */
  id?: string;
  /** Display name of the project. */
  name: string;
  /**
   * Latest sync of this project, started by a device or in the terminal,
   * while the host still shows it.
   */
  sync?: RemoteProjectSync;
};

/** One project the host lists, as `list_projects` returns it. */
export type RemoteProjectEntry = {
  /**
   * Sessions of the project still in an active lifecycle state. Absent
   * when there are none.
   */
  active_sessions?: number;
  /** Opaque identifier a request's `project_id` names the project by. */
  id: string;
  /**
   * Whether the project is the one the terminal has open. Absent when it
   * is not.
   */
  is_active?: boolean;
  /** Display name of the project. */
  name: string;
};

/**
 * One sync of a project's branch with its upstream.
 *
 * The count and message fields depend on `phase`; a page ignores the ones
 * it does not expect.
 */
export type RemoteProjectSync = {
  /** Branch the sync updates. */
  branch: string;
  /**
   * Files in conflict that an agent is resolving, or has resolved once
   * the sync is complete.
   */
  conflicts?: number;
  /** Why the sync is blocked or failed. */
  message?: string;
  /** How far the sync has come. */
  phase: RemoteProjectSyncPhase;
  /** Commits a complete sync pulled, when the host counted them. */
  pulled_commits?: number;
  /** Commits a complete sync pushed, when the host counted them. */
  pushed_commits?: number;
};

/** How far a project sync has come. */
export type RemoteProjectSyncPhase =
  /** The host did not start the sync, such as while a merge runs. */
  | "blocked"
  /** The branch matches its upstream. */
  | "complete"
  /** Git, authentication, or conflict resolution failed. */
  | "failed"
  /** An agent is resolving conflicts. */
  | "resolving_conflicts"
  /** The host is pulling, rebasing, or pushing. */
  | "running";

/** One pending clarification question. */
export type RemoteQuestion = {
  /** Suggested answers, recommended first. */
  options: string[];
  /** Question text. */
  text: string;
};

/** One answer to a pending clarification question. */
export type RemoteQuestionAnswer = {
  /** Answer text. */
  answer: string;
  /** Exact question text being answered. */
  question: string;
};

/**
 * Session operations a remote client may invoke.
 *
 * Each request beyond reading and chatting is a mutation the user approved
 * for remote devices. A server offers an optional request only through its
 * capability, and every request that names a session reaches only sessions
 * the host lists.
 *
 * The requests about a project, `get_host`, `list_project_files`,
 * `list_sessions`, `sync_project`, and `create_session`, are for the
 * host's active project, read when the host serves the request, unless they
 * name another in `project_id`, which reaches only a project the host
 * lists. A client sends `project_id` only to a server that offers the
 * `projects` capability: an older server ignores the field it does not
 * know and answers for its active project, so a client also checks that
 * the project an answer names is the one it asked for.
 */
export type RemoteRequest =
  /**
   * Acts on a session's execution graph as the terminal's inspector
   * does: accepts an output, retries or replies to a node, resumes or
   * cancels the graph, or adds provider calls to its spent budget.
   * Cancelling is permanent. Agentty refuses an action the graph's
   * `snapshot` no longer matches or does not offer. Offered through the
   * `graph_actions` capability.
   */
  | {
      method: "act_on_session_graph";
    } & RemoteGraphActionRequest
  /**
   * Answers the session's complete current question set: its turn's
   * `questions`, or, with `graph_question_target`, its `graph_questions`,
   * which go to the graph work that asked them.
   *
   * Agentty refuses answers that name graph work other than the one whose
   * questions are pending now, even when it asks the same questions, and
   * answers naming graph work while the turn asks its own. Answers that
   * name no graph work answer only the turn's questions, so a request
   * from a page that predates graph questions never reaches graph work.
   */
  | {
      method: "answer_questions";
      /** Answers paired with the exact question text. */
      answers: RemoteQuestionAnswer[];
      /**
       * The snapshot's `graph_question_target` of the `graph_questions`
       * answered; absent when answering the turn's `questions`.
       */
      graph_question_target?: string | null;
      /** Session that asked the questions. */
      session_id: string;
    }
  /**
   * Cancels the whole session, not only its current turn.
   *
   * The host stops the session's agent work, marks the session `Canceled`,
   * and removes its worktree, Git record, and branch, which discards any
   * unmerged and unpublished changes.
   */
  | {
      method: "cancel_session";
      /** Session to cancel. */
      session_id: string;
    }
  /**
   * Creates a session in the host's active project, or the one
   * `project_id` names, with its worktree and branch. Offered through the
   * `create_session` capability.
   */
  | {
      method: "create_session";
      /**
       * How the session runs; one of the modes `get_host` lists for the
       * same project.
       */
      mode?: RemoteSessionMode;
      /**
       * Listed project to create the session in, as `list_projects`
       * names it; absent for the active project. Understood only by a
       * server that offers `projects`.
       */
      project_id?: string | null;
    }
  /**
   * Starts model work that proposes an execution graph for the task in
   * `message`, as the terminal's `/graph` does. The proposal appears in
   * the session's `graph`; nothing launches until a chat reply approves
   * it. Agentty refuses it unless the session is prepared and idle, so
   * also for a draft that has not started, and refuses an empty or
   * oversized task. Offered through the `graph_proposal` capability.
   */
  | {
      method: "generate_graph_proposal";
      /** Task the graph should carry out. */
      message: string;
      /** Session that owns the proposal. */
      session_id: string;
    }
  /**
   * Describes the host's active project, or the one `project_id` names,
   * as new sessions in it start. Offered through the `host` capability.
   */
  | {
      method: "get_host";
      /**
       * Listed project to describe, as `list_projects` names it; absent
       * for the active project. Understood only by a server that offers
       * `projects`.
       */
      project_id?: string | null;
    }
  /**
   * Reads what a session changes against its base branch. Without `path`
   * the response lists the changed files; with `path` it also carries
   * that file's patch. Offered through the `diff` capability.
   */
  | {
      method: "get_session_diff";
      /** Changed file whose patch to return, as the file list names it. */
      path?: string | null;
      /** Session to read. */
      session_id: string;
    }
  /** Loads one session snapshot. */
  | {
      method: "get_session";
      /** Session to load. */
      session_id: string;
    }
  /**
   * Reads a session's graph proposal and execution graphs as the
   * terminal's inspector shows them, without acting on any. Offered
   * through the `graphs` capability.
   */
  | {
      method: "get_session_graphs";
      /**
       * Graph to select, one the response lists in `graphs`. Without it
       * the latest proposal's approved graph is selected, or else the
       * first graph.
       */
      graph_id?: string | null;
      /**
       * `version` of the `graphs` response the device shows for this same
       * selection. When the response would be that version again, it is
       * `unchanged` instead and carries nothing else.
       */
      known?: string | null;
      /**
       * Node of the selected graph whose details and outputs to return.
       * Without it, or when the graph has no such node, the first node.
       */
      node_id?: string | null;
      /** Session to read. */
      session_id: string;
    }
  /**
   * Lists the names of the files and directories in the checkout of the
   * host's active project, or the one `project_id` names, directories
   * first, without their contents, for mentioning files in a new
   * session's first message. Offered through the `project_files`
   * capability.
   */
  | {
      method: "list_project_files";
      /**
       * Listed project whose checkout to list, as `list_projects` names
       * it; absent for the active project. Understood only by a server
       * that offers `projects`.
       */
      project_id?: string | null;
    }
  /**
   * Lists the projects the host lists, in its project switcher's order:
   * its active project and every other one the switcher offers. Offered
   * through the `projects` capability, which a server lists only when its
   * host also reports project status, so a listing names its project.
   */
  | {
      method: "list_projects";
    }
  /**
   * Lists the names of the files and directories in a session's worktree,
   * directories first, without their contents. A session without a
   * worktree lists nothing. Offered through the `files` capability.
   */
  | {
      method: "list_session_files";
      /** Session whose worktree to list. */
      session_id: string;
    }
  /**
   * Lists the sessions of the host's active project, or of the one
   * `project_id` names, with that project's status.
   */
  | {
      method: "list_sessions";
      /**
       * Listed project whose sessions to list, as `list_projects` names
       * it; absent for the active project. Understood only by a server
       * that offers `projects`.
       */
      project_id?: string | null;
    }
  /**
   * Merges the session's open GitHub pull request on GitHub, with the
   * user's forge credentials, when the session's `review_ready` is
   * `ready`, or `ready_with_bypass` and `bypass` is true. A draft is
   * marked ready for review first. A `ready` pull request whose base
   * branch requires a merge queue joins the queue instead; a bypass
   * merges past branch protections and any merge queue with the user's
   * administrator rights.
   *
   * Agentty reads the pull request again before acting and answers
   * `refused` with a reason when it is no longer ready for the requested
   * merge, or no longer at the session branch's commit. It answers an
   * error when the session cannot merge it now: no ready GitHub pull
   * request, another merge or publish running, or a turn or other work
   * pending. Offered through the `merge_review_request` capability.
   */
  | {
      method: "merge_review_request";
      /**
       * Whether to merge by bypassing branch protections; it must be
       * true for a `ready_with_bypass` pull request and false for a
       * `ready` one. Absent means false.
       */
      bypass?: boolean;
      /** Session whose pull request merges. */
      session_id: string;
    }
  /**
   * Merges a review-ready session into its base branch in the host's
   * checkout. Offered through the `merge` capability.
   */
  | {
      method: "merge_session";
      /** Session to merge. */
      session_id: string;
    }
  /**
   * Pushes a session to the branch it was published to before, or under
   * its default name the first time, and opens or refreshes its review
   * request with the user's forge credentials. Offered through the
   * `publish` capability.
   */
  | {
      method: "publish_session";
      /** Session to publish. */
      session_id: string;
    }
  /** Sends one chat message, starting, resuming, or queueing a turn. */
  | {
      method: "send_message";
      /**
       * PNG or JPEG images the message refers to as `[Image #n]`,
       * numbered in this order from 1. Agentty stores them in its own
       * prompt-image directory; a placeholder the text lacks is
       * appended. Offered through the `images` capability.
       */
      images?: RemoteImage[];
      /** Message text. */
      message: string;
      /** Session that receives the message. */
      session_id: string;
    }
  /**
   * Stops the session's running turn and returns the session to review,
   * keeping its worktree and changes. It also discards the session's
   * queued messages. Offered through the `stop_turn` capability.
   */
  | {
      method: "stop_turn";
      /** Session whose turn stops. */
      session_id: string;
    }
  /**
   * Syncs the branch of the host's active project, or of the one
   * `project_id` names, with its upstream: a pull with rebase, then a
   * push, in that project's checkout with the user's Git credentials. A
   * conflict starts an agent-assisted resolution. The `project` of a
   * `list_sessions` response for the same project reports the progress
   * and the outcome. Offered through the `sync_project` capability.
   */
  | {
      method: "sync_project";
      /**
       * Listed project to sync, as `list_projects` names it; absent for
       * the active project. Understood only by a server that offers
       * `projects`.
       */
      project_id?: string | null;
    }
  /**
   * Rebases the session branch onto its base branch, or queues the rebase
   * behind a running turn. A conflict starts an agent-assisted resolution
   * turn. Offered through the `sync` capability.
   */
  | {
      method: "sync_session";
      /** Session whose branch is synced. */
      session_id: string;
    }
  /**
   * Changes the session's settings for later turns; an omitted setting
   * stays as it is. The fields carry the values a session snapshot
   * reports, except that `model` is the snapshot's `model_id`. Reasoning
   * and style apply at once; a model or speed change waits while the
   * session still has accepted work, as in the terminal. Offered through
   * the `settings` capability; `agent` is understood only by a server
   * that also offers `settings_agent`.
   */
  | {
      method: "set_session_settings";
      /**
       * Agent to run `model`, as `get_host` names it; omitted, the
       * session keeps its own. Sent only together with `model`, since
       * agents share model ids.
       */
      agent?: string | null;
      /** Model identifier to switch to, one `get_host` lists for `agent`. */
      model?: string | null;
      /** Reasoning level, such as `high`. */
      reasoning?: string | null;
      /** Style of the agent's answers, such as `concise`. */
      response_style?: string | null;
      /** Session whose settings change. */
      session_id: string;
      /** Speed preference, `normal` or `fast`. */
      speed_mode?: string | null;
    };

/** Successful request payload. */
export type RemoteResponse =
  /** The host admitted the mutation; its work may still be running. */
  | {
      kind: "accepted";
    }
  /** The host created a session. */
  | {
      kind: "created";
      /** Identifier of the new session. */
      session_id: string;
    }
  /** What a session changes, answering `get_session_diff`. */
  | {
      kind: "diff";
      /**
       * Changed files in diff order; only the requested file when the
       * request names one.
       */
      files: RemoteDiffFile[];
      /**
       * Unified patch of the requested file, cut at a line boundary when
       * it is too large.
       */
      patch?: string;
      /**
       * Whether the server left out the end of `files` or `patch` to
       * keep the response small.
       */
      truncated?: boolean;
    }
  /** A session's graphs, answering `get_session_graphs`. */
  | {
      kind: "graphs";
      /** The selected graph, absent when the session has none. */
      graph?: RemoteGraph;
      /** Every graph of the session in creation order. */
      graphs?: RemoteGraphEntry[];
      /** The session's latest graph proposal, absent when it has none. */
      proposal?: RemoteGraphProposal;
      /**
       * Whether the response is the request's `known` version again; it
       * then carries only `version`, and the device keeps what it shows.
       */
      unchanged?: boolean;
      /**
       * Opaque token for the content of this response: equal content
       * gives an equal token within one Agentty process. Absent from
       * older servers.
       */
      version?: string;
    }
  /**
   * The project a `get_host` request is for, where `create_session` for
   * the same project creates new sessions.
   */
  | {
      kind: "host";
      /** Branch new sessions start from. */
      base_branch: string;
      /** Model a new session starts with. */
      default_model: string;
      /**
       * Agent and model a new session of each of `session_modes` starts
       * with, in the same order.
       */
      mode_models?: RemoteModeModel[];
      /** Models a session can switch to now, in display order. */
      models?: RemoteModelOption[];
      /** Display name of the project. */
      project: string;
      /**
       * Identifier of the project, as `list_projects` names it. Absent
       * from older servers.
       */
      project_id?: string;
      /** Modes `create_session` accepts now, in display order. */
      session_modes: RemoteSessionMode[];
    }
  /** The projects the host lists, answering `list_projects`. */
  | {
      kind: "projects";
      /** Listed projects in the host's project switcher's order. */
      projects: RemoteProjectEntry[];
    }
  /** One session snapshot, or `None` when it does not exist. */
  | {
      kind: "session";
      /** Loaded session, boxed to keep other responses small. */
      session: RemoteSession | null;
    }
  /**
   * The files and directories in a session's worktree or the active
   * project's checkout, answering `list_session_files` or
   * `list_project_files`.
   */
  | {
      kind: "session_files";
      /** Listed entries, directories first, then by path. */
      files: RemoteFileEntry[];
      /**
       * Whether the server left out the end of `files` to keep the
       * response small.
       */
      truncated?: boolean;
    }
  /** The review request a publish created or refreshed. */
  | {
      kind: "review_request";
      /** Review request as the forge reported it. */
      review_request: RemoteReviewRequest;
    }
  /** What a `merge_review_request` did on the forge. */
  | {
      kind: "review_request_merge";
      /**
       * Commit the merge created on the target branch, present for
       * `merged` when the forge reports it.
       */
      merge_commit?: string;
      /**
       * Whether the pull request merged, joined the merge queue, or was
       * left unchanged.
       */
      outcome: RemoteReviewRequestMergeOutcome;
      /**
       * Why the forge state no longer allows the merge, present for
       * `refused`.
       */
      reason?: string;
      /**
       * The pull request as the forge reported it after the attempt;
       * absent when that refresh failed. Boxed to keep other responses
       * small.
       */
      review_request?: RemoteReviewRequest;
    }
  /**
   * The sessions of the project a `list_sessions` request is for, in
   * display order.
   */
  | {
      kind: "sessions";
      /**
       * The project the sessions belong to. Absent from hosts that do
       * not report it and from older servers.
       */
      project?: RemoteProject;
      /** Session listing entries. */
      sessions: RemoteSessionSummary[];
    };

/**
 * Outcome of one remote request, serialized as `{"ok": …}` or
 * `{"error": "…"}`.
 */
export type RemoteResult =
  /** The host rejected or failed the operation. */
  | {
      error: string;
    }
  /** The operation succeeded. */
  | {
      ok: RemoteResponse;
    };

/** Whether a linked pull request can merge, as the forge reported it. */
export type RemoteReviewReadiness =
  /** Checks pass and the pull request can merge. */
  | "ready"
  /** The pull request can merge only by bypassing branch protections. */
  | "ready_with_bypass";

/**
 * Pull or merge request linked to a session, as of the host's latest
 * forge refresh.
 */
export type RemoteReviewRequest = {
  /** Forge display id, such as `#123`. */
  display_id: string;
  /** Forge name, such as `GitHub` or `GitLab`. Absent from older servers. */
  forge?: string;
  /** Branch published for review. */
  source_branch: string;
  /** Remote state: `Open`, `Merged`, or `Closed`. */
  state: string;
  /** Condensed forge status, such as `PR ready`. */
  status_summary?: string;
  /** Branch receiving the review request. */
  target_branch: string;
  /** Review-request title. */
  title: string;
  /**
   * Browser URL of the review request. Clients must check its scheme
   * before they link it.
   */
  web_url: string;
};

/** What a `merge_review_request` did on the forge. */
export type RemoteReviewRequestMergeOutcome =
  /** The pull request merged into its target branch. */
  | "merged"
  /**
   * The pull request joined its target branch's merge queue, or is set
   * to join it once the forge's requirements pass.
   */
  | "queued"
  /** The forge state no longer allows the merge, so nothing changed. */
  | "refused";

/**
 * Chat-visible snapshot of one session.
 *
 * The settings, branch, and review-request fields are optional so a page
 * and a server of different versions still understand each other: an older
 * server omits them, and a page hides what is missing.
 */
export type RemoteSession = {
  /** Agent provider, such as `codex`. */
  agent?: string;
  /** Branch the session started from and merges into. */
  base_branch?: string;
  /** Session worktree branch. */
  branch?: string;
  /** Creation timestamp in Unix seconds. */
  created_at?: number;
  /**
   * Messages a draft session has staged and will send together when it
   * starts. Absent when it has none and from older servers.
   */
  draft_prompt?: string;
  /**
   * Latest focused review of the session's changes, while the terminal
   * shows it. Absent when there is none, while a review runs, and from
   * older servers.
   */
  focused_review?: RemoteFocusedReview;
  /**
   * The session's latest graph proposal and, once approved, its graph's
   * progress, as the terminal summarizes them beside the chat. Absent
   * when the session has no proposal and from older servers.
   */
  graph?: RemoteSessionGraph;
  /**
   * Opaque key of the graph work that asks `graph_questions`, which other
   * graph work asking the same questions does not share, so a device can
   * tell a new set from one it closed. It identifies no proposal or node;
   * `answer_questions` echoes it as `graph_question_target` so the
   * answers reach only the work they were entered for. Absent without
   * graph questions and from older servers.
   */
  graph_question_target?: string;
  /**
   * Clarification questions of the session's graph work, its pending
   * graph proposal or one of its graph nodes, while its turn asks none.
   * `answer_questions` naming their `graph_question_target` answers them,
   * and so does a chat message, so the status need not be `Question`.
   * Absent when there are none and from older servers.
   */
  graph_questions?: RemoteQuestion[];
  /** Stable session identifier. */
  id: string;
  /** Whether the session defers its worktree until its first message. */
  is_draft?: boolean;
  /** Durable transcript without hidden agent prompts. */
  messages: RemoteMessage[];
  /** Model name, as `get_host` reports `default_model`. */
  model?: string;
  /**
   * Model identifier, as `get_host` lists it in `models` and
   * `set_session_settings` accepts it.
   */
  model_id?: string;
  /**
   * Live line describing the session's current work, such as
   * `Working... Reading files`, while it works or waits. Further lines add
   * detail. Absent from older servers.
   */
  progress?: string;
  /** Upstream branch the session branch was published to. */
  published_upstream_ref?: string;
  /** Pending clarification questions of the session's turn. */
  questions: RemoteQuestion[];
  /**
   * Labels of the workflow actions waiting behind the active turn, such
   * as a queued sync or publish, in the order they run. Absent when none
   * wait and from older servers.
   */
  queued_actions?: string[];
  /** Chat messages waiting behind the active turn. */
  queued_messages: string[];
  /** Reasoning level, such as `high`. */
  reasoning?: string;
  /** Style of the agent's answers, such as `concise`. */
  response_style?: string;
  /** Linked pull or merge request, when the session has one. */
  review_request?: RemoteReviewRequest;
  /**
   * Speed preference, `normal` or `fast`. Absent when the agent has no
   * speed control.
   */
  speed_mode?: string;
  /** Lifecycle status, such as `InProgress` or `Review`. */
  status: string;
  /** Optional user-visible title. */
  title: string | null;
  /** Last update timestamp in Unix seconds. */
  updated_at: number;
};

/**
 * What a session snapshot says about its latest graph proposal and, once
 * approved, the graph it launched.
 */
export type RemoteSessionGraph = {
  /** Outputs that wait for the user's approval, in declaration order. */
  awaiting?: RemoteGraphApproval[];
  /** Nodes of the launched graph that succeeded. */
  done: number;
  /**
   * Line describing `phase`, or `status` once known, as the terminal
   * shows it.
   */
  label: string;
  /** Where the latest proposal stands. */
  phase: RemoteGraphProposalPhase;
  /** Live status of the approved graph, once Agentty reads it. */
  status?: RemoteGraphStatus;
  /** Nodes of the resolved proposal; zero before it resolves. */
  total: number;
  /** Number of the latest proposal version. */
  version: number;
};

/** How a session created remotely runs. */
export type RemoteSessionMode =
  /** On the in-process harness with its default model. */
  | "harness"
  /** On the project's default agent and model. */
  | "regular";

/**
 * Listing entry for one session.
 *
 * The stats fields are optional: a host that lists only summaries, or an
 * older server, omits them, and a page hides what is missing.
 */
export type RemoteSessionSummary = {
  /** Lines the session's diff adds. */
  added_lines?: number;
  /**
   * Commits the session branch is ahead of its base branch. Absent while
   * the host has not compared them.
   */
  base_ahead?: number;
  /**
   * Commits the session branch is behind its base branch. Absent while
   * the host has not compared them.
   */
  base_behind?: number;
  /** Lines the session's diff removes. */
  deleted_lines?: number;
  /** Whether merging the session branch into its base would conflict. */
  has_merge_conflict?: boolean;
  /** Stable session identifier. */
  id: string;
  /**
   * Start of the current active-work interval in Unix seconds, while the
   * session is running.
   */
  in_progress_started_at?: number;
  /** Active-work seconds completed before the current interval. */
  in_progress_total_seconds?: number;
  /** Whether the session branch was published to an upstream branch. */
  is_published?: boolean;
  /** Model name, as `get_host` reports `default_model`. */
  model?: string;
  /** Compact model name for dense rows, such as `opus-5.5`. */
  model_short_name?: string;
  /** Reasoning level, such as `high`. */
  reasoning?: string;
  /** Forge display id of the linked review request, such as `#123`. */
  review_display_id?: string;
  /** Whether the linked pull request can merge, when the forge says so. */
  review_ready?: RemoteReviewReadiness;
  /** State of the linked review request: `Open`, `Merged`, or `Closed`. */
  review_state?: string;
  /** Host size bucket of the diff, from `XS` to `XXL`. */
  size?: string;
  /** Lifecycle status, such as `InProgress` or `Review`. */
  status: string;
  /** Optional user-visible title. */
  title: string | null;
  /** Last update timestamp in Unix seconds. */
  updated_at: number;
  /**
   * Commits the session branch is ahead of the upstream branch it
   * tracks. Absent when it tracks none or the host has not compared
   * them.
   */
  upstream_ahead?: number;
  /**
   * Commits the session branch is behind the upstream branch it tracks.
   * Absent when it tracks none or the host has not compared them.
   */
  upstream_behind?: number;
};

/** A frame sent by the server. */
export type ServerFrame =
  /**
   * A valid pairing code was accepted and waits for approval in the
   * terminal, which shows the same `confirmation` beside the request.
   */
  | {
      type: "awaiting_approval";
      /** Short code the user compares between the device and terminal. */
      confirmation: string;
    }
  /** Host session state changed after `revision` was last reported. */
  | {
      type: "changed";
      /** Latest host revision. */
      revision: number;
    }
  /**
   * Pairing succeeded; the device's key authenticates its later
   * connections.
   */
  | {
      type: "paired";
      /**
       * Optional requests the server supports, as in
       * `ServerFrame::Welcome`.
       */
      capabilities?: string[];
      /** Identifier of the new device. */
      device_id: string;
      /** Protocol version the server speaks. */
      protocol_version: number;
      /** Host revision at authentication time. */
      revision: number;
    }
  /** The server refused the connection and closes it next. */
  | {
      type: "rejected";
      /** Human-readable reason. */
      reason: string;
    }
  /** Result of one `ClientFrame::Request`. */
  | {
      type: "response";
      /** Identifier from the request. */
      id: number;
      /** Operation outcome. */
      result: RemoteResult;
    }
  /** A paired device authenticated. */
  | {
      type: "welcome";
      /**
       * Optional requests the server supports. A client offers a feature
       * only when its capability is listed, so it never sends a request
       * an older server lacks. Absent when empty.
       */
      capabilities?: string[];
      /** Identifier of the authenticated device. */
      device_id: string;
      /** Protocol version the server speaks. */
      protocol_version: number;
      /** Host revision at authentication time. */
      revision: number;
    };
