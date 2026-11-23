// `@` file lookup of the remote control client's composer and of the
// typed answers to the agent's questions. As in the
// terminal, typing `@` at the start of a word lists the files and
// directories of the open session's worktree, or for a session yet to
// start those of the active project's checkout, ranked by a fuzzy match
// of what follows; picking one puts `@path ` in its place. The names come
// from `list_session_files`, offered through the `files` capability, or
// `list_project_files`, offered through `project_files`, and only ever
// become text nodes, through suggest.mjs. The ranking follows
// `filter_entries` in crates/agentty/src/domain/file_entry.rs and the
// token rules follow `extract_at_mention_query` in
// crates/agentty/src/domain/input.rs. The Node tests in
// crates/ag-remote/tests/web/mention.test.mjs pass the fake DOM.

// Most matches the list shows; typing more narrows it.
export const MAX_MENTION_ITEMS = 50;

// Names this lookup's lists in the popup it shares with the slash
// commands.
const OWNER = "mention";

// Characters that may come right before `@` besides whitespace.
const OPENING_DELIMITERS = new Set(["(", "[", "{"]);
// Characters that end a fuzzy-match segment.
const SEGMENT_BOUNDARIES = new Set(["/", ".", "_", "-"]);
// A character that can be part of an `@` token.
const TOKEN_CHARACTER = /^[\p{L}\p{N}/._-]$/u;

// Whether the host lets this device look up the files of `scope`: a
// session's worktree, or with `"project"` the active project's checkout.
export function offersMentions(capabilities, scope = "session") {
  return capabilities.includes(scope === "project" ? "project_files" : "files");
}

// The `@` token the caret sits in, as `{ start, end, query }`: `start` is
// the index of `@`, `end` the end of the whole token, including what
// follows the caret, and `query` the text between `@` and the caret.
// `null` when the caret is not in a token or text is selected.
export function mentionQuery(text, selectionStart = text.length, selectionEnd = selectionStart) {
  if (selectionStart !== selectionEnd) {
    return null;
  }
  const start = tokenStart(text, selectionStart);
  if (start === -1) {
    return null;
  }
  let end = selectionStart;
  while (end < text.length && TOKEN_CHARACTER.test(text[end])) {
    end += 1;
  }

  return { end: end, query: text.slice(start + 1, selectionStart), start: start };
}

// The index of the `@` that starts a token before `caret` in `text`, or
// `-1` when whitespace comes first or the `@` sits inside a word.
function tokenStart(text, caret) {
  let index = caret - 1;
  while (index >= 0 && text[index] !== "@" && !/\s/.test(text[index])) {
    index -= 1;
  }
  if (index < 0 || text[index] !== "@") {
    return -1;
  }
  const previous = text[index - 1];

  return previous === undefined || /\s/.test(previous) || OPENING_DELIMITERS.has(previous) ? index : -1;
}

// The `list_session_files` or `list_project_files` entries in a result,
// as `{ path, isDir }`.
export function parseEntries(result) {
  if (result?.kind !== "session_files" || !Array.isArray(result.files)) {
    return [];
  }

  return result.files
    .filter((entry) => typeof entry?.path === "string" && entry.path !== "")
    .map((entry) => ({ isDir: entry.is_dir === true, path: entry.path }));
}

// `entries` that contain the characters of `query` in order, ignoring
// case, best match first. Runs of characters, matches that start a path
// segment, and a match in the file name rank higher; equal matches prefer
// shallower, then shorter, paths. A query ending in `/` lists directories
// first. An empty query keeps every entry in its order.
export function filterEntries(entries, query) {
  if (query === "") {
    return entries.slice();
  }
  const lowerQuery = query.toLowerCase();
  const queryCharacters = Array.from(lowerQuery);
  const scored = [];
  for (const entry of entries) {
    const score = fuzzyScore(entry.path, queryCharacters, entry.isDir);
    if (score !== null) {
      scored.push({
        depth: entry.path.split("/").length - 1,
        entry: entry,
        score: score + basenameBonus(entry.path, lowerQuery),
      });
    }
  }
  scored.sort(
    (first, second) =>
      second.score - first.score ||
      first.depth - second.depth ||
      first.entry.path.length - second.entry.path.length ||
      compareText(first.entry.path, second.entry.path),
  );
  const matches = scored.map((item) => item.entry);

  return query.endsWith("/")
    ? [...matches.filter((entry) => entry.isDir), ...matches.filter((entry) => !entry.isDir)]
    : matches;
}

// What picking `entry` puts in the message: `@path ` for a file and
// `@path/ ` for a directory.
export function mentionText(entry) {
  return "@" + entry.path + (entry.isDir ? "/ " : " ");
}

// Wires the `@` lookup of text field `input` to popup `suggestions` from
// suggest.mjs. `context()` returns the open `session`, the host's
// `capabilities` and `isConnected`, and `fileScope`: `"session"`, the
// default, for the session's worktree, `"project"` for the project
// checkout, or `null` for no lookup; with `"project"`, `projectKey` names
// the Agentty whose project it is when a page shows several;
// `request(body)` sends one request and
// resolves with its result; `setText(text, caret)` replaces the field's
// text and its saved draft and puts the caret at `caret`;
// `showError(message)` shows a failure.
//
// Returns `update()`, for the field's input, caret and focus changes,
// which opens, filters or closes the list and says whether it shows;
// `refresh()` for a newer snapshot of the session; `close()`; and
// `reset()` for another session.
export function createMentions({ input, suggestions, request, context, setText, showError }) {
  const state = {
    // Text the user closed the list on with `Escape`; it stays closed
    // until the text changes.
    dismissed: null,
    entries: [],
    // The key of the source the entries belong to.
    entriesFor: null,
    // Whether a file load is on its way.
    isLoading: false,
    // Whether the user is in a lookup, so a load that finishes may show
    // its list; closing, `Escape` and leaving the token end it.
    isWanted: false,
    // Counts file loads, so an answer for an older one is dropped.
    loads: 0,
  };

  // Where this device may look up files now, as `{ body, key }`: the
  // request that lists them and the key their entries are kept under;
  // `null` when it may not.
  function mentionSource() {
    const {
      capabilities = [],
      fileScope = "session",
      isConnected = false,
      projectKey = "",
      session = null,
    } = context() ?? {};
    if (!isConnected || !fileScope || !offersMentions(capabilities, fileScope)) {
      return null;
    }
    if (fileScope === "project") {
      return { body: { method: "list_project_files" }, key: "project:" + projectKey };
    }

    return session
      ? { body: { method: "list_session_files", session_id: session.id }, key: "session:" + session.id }
      : null;
  }

  function isShown() {
    return suggestions.isOpen(OWNER);
  }

  function close() {
    state.isWanted = false;
    suggestions.close(OWNER);
  }

  // Reads the files of `source` again, keeping the earlier ones on
  // screen until the answer arrives.
  async function load(source) {
    const loading = ++state.loads;
    state.isLoading = true;
    try {
      const entries = parseEntries(await request(source.body));
      if (loading === state.loads) {
        state.entries = entries;
        state.entriesFor = source.key;
      }
    } catch (error) {
      if (loading === state.loads) {
        showError("Could not list the files: " + error.message);
      }
    }
    if (loading === state.loads) {
      state.isLoading = false;
      if (state.isWanted) {
        show();
      }
    }
  }

  function show() {
    const token = mentionQuery(input.value, input.selectionStart, input.selectionEnd);
    const source = mentionSource();
    if (!token || !source || state.entriesFor !== source.key) {
      suggestions.close(OWNER);

      return;
    }
    const matches = new Map(
      filterEntries(state.entries, token.query)
        .slice(0, MAX_MENTION_ITEMS)
        .map((entry) => [entry.path + (entry.isDir ? "/" : ""), entry]),
    );
    suggestions.show({
      items: Array.from(matches.keys(), (key) => ({ detail: "", key: key, label: key })),
      onDismiss: () => {
        state.isWanted = false;
        state.dismissed = input.value;
      },
      onPick: (item) => pick(token, matches.get(item.key)),
      owner: OWNER,
      title: "Files",
    });
  }

  function pick(token, entry) {
    const text = input.value;
    const replacement = mentionText(entry);
    close();
    setText(text.slice(0, token.start) + replacement + text.slice(token.end), token.start + replacement.length);
  }

  function update() {
    const source = mentionSource();
    const token = source ? mentionQuery(input.value, input.selectionStart, input.selectionEnd) : null;
    if (!token || (state.dismissed !== null && input.value === state.dismissed)) {
      close();

      return false;
    }
    state.dismissed = null;
    // A new lookup reads the files again, as they change while the agent
    // works.
    if (!state.isWanted && !state.isLoading) {
      void load(source);
    }
    state.isWanted = true;
    show();

    return isShown();
  }

  // Shows the list again for a newer snapshot, or closes it once the
  // composer no longer allows it.
  function refresh() {
    if (!mentionSource()) {
      close();
    } else if (isShown()) {
      show();
    }
  }

  function reset() {
    close();
    state.dismissed = null;
    state.entries = [];
    state.entriesFor = null;
    state.isLoading = false;
    state.loads += 1;
  }

  return { close, refresh, reset, update };
}

// Scores a fuzzy match of `queryCharacters` in `path`, treating a
// directory as if its path ended with `/`, or returns `null` when the
// characters do not all appear in order.
function fuzzyScore(path, queryCharacters, isDir) {
  const characters = Array.from(path);
  if (isDir) {
    characters.push("/");
  }
  const state = { previousCharacter: null, previousMatched: false, queryCharacters, queryIndex: 0, score: 0 };
  for (const original of characters) {
    if (state.queryIndex >= queryCharacters.length) {
      break;
    }
    state.previousMatched = scoreCharacter(state, original);
    state.previousCharacter = original;
  }

  return state.queryIndex >= queryCharacters.length ? state.score : null;
}

// Scores one path character of a fuzzy match in `state`, each character
// of its lowercase form against the next query character, and returns
// whether the last one matched. A match scores one, three more right
// after another match, and five more at the start of a segment.
function scoreCharacter(state, original) {
  let matched = false;
  for (const character of Array.from(original.toLowerCase())) {
    if (state.queryIndex >= state.queryCharacters.length) {
      break;
    }
    matched = character === state.queryCharacters[state.queryIndex];
    if (matched) {
      const startsSegment =
        state.previousCharacter === null || SEGMENT_BOUNDARIES.has(state.previousCharacter);
      state.score += 1 + (state.previousMatched ? 3 : 0) + (startsSegment ? 5 : 0);
      state.queryIndex += 1;
    }
    state.previousMatched = matched;
  }

  return matched;
}

// Extra score when `query` is in the file name: the whole name before its
// first `.`, its start, or anywhere in it.
function basenameBonus(path, query) {
  if (query.includes("/")) {
    return 0;
  }
  let end = path.length;
  while (end > 0 && path[end - 1] === "/") {
    end -= 1;
  }
  const name = path.slice(0, end).split("/").pop().toLowerCase();
  if (name.split(".")[0] === query) {
    return 60;
  }
  if (name.startsWith(query)) {
    return 45;
  }

  return name.includes(query) ? 30 : 0;
}

// Orders text by code unit, as Rust orders paths by bytes for ASCII.
function compareText(first, second) {
  return Number(first > second) - Number(first < second);
}
