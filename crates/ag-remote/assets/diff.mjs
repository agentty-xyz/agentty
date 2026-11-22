// Read-only diff of the open session: the changed files, each with its
// status letter, path and line counts, and its unified patch once opened.
// Paths and patch lines are untrusted and only ever become text nodes.
// Functions take `document` as a parameter, so the Node tests in
// crates/ag-remote/tests/web/diff.test.mjs pass the fake DOM.

// The letter a file's status shows, as `git status --short` does.
const STATUS_LETTERS = { added: "A", deleted: "D", modified: "M", renamed: "R" };

// The letter of diff file status `status`; a status from a newer host shows
// as `?`.
export function statusLetter(status) {
  return Object.hasOwn(STATUS_LETTERS, status) ? STATUS_LETTERS[status] : "?";
}

// How `file` is named: its path, or `old → new` after a rename.
export function filePath(file) {
  const path = String(file?.path ?? "");
  const oldPath = typeof file?.old_path === "string" ? file.old_path : "";

  return oldPath && oldPath !== path ? oldPath + " → " + path : path;
}

// Splits a unified patch into lines, each with the kind that tints it:
// `add` and `del` for changed lines, `hunk` for `@@` headers, `meta` for
// the file headers, and `context` for the rest. A trailing newline adds no
// empty line.
export function patchLines(patch) {
  const text = String(patch ?? "");
  if (text === "") {
    return [];
  }
  const lines = text.endsWith("\n") ? text.slice(0, -1).split("\n") : text.split("\n");
  let isHeader = true;

  return lines.map((line) => {
    if (line.startsWith("@@")) {
      isHeader = false;

      return { kind: "hunk", text: line };
    }
    if (line.startsWith("diff --git ")) {
      isHeader = true;
    }
    if (isHeader) {
      return { kind: "meta", text: line };
    }
    if (line.startsWith("+")) {
      return { kind: "add", text: line };
    }
    if (line.startsWith("-")) {
      return { kind: "del", text: line };
    }

    return { kind: line.startsWith("\\") ? "meta" : "context", text: line };
  });
}

// The changed lines over `files`, as `{ added, deleted }`.
export function diffTotals(files) {
  return files.reduce(
    (totals, file) => ({
      added: totals.added + (Number(file.added_lines) || 0),
      deleted: totals.deleted + (Number(file.deleted_lines) || 0),
    }),
    { added: 0, deleted: 0 },
  );
}

// Wires the diff view. `elements` holds `view` (the section that replaces
// the transcript), `summary` (the line above the files), `files` (the
// list) and `error`. `request(body)` sends one request and resolves with
// its result.
//
// Returns `open(sessionId)`, which loads the file list, `refresh()`, which
// loads it again and the patches the reader opened, `close()`, and
// `isOpen()`.
export function createDiff({ document, elements, request }) {
  const state = { loads: 0, sessionId: null };

  async function load() {
    const sessionId = state.sessionId;
    const load = ++state.loads;
    let result;
    try {
      result = await request({ method: "get_session_diff", session_id: sessionId });
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
    const files = Array.isArray(result?.files) ? result.files : [];
    renderSummary(files, Boolean(result?.truncated));
    const opened = new Set(
      [...elements.files.querySelectorAll("details")]
        .filter((details) => details.open)
        .map((details) => details.dataset.path),
    );
    elements.files.replaceChildren(...files.map((file) => renderFile(file, opened.has(file.path))));
  }

  function renderSummary(files, isTruncated) {
    if (files.length === 0) {
      elements.summary.replaceChildren("No changes yet.");

      return;
    }
    const totals = diffTotals(files);
    const lines = element(document, "span", "lines");
    lines.append(
      element(document, "span", "add", "+" + totals.added),
      " ",
      element(document, "span", "del", "-" + totals.deleted),
    );
    elements.summary.replaceChildren(
      (files.length === 1 ? "1 file" : files.length + " files") + (isTruncated ? " shown" : ""),
      " · ",
      lines,
    );
    if (isTruncated) {
      elements.summary.append(" · the rest is too large to list here");
    }
  }

  function renderFile(file, isOpen) {
    const item = element(document, "li", "diff-file");
    const details = element(document, "details");
    details.dataset.path = String(file.path ?? "");
    const summary = element(document, "summary");
    const letter = element(document, "span", "diff-status diff-status--" + statusLetter(file.status), statusLetter(file.status));
    letter.title = String(file.status ?? "");
    const path = element(document, "span", "diff-path", filePath(file));
    path.title = filePath(file);
    const lines = element(document, "span", "lines");
    lines.append(
      element(document, "span", "add", "+" + (Number(file.added_lines) || 0)),
      " ",
      element(document, "span", "del", "-" + (Number(file.deleted_lines) || 0)),
    );
    summary.append(letter, path, lines);
    const body = element(document, "div", "diff-body");
    details.append(summary, body);
    details.addEventListener("toggle", () => {
      if (details.open && !body.dataset.loaded) {
        void loadPatch(details, body);
      }
    });
    item.append(details);
    if (isOpen) {
      details.open = true;
      void loadPatch(details, body);
    }

    return item;
  }

  async function loadPatch(details, body) {
    const sessionId = state.sessionId;
    body.dataset.loaded = "true";
    body.replaceChildren(element(document, "p", "diff-note", "Loading…"));
    let result;
    try {
      result = await request({
        method: "get_session_diff",
        path: details.dataset.path,
        session_id: sessionId,
      });
    } catch (error) {
      delete body.dataset.loaded;
      body.replaceChildren(element(document, "p", "error", error.message));

      return;
    }
    if (sessionId !== state.sessionId) {
      return;
    }
    body.replaceChildren(renderPatch(result?.patch));
    if (result?.truncated) {
      body.append(element(document, "p", "diff-note", "The rest of this patch is too large to show here."));
    }
  }

  function renderPatch(patch) {
    const lines = patchLines(patch);
    if (lines.length === 0) {
      return element(document, "p", "diff-note", "No text changes to show.");
    }
    const pre = element(document, "pre", "patch");
    pre.append(...lines.map((line) => element(document, "span", "patch-line patch-line--" + line.kind, line.text)));

    return pre;
  }

  return {
    close: () => {
      state.sessionId = null;
      state.loads += 1;
      elements.view.hidden = true;
      elements.files.replaceChildren();
      elements.summary.replaceChildren();
      elements.error.textContent = "";
    },
    isOpen: () => state.sessionId !== null,
    open: (sessionId) => {
      state.sessionId = sessionId;
      elements.view.hidden = false;
      elements.files.replaceChildren();
      elements.summary.replaceChildren("Loading the changes…");

      return load();
    },
    refresh: () => (state.sessionId ? load() : Promise.resolve()),
  };
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
