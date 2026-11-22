import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, createDocument, createWindow } from "./fake_dom.mjs";
import {
  createDiff,
  diffTotals,
  filePath,
  patchLines,
  statusLetter,
} from "../../assets/diff.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");
const FILES = [
  { added_lines: 12, deleted_lines: 3, path: "src/lib.rs", status: "modified" },
  { added_lines: 40, deleted_lines: 0, path: "src/new.rs", status: "added" },
  { added_lines: 0, deleted_lines: 9, old_path: "src/a.rs", path: "src/b.rs", status: "renamed" },
];
const PATCH = [
  "diff --git a/src/lib.rs b/src/lib.rs",
  "index 1..2 100644",
  "--- a/src/lib.rs",
  "+++ b/src/lib.rs",
  "@@ -1,3 +1,3 @@",
  " fn main() {",
  "-    old();",
  "+    new();",
  "\\ No newline at end of file",
  "",
].join("\n");

// Mounts the diff view on the client's page. `respond(body)` answers each
// request; the requests sent are recorded.
function mountDiff(respond) {
  const window = createWindow(createDocument(INDEX_HTML));
  const document = window.document;
  const byId = (id) => document.getElementById(id);
  const requests = [];
  const diff = createDiff({
    document: document,
    elements: {
      error: byId("diff-error"),
      files: byId("diff-files"),
      summary: byId("diff-summary"),
      view: byId("diff-view"),
    },
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
  });

  return { byId, diff, document, requests };
}

function answer(body) {
  return body.path
    ? { files: FILES.filter((file) => file.path === body.path), kind: "diff", patch: PATCH }
    : { files: FILES, kind: "diff" };
}

// Lets pending promise callbacks run.
function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function openFile(page, index) {
  const details = page.byId("diff-files").querySelectorAll("details")[index];
  details.open = true;
  details.dispatchEvent(new FakeEvent("toggle"));

  return details;
}

test("statuses show git's letters, and unknown ones a question mark", () => {
  assert.deepEqual(
    ["added", "deleted", "modified", "renamed", "copied", undefined].map(statusLetter),
    ["A", "D", "M", "R", "?", "?"],
  );
});

test("a renamed file names both paths", () => {
  assert.equal(filePath(FILES[0]), "src/lib.rs");
  assert.equal(filePath(FILES[2]), "src/a.rs → src/b.rs");
  assert.equal(filePath({ old_path: "same.rs", path: "same.rs" }), "same.rs");
  assert.equal(filePath(null), "");
});

test("patch lines take the kind that tints them", () => {
  assert.deepEqual(
    patchLines(PATCH).map((line) => line.kind),
    ["meta", "meta", "meta", "meta", "hunk", "context", "del", "add", "meta"],
  );
  // `---` and `+++` in a hunk are changed lines, not headers.
  assert.deepEqual(
    patchLines("@@ -1 +1 @@\n--- a\n+++ b").map((line) => line.kind),
    ["hunk", "del", "add"],
  );
  assert.deepEqual(patchLines(""), []);
  assert.deepEqual(patchLines(undefined), []);
});

test("totals add up the files' line counts", () => {
  assert.deepEqual(diffTotals(FILES), { added: 52, deleted: 12 });
  assert.deepEqual(diffTotals([]), { added: 0, deleted: 0 });
});

test("the view lists the files and loads a patch only when one opens", async () => {
  const page = mountDiff(answer);
  await page.diff.open("s1");

  assert.equal(page.diff.isOpen(), true);
  assert.equal(page.byId("diff-view").hidden, false);
  assert.equal(page.byId("diff-summary").textContent, "3 files · +52 -12");
  const rows = page.byId("diff-files").querySelectorAll("summary");
  assert.deepEqual(
    Array.from(rows, (row) => row.textContent),
    ["Msrc/lib.rs+12 -3", "Asrc/new.rs+40 -0", "Rsrc/a.rs → src/b.rs+0 -9"],
  );
  assert.equal(rows[1].querySelector(".diff-status").className, "diff-status diff-status--A");
  assert.deepEqual(page.requests, [{ method: "get_session_diff", session_id: "s1" }]);

  const details = openFile(page, 0);
  await settle();
  assert.deepEqual(page.requests[1], {
    method: "get_session_diff",
    path: "src/lib.rs",
    session_id: "s1",
  });
  const lines = details.querySelectorAll(".patch-line");
  assert.equal(lines.length, 9);
  assert.equal(lines[6].className, "patch-line patch-line--del");
  assert.equal(lines[7].className, "patch-line patch-line--add");
  // Each line ends in its newline, so a copied patch keeps its breaks.
  assert.equal(lines[7].textContent, "+    new();\n");
  assert.equal(details.querySelector(".patch").textContent, PATCH);
  // Closing and opening again reuses the loaded patch.
  details.dispatchEvent(new FakeEvent("toggle"));
  assert.equal(page.requests.length, 2);
});

test("a refresh with the same files changes nothing on screen", async () => {
  const page = mountDiff(answer);
  await page.diff.open("s1");
  const details = openFile(page, 2);
  await settle();
  const rows = [...page.byId("diff-files").children];
  const patch = details.querySelector(".patch");
  const summary = page.byId("diff-summary").firstChild;
  await page.diff.refresh();
  await settle();

  assert.deepEqual([...page.byId("diff-files").children], rows);
  assert.equal(details.querySelector(".patch"), patch);
  assert.equal(page.byId("diff-summary").firstChild, summary);
  assert.equal(details.open, true);
  // Only the list was read again; the open patch was not.
  assert.deepEqual(
    page.requests.map((body) => body.path ?? null),
    [null, "src/b.rs", null],
  );
});

test("a changed file keeps its row and its old patch until the new one arrives", async () => {
  let files = FILES;
  let patch = PATCH;
  let release = null;
  const page = mountDiff((body) => {
    if (!body.path) {
      return { files: files, kind: "diff" };
    }
    const answered = { files: files.filter((file) => file.path === body.path), kind: "diff", patch };

    return release === null
      ? answered
      : new Promise((resolve) => {
          release = () => resolve(answered);
        });
  });
  await page.diff.open("s1");
  const changed = openFile(page, 0);
  const untouched = openFile(page, 1);
  await settle();
  const untouchedPatch = untouched.querySelector(".patch");
  const rows = [...page.byId("diff-files").children];

  // `src/lib.rs` grows, `src/new.rs` stays, `src/b.rs` goes, and a new
  // file comes first.
  files = [
    { added_lines: 1, deleted_lines: 0, path: "README.md", status: "modified" },
    { ...FILES[0], added_lines: 13 },
    FILES[1],
  ];
  patch = "@@ -1 +1 @@\n-old\n+newer\n";
  release = () => {};
  await page.diff.refresh();
  await settle();

  const now = page.byId("diff-files").children;
  assert.deepEqual(
    Array.from(now, (row) => row.dataset.path),
    ["README.md", "src/lib.rs", "src/new.rs"],
  );
  assert.equal(now[1], rows[0]);
  assert.equal(now[2], rows[1]);
  assert.equal(now[1].querySelector("summary").textContent, "Msrc/lib.rs+13 -3");
  assert.equal(page.byId("diff-summary").textContent, "3 files · +54 -3");
  // The changed file shows its old patch while the new one loads; the
  // other keeps its patch and is not read again.
  assert.equal(changed.querySelectorAll(".patch-line").length, 9);
  assert.equal(changed.querySelector(".diff-note"), null);
  assert.equal(untouched.querySelector(".patch"), untouchedPatch);
  release();
  await settle();
  assert.equal(changed.querySelector(".patch").textContent, "@@ -1 +1 @@\n-old\n+newer\n");
  assert.deepEqual(
    page.requests.map((body) => body.path ?? null),
    [null, "src/lib.rs", "src/new.rs", null, "src/lib.rs"],
  );
});

test("a patch answer for an older change of the file is dropped", async () => {
  let files = FILES;
  const releases = [];
  const page = mountDiff((body) =>
    body.path
      ? new Promise((resolve) => {
          releases.push(() => resolve({ files: [], kind: "diff", patch: "@@ -1 +1 @@\n+v" + releases.length }));
        })
      : { files: files, kind: "diff" },
  );
  await page.diff.open("s1");
  const details = openFile(page, 0);
  files = [{ ...FILES[0], added_lines: 20 }, ...FILES.slice(1)];
  await page.diff.refresh();
  releases[0]();
  await settle();
  assert.equal(details.querySelector(".patch"), null);
  releases[1]();
  await settle();
  assert.equal(details.querySelector(".patch").textContent, "@@ -1 +1 @@\n+v2\n");
});

test("a closed file whose change differs loads afresh when it opens", async () => {
  let files = FILES;
  const page = mountDiff((body) =>
    body.path ? answer(body) : { files: files, kind: "diff" },
  );
  await page.diff.open("s1");
  const details = openFile(page, 0);
  await settle();
  details.open = false;
  files = [{ ...FILES[0], added_lines: 20 }, ...FILES.slice(1)];
  await page.diff.refresh();
  details.open = true;
  details.dispatchEvent(new FakeEvent("toggle"));
  await settle();

  assert.deepEqual(
    page.requests.map((body) => body.path ?? null),
    [null, "src/lib.rs", null, "src/lib.rs"],
  );
});

test("paths and patch lines render only as text", async () => {
  const hostile = '<img src=x onerror="alert(1)">';
  const page = mountDiff((body) => ({
    files: [{ added_lines: 1, deleted_lines: 0, path: hostile, status: "added" }],
    kind: "diff",
    patch: body.path ? "@@ -0,0 +1 @@\n+" + hostile : undefined,
  }));
  await page.diff.open("s1");
  openFile(page, 0);
  await settle();

  assert.equal(page.byId("diff-files").querySelector(".diff-path").textContent, hostile);
  assert.equal(page.byId("diff-files").querySelectorAll(".patch-line")[1].textContent, "+" + hostile + "\n");
  assert.equal(page.document.querySelectorAll("img").length, 0);
});

test("an empty diff, a cut list and a cut patch say so", async () => {
  const empty = mountDiff(() => ({ files: [], kind: "diff" }));
  await empty.diff.open("s1");
  assert.equal(empty.byId("diff-summary").textContent, "No changes yet.");

  const cut = mountDiff((body) => ({
    files: [FILES[0]],
    kind: "diff",
    patch: body.path ? "" : undefined,
    truncated: true,
  }));
  await cut.diff.open("s1");
  assert.equal(
    cut.byId("diff-summary").textContent,
    "1 file shown · +12 -3 · the rest is too large to list here",
  );
  const details = openFile(cut, 0);
  await settle();
  assert.deepEqual(
    Array.from(details.querySelectorAll(".diff-note"), (note) => note.textContent),
    ["No text changes to show.", "The rest of this patch is too large to show here."],
  );
});

test("failures show beside what failed, and a failed patch can load again", async () => {
  let fails = true;
  const page = mountDiff((body) => {
    if (fails) {
      throw new Error(body.path ? "Patch unavailable" : "Session not found");
    }

    return answer(body);
  });
  await page.diff.open("s1");
  assert.equal(page.byId("diff-error").textContent, "Session not found");

  fails = false;
  await page.diff.refresh();
  assert.equal(page.byId("diff-error").textContent, "");
  fails = true;
  const details = openFile(page, 0);
  await settle();
  assert.equal(details.querySelector(".error").textContent, "Patch unavailable");
  fails = false;
  details.dispatchEvent(new FakeEvent("toggle"));
  await settle();
  assert.equal(details.querySelectorAll(".patch-line").length, 9);
});

test("closing drops the view, and answers that arrive afterwards", async () => {
  let release;
  const page = mountDiff(
    (body) =>
      new Promise((resolve) => {
        release = () => resolve(answer(body));
      }),
  );
  const loading = page.diff.open("s1");
  assert.equal(page.byId("diff-summary").textContent, "Loading the changes…");
  page.diff.close();
  release();
  await loading;

  assert.equal(page.diff.isOpen(), false);
  assert.equal(page.byId("diff-view").hidden, true);
  assert.equal(page.byId("diff-files").children.length, 0);
  assert.equal(page.byId("diff-summary").textContent, "");
  await page.diff.refresh();
  assert.equal(page.requests.length, 1);
});

test("a failure after closing stays quiet", async () => {
  let reject;
  const page = mountDiff(
    () =>
      new Promise((_resolve, fail) => {
        reject = () => fail(new Error("late"));
      }),
  );
  const loading = page.diff.open("s1");
  page.diff.close();
  reject();
  await loading;

  assert.equal(page.byId("diff-error").textContent, "");
});

test("a patch for a session no longer shown is dropped", async () => {
  let release;
  const page = mountDiff((body) => {
    if (!body.path) {
      return answer(body);
    }

    return new Promise((resolve) => {
      release = () => resolve(answer(body));
    });
  });
  await page.diff.open("s1");
  const details = openFile(page, 0);
  page.diff.close();
  release();
  await settle();

  assert.equal(details.querySelectorAll(".patch-line").length, 0);
});
