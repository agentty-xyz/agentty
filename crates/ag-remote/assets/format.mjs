// Pure helpers of the remote control client: status rules, session
// figures and settings, list grouping and filtering, transcript parsing,
// and unread tracking. Nothing
// here touches the DOM, storage, or the clock, so Node tests cover it
// directly; see crates/ag-remote/tests/web/format.test.mjs.

// Session list groups, in display order. `session_group` in
// crates/agentty/src/domain/session_order.rs owns the same mapping.
export const GROUPS = [
  { id: "merge_queue", label: "Merge queue" },
  { id: "active", label: "Active" },
  { id: "archive", label: "Archive" },
];

// List filters, in display order. `all` keeps every session.
export const FILTERS = [
  { id: "all", label: "All" },
  { id: "needs_you", label: "Needs you" },
  { id: "running", label: "Running" },
];

// What each `ag_session::SessionStatus` means to the client. The action
// flags follow the rules on that type: `review` is
// `allows_review_actions`, `diff` is `allows_diff_view`, `sync` is
// `allows_rebase_action`, and `composer` is `allows_chat_composer`.
// `cancel` lists the statuses the host lets a device cancel.
const STATUSES = {
  Draft: status("Draft", "idle", "active", { cancel: true, composer: true }),
  InProgress: status("In progress", "working", "active", {
    cancel: true,
    composer: true,
    diff: true,
    running: true,
    sync: true,
  }),
  Review: status("Review", "ready", "active", reviewable()),
  AgentReview: status("Agent review", "ready", "active", reviewable()),
  Watching: status("Watching", "ready", "active", reviewable()),
  Question: status("Question", "asking", "active", {
    cancel: true,
    composer: true,
    needsYou: true,
  }),
  Queued: status("Queued", "queue", "merge_queue", { running: true }),
  Rebasing: status("Syncing", "queue", "active", { composer: true, running: true }),
  Merging: status("Merging", "queue", "merge_queue", { running: true }),
  Merged: status("Merged", "done", "active", { diff: true, needsYou: true }),
  Done: status("Done", "done", "archive", { diff: true }),
  Canceled: status("Canceled", "failed", "archive", { diff: true }),
};

function status(label, tone, group, flags) {
  return Object.freeze({
    cancel: false,
    composer: false,
    diff: false,
    group: group,
    label: label,
    needsYou: false,
    review: false,
    running: false,
    sync: false,
    tone: tone,
    ...flags,
  });
}

function reviewable() {
  return {
    cancel: true,
    composer: true,
    diff: true,
    needsYou: true,
    review: true,
    sync: true,
  };
}

// Returns the rules for `statusName`. A status this client does not know,
// from a newer Agentty, shows its raw name in the idle tone and allows
// nothing.
export function statusInfo(statusName) {
  return Object.hasOwn(STATUSES, statusName)
    ? STATUSES[statusName]
    : status(String(statusName), "idle", "active", {});
}

// The line the terminal's session chat ends the transcript with while the
// session works or waits, and whether its marker animates as a loader. The
// host's `progress` replaces the text; these defaults serve older servers.
// `status_message` in crates/agentty/src/domain/session.rs owns the same text.
const STATUS_LINES = {
  InProgress: { isLoading: true, text: "Working..." },
  AgentReview: { isLoading: true, text: "Reviewing changes..." },
  Watching: {
    isLoading: false,
    text: "Watching review request for failing checks, comments, and conflicts...",
  },
  Queued: { isLoading: false, text: "Waiting in merge queue..." },
  Rebasing: { isLoading: true, text: "Rebasing..." },
  Merging: { isLoading: true, text: "Merging..." },
  Merged: { isLoading: true, text: "Waiting for manual local target sync..." },
};

// Returns the transcript's closing status line for `statusName` as
// `{ details, isLoading, text }`, or `null` when the status shows none. A
// non-blank `progress` from the host gives the text in its first line and
// the detail rows in the rest, as the terminal shows them.
export function statusLine(statusName, progress) {
  if (!Object.hasOwn(STATUS_LINES, statusName)) {
    return null;
  }
  const { isLoading, text } = STATUS_LINES[statusName];
  const [heading = "", ...rest] = String(progress ?? "")
    .trim()
    .split("\n")
    .map((row) => row.trim());
  if (!heading) {
    return { details: [], isLoading, text };
  }

  return { details: rest.filter(Boolean), isLoading, text: heading };
}

// Formats how long ago `unixSeconds` was, as seen at `nowSeconds`, in the
// list's short form: `now`, then minutes, hours and days such as `3m`.
export function relativeTime(unixSeconds, nowSeconds) {
  const seconds = Math.max(0, Math.round(nowSeconds - unixSeconds));
  if (seconds < 60) {
    return "now";
  }
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) {
    return minutes + "m";
  }
  const hours = Math.round(seconds / 3600);
  if (hours < 24) {
    return hours + "h";
  }

  return Math.round(seconds / 86400) + "d";
}

// Formats active-work seconds as the terminal's timer does: `12s`,
// `4m 12s`, `1h 2m 3s`.
export function formatTimer(seconds) {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  let label = "";
  if (hours > 0) {
    label += hours + "h ";
  }
  if (minutes > 0) {
    label += minutes + "m ";
  }

  return label + (total % 60) + "s";
}

// Active-work seconds of a listed session at `nowSeconds`: the earlier
// work plus, while a turn runs, its open interval, as the terminal's timer
// counts. `null` when the session never worked or the host does not say.
export function activeSeconds(summary, nowSeconds) {
  const startedAt = summary?.in_progress_started_at;
  const before = Number.isFinite(summary?.in_progress_total_seconds)
    ? summary.in_progress_total_seconds
    : 0;
  if (!Number.isFinite(startedAt)) {
    return before > 0 ? before : null;
  }

  return before + Math.max(0, nowSeconds - startedAt);
}

// How long ago a session created at `createdAt` started, as seen at
// `nowSeconds`, in the terminal's words: `Started just now`, then the two
// largest units, as `Started 12m ago`, `Started 3h 5m ago` or
// `Started 2d 4h ago`. `null` when the host does not say.
// `session_started_label` in crates/agentty/src/ui/session_format.rs owns
// the same text.
export function startedLabel(createdAt, nowSeconds) {
  if (!Number.isFinite(createdAt)) {
    return null;
  }
  const seconds = Math.max(0, Math.floor(nowSeconds - createdAt));
  if (seconds < 60) {
    return "Started just now";
  }
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(seconds / 3600);
  const days = Math.floor(seconds / 86400);
  let age = minutes + "m";
  if (days > 0) {
    age = days + "d " + (hours % 24) + "h";
  } else if (hours > 0) {
    age = hours + "h " + (minutes % 60) + "m";
  }

  return "Started " + age + " ago";
}

// How the list labels a pull request that can merge, as the terminal's
// `[ready]` and `[ready (bypass)]` do.
const PR_READY_LABELS = { ready: "PR ready", ready_with_bypass: "PR ready (bypass)" };
// How the list labels a session branch that conflicts with its base, as
// the terminal's `[merge conflict]` does.
export const MERGE_CONFLICT_LABEL = "merge conflict";

// What a listed session's figures show: the host's size label, the line
// counts (`null` when unknown or both zero), the compact model name,
// whether its branch conflicts with its base, the label of a pull request
// that can merge (`null` when it cannot or the forge does not say), the
// forge indicator, and how far the session branch is from its base and
// from its upstream, as `branchCounts` words them. Older hosts omit every
// figure.
export function sessionFigures(summary) {
  const added = summary?.added_lines;
  const deleted = summary?.deleted_lines;
  const hasLines = Number.isFinite(added) && Number.isFinite(deleted) && added + deleted > 0;
  const readiness = summary?.review_ready;

  return {
    base: branchCounts(summary?.base_ahead, summary?.base_behind),
    forge: forgeIndicator(summary),
    hasConflict: summary?.has_merge_conflict === true,
    lines: hasLines ? { added: added, deleted: deleted } : null,
    model: text(summary?.model_short_name) || text(summary?.model),
    prReady: Object.hasOwn(PR_READY_LABELS, readiness) ? PR_READY_LABELS[readiness] : null,
    size: text(summary?.size),
    upstream: branchCounts(summary?.upstream_ahead, summary?.upstream_behind),
  };
}

// The terminal's marks for each state of a linked review request, with
// the tone each shows in.
const FORGE_STATES = {
  Closed: { label: "closed", mark: "✗", tone: "closed" },
  Merged: { label: "merged", mark: "✓", tone: "merged" },
  Open: { label: "open", mark: "⊙", tone: "open" },
};

// The forge indicator the terminal's status column shows for a listed
// session, as `{ label, mark, text, tone }`: `⊙ #12`, `✓ #12` or `✗ #12`
// for an open, merged or closed review request, or `↑` for a branch
// published without one; `mark` is the symbol alone, for a narrow place.
// `null` when neither applies, or when a newer host reports a review state
// this page does not know. Older hosts that send the state without the
// display id get the mark alone.
export function forgeIndicator(summary) {
  const state = summary?.review_state;
  if (Object.hasOwn(FORGE_STATES, state)) {
    const { label, mark, tone } = FORGE_STATES[state];
    const id = text(summary.review_display_id);

    return {
      label: "Review request " + (id ? id + " " : "") + label,
      mark: mark,
      text: id ? mark + " " + id : mark,
      tone: tone,
    };
  }
  if (!state && summary?.is_published === true) {
    return { label: "Branch published", mark: "↑", text: "↑", tone: "published" };
  }

  return null;
}

// How far one branch is from another, as the terminal's footer shows it:
// `✓` when even, or `↓2 ↑1` for two commits behind and one ahead. `null`
// when either count is unknown.
export function branchCounts(ahead, behind) {
  if (count(ahead) === null || count(behind) === null) {
    return null;
  }
  if (ahead === 0 && behind === 0) {
    return "✓";
  }

  return "↓" + behind + " ↑" + ahead;
}

// The project a listing names, with the fields the sidebar shows, or
// `null` when the host does not report it. `ahead` and `behind` are `null`
// until the host has compared the branch with its upstream, and `id` is
// `null` when the server does not say which project it listed.
export function parseProject(listed) {
  const project = listed?.project;
  const name = text(project?.name);
  if (!name) {
    return null;
  }

  return {
    ahead: count(project.ahead),
    behind: count(project.behind),
    branch: text(project.branch),
    id: text(project.id),
    name: name,
    sync: parseProjectSync(project.sync),
  };
}

// The phases of a project sync, with the tone of each one's line and
// whether the sync still runs.
const SYNC_PHASES = {
  blocked: { isRunning: false, tone: "asking" },
  complete: { isRunning: false, tone: "ready" },
  failed: { isRunning: false, tone: "failed" },
  resolving_conflicts: { isRunning: true, tone: "working" },
  running: { isRunning: true, tone: "working" },
};

// A project's latest sync, or `null` when it has none or a newer host
// reports a phase this page does not know.
function parseProjectSync(sync) {
  const phase = sync?.phase;
  if (!Object.hasOwn(SYNC_PHASES, phase)) {
    return null;
  }

  return {
    branch: text(sync.branch),
    conflicts: count(sync.conflicts),
    isRunning: SYNC_PHASES[phase].isRunning,
    message: text(sync.message),
    phase: phase,
    pulled: count(sync.pulled_commits),
    pushed: count(sync.pushed_commits),
    tone: SYNC_PHASES[phase].tone,
  };
}

// How far the branch of `project` is from its upstream, as `branchCounts`
// words it. `null` when the project has no branch or the host has not
// compared it.
export function projectCounts(project) {
  return project?.branch ? branchCounts(project.ahead, project.behind) : null;
}

// What the sync of `project` is called: the project and the branch it
// updates, such as `agentty/main`.
export function projectSyncTarget(project) {
  const branch = project?.sync?.branch ?? project?.branch;

  return branch ? project.name + "/" + branch : (project?.name ?? "");
}

// The line that reports the latest sync of `project`, as the terminal's
// status bar words it, or `null` when the project has none.
export function projectSyncLine(project) {
  const sync = project?.sync;
  if (!sync) {
    return null;
  }
  const target = projectSyncTarget(project);
  switch (sync.phase) {
    case "running":
      return "Syncing " + target + "...";
    case "resolving_conflicts":
      return "Resolving " + countLabel(sync.conflicts ?? 0, "conflict") + " for " + target + "...";
    case "complete": {
      const parts = [];
      if (sync.pulled !== null) {
        parts.push(sync.pulled + " pulled");
      }
      if (sync.pushed !== null) {
        parts.push(sync.pushed + " pushed");
      }
      if (sync.conflicts) {
        parts.push(countLabel(sync.conflicts, "conflict") + " resolved");
      }

      return "Synced " + target + (parts.length > 0 ? ": " + parts.join(", ") : "");
    }
    case "blocked":
      return "Sync blocked for " + target + (sync.message ? ": " + sync.message : "");
    default:
      return "Sync failed for " + target + (sync.message ? ": " + sync.message : "");
  }
}

// `1 conflict`, `2 conflicts`.
function countLabel(value, singular) {
  return value + " " + singular + (value === 1 ? "" : "s");
}

// `value` when it is a whole number of zero or more, otherwise `null`.
function count(value) {
  return Number.isInteger(value) && value >= 0 ? value : null;
}

// The model a session runs, as `claude/opus-5.5`: the agent, then the
// compact name the listing gives for the same model, or the model id.
// `null` when the host names no model.
export function modelLabel(session, summary = null) {
  const model = text(session?.model);
  if (!model) {
    return null;
  }
  const short = summary?.model === model ? text(summary.model_short_name) : null;
  const agent = text(session.agent);

  return short && agent ? agent + "/" + short : model;
}

// The session settings the composer line shows, in order: the model with
// its reasoning level, the answer style, and the speed. Missing ones are
// left out.
export function settingsParts(session, summary = null) {
  const parts = [];
  const model = modelLabel(session, summary);
  const reasoning = text(session?.reasoning);
  if (model) {
    parts.push({ label: model, reasoning: reasoning, type: "model" });
  }
  const style = text(session?.response_style);
  if (style) {
    parts.push({ label: style, type: "style" });
  }
  const speed = text(session?.speed_mode);
  if (speed) {
    parts.push({ label: speed + " speed", type: "speed" });
  }

  return parts;
}

// How the header names a linked review request: `#1261 open · PR ready`.
export function reviewLabel(reviewRequest) {
  const id = text(reviewRequest?.display_id);
  if (!id) {
    return null;
  }
  const state = text(reviewRequest.state);
  const summary = text(reviewRequest.status_summary);

  return [id + (state ? " " + state.toLowerCase() : ""), summary].filter(Boolean).join(" · ");
}

// `value` when it is a non-empty string, otherwise `null`.
function text(value) {
  return typeof value === "string" && value !== "" ? value : null;
}

// Splits `sessions` into the populated groups of `GROUPS`, in that order,
// keeping the host's order inside each group.
export function groupSessions(sessions) {
  return GROUPS.map((group) => ({
    ...group,
    sessions: sessions.filter((session) => statusInfo(session.status).group === group.id),
  })).filter((group) => group.sessions.length > 0);
}

// Keeps the sessions whose title contains `query`, ignoring case, and that
// pass `filter`, one of the ids in `FILTERS`.
export function filterSessions(sessions, { filter = "all", query = "" } = {}) {
  return matchingQuery(sessions, query).filter((session) => passesFilter(session, filter));
}

// Counts, for each filter in `FILTERS`, the sessions that match `query`
// and pass it.
export function filterCounts(sessions, query = "") {
  const matching = matchingQuery(sessions, query);

  return Object.fromEntries(
    FILTERS.map((filter) => [
      filter.id,
      matching.filter((session) => passesFilter(session, filter.id)).length,
    ]),
  );
}

function matchingQuery(sessions, query) {
  const needle = query.trim().toLowerCase();

  return sessions.filter((session) => (session.title || "").toLowerCase().includes(needle));
}

function passesFilter(session, filter) {
  const info = statusInfo(session.status);
  switch (filter) {
    case "needs_you":
      return info.needsYou;
    case "running":
      return info.running;
    default:
      return true;
  }
}

// A transcript notice starts with a label such as `[Commit Error]`; see
// `TranscriptNotice::prefix` in crates/ag-session/src/transcript_notice.rs.
const NOTICE_LABEL = /^\[([A-Za-z][A-Za-z -]{0,40})\]/;

// Splits a notice into its label, the tone the label implies, and the rest.
// A notice without a label has a `null` label and the `plain` tone.
export function parseNotice(content) {
  const text = content.trim();
  const match = NOTICE_LABEL.exec(text);
  if (!match) {
    return { body: text, label: null, tone: "plain" };
  }
  const label = match[1].trim();
  let tone = "info";
  if (label.endsWith("Error")) {
    tone = "error";
  } else if (label.endsWith("Warning")) {
    tone = "warning";
  }

  return { body: text.slice(match[0].length).trim(), label: label, tone: tone };
}

// Splits an activity summary into its non-blank lines, each with the tone
// its first character implies, as `activity_line_style` does in the
// terminal (crates/agentty/src/ui/component/session_output.rs).
export function parseActivity(content) {
  return content
    .split("\n")
    .map((line) => line.trimEnd())
    .filter((line) => line.length > 0)
    .map((line) => ({ text: line, tone: activityTone(line) }));
}

// The first line of a turn summary in the layout this page knows, and the
// heading, glyph, and note prefix its rows take their tones from, as
// `TurnSummary` writes them (crates/agentty/src/domain/turn_summary.rs).
const TURN_SUMMARY_MARKER = "turn-summary v1";
const SUMMARY_FAILED_HEADING = "Failed";
const SUMMARY_FAILURE_GLYPH = "✗";
const SUMMARY_RECOVERY_PREFIX = "→ ";

// Splits a finished turn's summary into its header, its rows, and its
// closing `─ Worked` line, or returns `null` for other activity text, such
// as an older `Tools:` footer or a summary layout this page does not know.
// A row has five tab-separated columns (heading, glyph, target, result,
// note) or two (heading, text); a row with an empty heading continues the
// section above it.
export function parseTurnSummary(content) {
  const lines = content.split("\n");
  if (lines[0].trim() !== TURN_SUMMARY_MARKER) {
    return null;
  }
  const body = lines.slice(1).filter((line) => line.trim().length > 0);
  const divider = (body.pop() ?? "").trim();
  const header = (body.shift() ?? "").trim();
  let section = "";
  const rows = body.map((line) => {
    const columns = line.split("\t");
    section = columns[0] || section;
    if (columns.length !== 5) {
      return { heading: columns[0], text: columns.slice(1).join(" ") };
    }
    const [heading, glyph, target, result, note] = columns;

    return {
      glyph: glyph,
      glyphTone: glyph === SUMMARY_FAILURE_GLYPH ? "error" : "success",
      heading: heading,
      note: note,
      noteTone: note.startsWith(SUMMARY_RECOVERY_PREFIX) ? "success" : "muted",
      resultParts: summaryResultParts(result, section),
      target: target,
    };
  });

  return { divider: divider, header: header, rows: rows };
}

// Splits a summary row's result into toned parts as the terminal colours
// it: a failure is one error part, and elsewhere added lines (`+N`) are
// green and removed ones (`−N`) red.
function summaryResultParts(result, section) {
  if (section === SUMMARY_FAILED_HEADING) {
    return [{ text: result, tone: "error" }];
  }

  return result
    .split(" ")
    .filter((word) => word.length > 0)
    .map((word) => {
      let tone = "muted";
      if (word.startsWith("+")) {
        tone = "success";
      } else if (word.startsWith("−")) {
        tone = "error";
      }

      return { text: word, tone: tone };
    });
}

function activityTone(line) {
  if (line.startsWith("✓")) {
    return "success";
  }
  if (line.startsWith("✗")) {
    return "error";
  }
  if (line.startsWith("!")) {
    return "warning";
  }
  if (line.startsWith("…")) {
    return "info";
  }

  return "muted";
}

// Reads the stored map from session id to the `updated_at` last seen, or
// `null` when there is none or it cannot be read.
export function parseSeen(text) {
  if (!text) {
    return null;
  }
  let seen = null;
  try {
    seen = JSON.parse(text);
  } catch (_error) {
    // A damaged value counts as no map: the caller then marks every
    // session seen, as on a first load.
  }

  return seen && typeof seen === "object" && !Array.isArray(seen) ? seen : null;
}

// Most sessions the seen map remembers. Ids that are not listed stay, so
// switching the terminal to another project and back, or a session that
// drops out of the listing for a while, does not mark them unread.
export const MAX_SEEN = 500;

// Combines two seen maps, as two tabs of this page write them, keeping the
// later time of each session. Either may be `null`; both `null` gives
// `null`.
export function mergeSeen(first, second) {
  if (!first && !second) {
    return null;
  }
  const merged = {};
  for (const map of [first, second]) {
    for (const [id, seenAt] of Object.entries(map || {})) {
      if (typeof seenAt === "number" && !(Object.hasOwn(merged, id) && merged[id] >= seenAt)) {
        merged[id] = seenAt;
      }
    }
  }

  return merged;
}

// Returns the seen map to store after `sessions` arrived while
// `openSessionId` is open. Without a map, as on the first load, or with
// one that knows none of the listed sessions, as after pairing with
// another Agentty, every listed session counts as seen. Ids no longer
// listed stay until the map exceeds `MAX_SEEN`; then the most recently
// updated ones are kept, and listed ones always.
export function reconcileSeen(seen, sessions, openSessionId) {
  const isFirstLoad =
    !seen || (sessions.length > 0 && !sessions.some((session) => Object.hasOwn(seen, session.id)));
  const next = mergeSeen(seen, {}) || {};
  for (const session of sessions) {
    if (isFirstLoad || session.id === openSessionId) {
      next[session.id] = session.updated_at;
    }
  }

  return pruneSeen(next, sessions);
}

function pruneSeen(seen, sessions) {
  const ids = Object.keys(seen);
  if (ids.length <= MAX_SEEN) {
    return seen;
  }
  const listed = new Set(sessions.map((session) => session.id));
  const kept = ids
    .filter((id) => !listed.has(id))
    .sort((first, second) => seen[second] - seen[first])
    .slice(0, Math.max(0, MAX_SEEN - listed.size));
  const pruned = {};
  for (const id of [...listed, ...kept]) {
    if (Object.hasOwn(seen, id)) {
      pruned[id] = seen[id];
    }
  }

  return pruned;
}

// Returns the ids of the sessions updated since they were last seen, other
// than the open one. A session missing from `seen` appeared since then and
// is unread too. Without a map, as when storage fails, nothing is unread.
export function unreadIds(sessions, seen, openSessionId) {
  const unread = new Set();
  if (!seen) {
    return unread;
  }
  for (const session of sessions) {
    if (session.id === openSessionId) {
      continue;
    }
    const seenAt = Object.hasOwn(seen, session.id) ? seen[session.id] : null;
    if (seenAt === null || session.updated_at > seenAt) {
      unread.add(session.id);
    }
  }

  return unread;
}
