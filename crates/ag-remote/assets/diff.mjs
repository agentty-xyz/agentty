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

// What identifies one listed file's change: when it differs, the file's
// row and an opened patch need loading again.
export function fileKey(file) {
  return JSON.stringify([
    String(file?.path ?? ""),
    file?.old_path ?? null,
    file?.status ?? null,
    Number(file?.added_lines) || 0,
    Number(file?.deleted_lines) || 0,
  ]);
}

// Wires the diff view. `elements` holds `view` (the section that replaces
// the transcript), `summary` (the line above the files), `files` (the
// list) and `error`. `request(body)` sends one request and resolves with
// its result.
//
// Returns `open(sessionId)`, which loads the file list, `refresh()`, which
// loads it again, `close()`, and `isOpen()`. A refresh changes only the
// rows whose file changed, so the reader keeps their place and focus, and
// an opened patch stays on screen until its new text arrives.
export function createDiff({ document, elements, request }) {
  const state = { listKey: null, loads: 0, sessionId: null };

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
    const isTruncated = Boolean(result?.truncated);
    const listKey = JSON.stringify([isTruncated, ...files.map(fileKey)]);
    if (listKey === state.listKey) {
      return;
    }
    state.listKey = listKey;
    renderSummary(files, isTruncated);
    renderFiles(files);
  }

  function renderSummary(files, isTruncated) {
    if (files.length === 0) {
      elements.summary.replaceChildren("No changes yet.");

      return;
    }
    const totals = diffTotals(files);
    elements.summary.replaceChildren(
      (files.length === 1 ? "1 file" : files.length + " files") + (isTruncated ? " shown" : ""),
      " · ",
      lineCounts(totals.added, totals.deleted),
    );
    if (isTruncated) {
      elements.summary.append(" · the rest is too large to list here");
    }
  }

  // Keeps the row of each file that is still listed, rebuilds its heading
  // only when its change differs, and moves a row only when the order
  // changed.
  function renderFiles(files) {
    const rows = new Map(
      [...elements.files.children].map((row) => [row.dataset.path, row]),
    );
    const wanted = files.map((file) => {
      const path = String(file.path ?? "");
      const row = rows.get(path);
      rows.delete(path);
      if (!row) {
        return renderFile(file);
      }
      if (row.dataset.key !== fileKey(file)) {
        updateFile(row, file);
      }

      return row;
    });
    for (const stale of rows.values()) {
      stale.remove();
    }
    wanted.forEach((row, index) => {
      const current = elements.files.children[index];
      if (current !== row) {
        elements.files.insertBefore(row, current ?? null);
      }
    });
  }

  function renderFile(file) {
    const item = element(document, "li", "diff-file");
    item.dataset.path = String(file.path ?? "");
    const details = element(document, "details");
    const summary = element(document, "summary");
    const body = element(document, "div", "diff-body");
    details.append(summary, body);
    details.addEventListener("toggle", () => {
      if (details.open && !body.dataset.loaded) {
        void loadPatch(item, body);
      }
    });
    item.append(details);
    updateFile(item, file);

    return item;
  }

  // Shows `file` in the heading of `item`. An opened patch loads again and
  // stays on screen meanwhile; a closed one loads when it next opens.
  function updateFile(item, file) {
    item.dataset.key = fileKey(file);
    const letter = element(
      document,
      "span",
      "diff-status diff-status--" + statusLetter(file.status),
      statusLetter(file.status),
    );
    letter.title = String(file.status ?? "");
    const path = element(document, "span", "diff-path", filePath(file));
    path.title = filePath(file);
    item
      .querySelector("summary")
      .replaceChildren(
        letter,
        path,
        lineCounts(Number(file.added_lines) || 0, Number(file.deleted_lines) || 0),
      );
    const body = item.querySelector(".diff-body");
    if (item.querySelector("details").open && body.dataset.loaded) {
      void loadPatch(item, body);
    } else {
      delete body.dataset.loaded;
    }
  }

  async function loadPatch(item, body) {
    const sessionId = state.sessionId;
    const key = item.dataset.key;
    body.dataset.loaded = "true";
    if (body.children.length === 0) {
      body.append(element(document, "p", "diff-note", "Loading…"));
    }
    let result;
    try {
      result = await request({
        method: "get_session_diff",
        path: item.dataset.path,
        session_id: sessionId,
      });
    } catch (error) {
      if (sessionId === state.sessionId && key === item.dataset.key) {
        delete body.dataset.loaded;
        body.replaceChildren(element(document, "p", "error", error.message));
      }

      return;
    }
    // A newer change of the file loads its own patch.
    if (sessionId !== state.sessionId || key !== item.dataset.key) {
      return;
    }
    body.replaceChildren(renderPatch(result?.patch));
    if (result?.truncated) {
      body.append(
        element(document, "p", "diff-note", "The rest of this patch is too large to show here."),
      );
    }
  }

  // One block per line, each ending in its newline, so a copied patch
  // keeps its line breaks.
  function renderPatch(patch) {
    const lines = patchLines(patch);
    if (lines.length === 0) {
      return element(document, "p", "diff-note", "No text changes to show.");
    }
    const pre = element(document, "pre", "patch");
    pre.append(
      ...lines.map((line) =>
        element(document, "span", "patch-line patch-line--" + line.kind, line.text + "\n"),
      ),
    );

    return pre;
  }

  function lineCounts(added, deleted) {
    const lines = element(document, "span", "lines");
    lines.append(
      element(document, "span", "add", "+" + added),
      " ",
      element(document, "span", "del", "-" + deleted),
    );

    return lines;
  }

  return {
    close: () => {
      state.sessionId = null;
      state.listKey = null;
      state.loads += 1;
      elements.view.hidden = true;
      elements.files.replaceChildren();
      elements.summary.replaceChildren();
      elements.error.textContent = "";
    },
    isOpen: () => state.sessionId !== null,
    open: (sessionId) => {
      state.sessionId = sessionId;
      state.listKey = null;
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
