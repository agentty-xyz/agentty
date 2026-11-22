// Pure helpers of the remote control client: status rules, session list
// grouping and filtering, transcript parsing, and unread tracking. Nothing
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

// Returns the seen map to store after `sessions` arrived while
// `openSessionId` is open. Without a map, as on the first load, every
// session counts as seen. Sessions no longer listed are dropped.
export function reconcileSeen(seen, sessions, openSessionId) {
  const next = {};
  for (const session of sessions) {
    if (!seen || session.id === openSessionId) {
      next[session.id] = session.updated_at;
    } else if (Object.hasOwn(seen, session.id)) {
      next[session.id] = seen[session.id];
    }
  }

  return next;
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
