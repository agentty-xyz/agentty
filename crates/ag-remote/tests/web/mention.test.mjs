import assert from "node:assert/strict";
import test from "node:test";

import { FakeEvent, createDocument } from "./fake_dom.mjs";
import {
  MAX_MENTION_ITEMS,
  createMentions,
  filterEntries,
  mentionQuery,
  mentionText,
  offersMentions,
  parseEntries,
} from "../../assets/mention.mjs";
import { createSuggestions } from "../../assets/suggest.mjs";

const FILES = {
  files: [
    { is_dir: true, path: "crates" },
    { is_dir: true, path: "src" },
    { is_dir: false, path: "README.md" },
    { is_dir: false, path: "src/main.rs" },
    { is_dir: false, path: "src/xmxaxixn.rs" },
  ],
  kind: "session_files",
};

function entries(...paths) {
  return paths.map((path) => ({ isDir: path.endsWith("/"), path: path.replace(/\/$/, "") }));
}

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

// Mounts the `@` lookup on a text field. `respond(body)` answers each
// request; the requests sent and the errors shown are recorded. `context`
// is what the provider reads, and tests change it in place.
function mountMentions({ respond = () => FILES } = {}) {
  const document = createDocument(
    "<!DOCTYPE html><html><body><form><textarea id=\"field\"></textarea></form></body></html>",
  );
  const input = document.getElementById("field");
  const suggestions = createSuggestions({ document: document, id: "files", input: input });
  const context = { capabilities: ["files"], isConnected: true, session: { id: "s1" } };
  const requests = [];
  const errors = [];
  const mentions = createMentions({
    context: () => context,
    input: input,
    request: async (body) => {
      requests.push(body);

      return respond(body);
    },
    setText: (text, caret) => {
      input.value = text;
      input.setSelectionRange(caret, caret);
    },
    showError: (message) => errors.push(message),
    suggestions: suggestions,
  });
  input.addEventListener("keydown", (event) => suggestions.handleKey(event));
  const type = (text, caret = text.length) => {
    input.value = text;
    input.setSelectionRange(caret, caret);

    return mentions.update();
  };
  const press = (key) => input.dispatchEvent(new FakeEvent("keydown", { key: key }));
  const shown = () => Array.from(document.getElementById("files").children, (option) => option.dataset.key);

  return { context, errors, input, mentions, press, requests, shown, suggestions, type };
}

test("the lookup needs the files capability", () => {
  assert.equal(offersMentions(["files"]), true);
  assert.equal(offersMentions(["diff", "settings"]), false);
  assert.equal(offersMentions(["project_files"], "project"), true);
  assert.equal(offersMentions(["files"], "project"), false);
});

test("mentionQuery finds the @ token the caret is in, as the terminal does", () => {
  assert.deepEqual(mentionQuery("@"), { end: 1, query: "", start: 0 });
  assert.deepEqual(mentionQuery("see @src/ma"), { end: 11, query: "src/ma", start: 4 });
  assert.deepEqual(mentionQuery("(@src"), { end: 5, query: "src", start: 1 });
  assert.deepEqual(mentionQuery("[@a"), { end: 3, query: "a", start: 1 });
  assert.deepEqual(mentionQuery("{@a"), { end: 3, query: "a", start: 1 });
  assert.deepEqual(mentionQuery("line\n@a"), { end: 7, query: "a", start: 5 });
  // The token goes on past the caret, up to the first other character.
  assert.deepEqual(mentionQuery("@src/main.rs, then", 4), { end: 12, query: "src", start: 0 });
  // An @ inside a word, after a space, or with text selected is no token.
  assert.equal(mentionQuery("me@example"), null);
  assert.equal(mentionQuery("@src now"), null);
  assert.equal(mentionQuery("plain"), null);
  assert.equal(mentionQuery(""), null);
  assert.equal(mentionQuery("@src", 1, 3), null);
});

test("parseEntries keeps the listed paths and refuses other answers", () => {
  assert.deepEqual(
    parseEntries({ files: [{ is_dir: true, path: "src" }, { path: "a.rs" }, { path: "" }, null], kind: "session_files" }),
    [
      { isDir: true, path: "src" },
      { isDir: false, path: "a.rs" },
    ],
  );
  assert.deepEqual(parseEntries({ kind: "accepted" }), []);
  assert.deepEqual(parseEntries({ kind: "session_files" }), []);
  assert.deepEqual(parseEntries(null), []);
});

test("filterEntries ranks matches the way the terminal does", () => {
  const all = entries("src/", "src/main.rs");
  assert.deepEqual(filterEntries(all, ""), all);
  assert.notEqual(filterEntries(all, ""), all);
  // Characters must appear in order, ignoring case.
  assert.deepEqual(filterEntries(entries("src/Main.rs", "lib.rs"), "MAIN").map((entry) => entry.path), ["src/Main.rs"]);
  assert.deepEqual(filterEntries(entries("src/main.rs"), "niam"), []);
  // An exact file name beats a deeper match.
  assert.equal(
    filterEntries(entries("crates/ag-git/src/setting/lib.rs", "crates/agentty/src/app/setting.rs"), "setting")[0].path,
    "crates/agentty/src/app/setting.rs",
  );
  // A run of characters beats scattered ones.
  assert.equal(filterEntries(entries("src/xmxaxixn.rs", "src/main.rs"), "main")[0].path, "src/main.rs");
  // Equal matches prefer the shallower, then the shorter, then the first
  // path.
  assert.deepEqual(
    filterEntries(entries(".codex/AGENTS.md", "docs/AGENTS.md", "AGENTS.md"), "agents.md").map((entry) => entry.path),
    ["AGENTS.md", "docs/AGENTS.md", ".codex/AGENTS.md"],
  );
  assert.deepEqual(filterEntries(entries("b/x.rs", "a/x.rs"), "x").map((entry) => entry.path), ["a/x.rs", "b/x.rs"]);
  // Starts and middles of file names score too; a query with `/` gets no
  // name bonus.
  assert.deepEqual(filterEntries(entries("a/zzsetzz.rs", "b/settings.rs"), "set").map((entry) => entry.path), [
    "b/settings.rs",
    "a/zzsetzz.rs",
  ]);
  // A trailing `/` lists directories first, and matches a directory's own
  // name.
  assert.deepEqual(filterEntries(entries("src/aaa.rs", "src/zzz/"), "src/"), entries("src/zzz/", "src/aaa.rs"));
  assert.deepEqual(filterEntries(entries("src/", "src/main.rs"), "src/"), entries("src/", "src/main.rs"));
  // A character whose lowercase form is longer still matches.
  assert.equal(filterEntries(entries("İx.rs"), "i").length, 1);
  // A path written with a trailing `/` still earns the name bonus.
  assert.deepEqual(
    filterEntries([{ isDir: false, path: "zdocsz/x" }, { isDir: false, path: "a/docs/" }], "docs").map((entry) => entry.path),
    ["a/docs/", "zdocsz/x"],
  );
});

test("mentionText puts a trailing space after files and a slash after directories", () => {
  assert.equal(mentionText({ isDir: false, path: "src/main.rs" }), "@src/main.rs ");
  assert.equal(mentionText({ isDir: true, path: "src" }), "@src/ ");
});

test("the project scope lists the project checkout, and never the session's entries", async () => {
  const page = mountMentions();
  page.type("@");
  await settle();
  Object.assign(page.context, { capabilities: ["project_files"], fileScope: "project", session: null });

  // The session's entries stay hidden until the project's arrive.
  page.mentions.refresh();
  assert.equal(page.suggestions.isOpen(), false);
  page.mentions.close();
  page.type("@READ");
  assert.deepEqual(page.requests.at(-1), { method: "list_project_files" });
  assert.equal(page.suggestions.isOpen(), false);
  await settle();
  assert.deepEqual(page.shown(), ["README.md"]);
});

test("a picked project's checkout is asked for by its id and kept apart from the terminal's", async () => {
  // Arrange
  const page = mountMentions();
  Object.assign(page.context, { capabilities: ["project_files"], fileScope: "project", session: null });
  page.type("@READ");
  await settle();
  page.mentions.close();

  // Act: the device picks project `2`; the terminal's entries do not show
  // for it.
  page.context.projectId = "2";
  page.mentions.refresh();
  const shownBefore = page.suggestions.isOpen();
  page.type("@REA");

  // Assert
  assert.equal(shownBefore, false);
  assert.deepEqual(page.requests, [
    { method: "list_project_files" },
    { method: "list_project_files", project_id: "2" },
  ]);
  assert.equal(page.suggestions.isOpen(), false);
  await settle();
  assert.deepEqual(page.shown(), ["README.md"]);
});

test("typing @ lists the session's files once they arrive", async () => {
  const page = mountMentions();

  assert.equal(page.type("look at @"), false);
  assert.deepEqual(page.requests, [{ method: "list_session_files", session_id: "s1" }]);
  await settle();
  assert.equal(page.suggestions.isOpen(), true);
  assert.deepEqual(page.shown(), ["crates/", "src/", "README.md", "src/main.rs", "src/xmxaxixn.rs"]);
  assert.equal(page.suggestions.element.querySelector(".suggest-title").textContent, "Files");

  // Typing on filters the same list without asking again.
  assert.equal(page.type("look at @main"), true);
  assert.deepEqual(page.shown(), ["src/main.rs", "src/xmxaxixn.rs"]);
  assert.equal(page.requests.length, 1);
});

test("picking a file replaces the whole token and moves the caret after it", async () => {
  const page = mountMentions();
  page.type("see @ma and more", 7);
  await settle();
  assert.deepEqual(page.shown(), ["src/main.rs", "src/xmxaxixn.rs"]);

  page.press("Enter");

  assert.equal(page.input.value, "see @src/main.rs  and more");
  assert.equal(page.input.selectionStart, 17);
  assert.equal(page.suggestions.isOpen(), false);
  // The caret after the inserted space ends the lookup.
  assert.equal(page.mentions.update(), false);
});

test("picking a directory leaves @dir/ with the caret after it", async () => {
  const page = mountMentions();
  page.type("@sr");
  await settle();
  page.suggestions.element.querySelector('[data-key="src/"]').click();

  assert.equal(page.input.value, "@src/ ");
  assert.equal(page.input.selectionStart, 6);
});

test("Escape closes the list until the text changes, and a new lookup reads the files again", async () => {
  const page = mountMentions();
  page.type("@");
  await settle();
  page.press("Escape");

  assert.equal(page.suggestions.isOpen(), false);
  assert.equal(page.mentions.update(), false);
  assert.equal(page.type("@s"), true);
  await settle();
  assert.equal(page.requests.length, 2);
});

test("without the capability, a connection, a session or a scope the lookup stays shut", async () => {
  for (const change of [{ capabilities: ["diff"] }, { isConnected: false }, { session: null }, { fileScope: null }]) {
    const page = mountMentions();
    Object.assign(page.context, change);

    assert.equal(page.type("@"), false);
    assert.deepEqual(page.requests, []);
  }
});

test("a refresh follows the session and keeps an open list", async () => {
  const page = mountMentions();
  page.type("@");
  await settle();
  page.press("ArrowDown");

  page.mentions.refresh();
  assert.equal(page.suggestions.isOpen(), true);
  assert.equal(page.input.getAttribute("aria-activedescendant"), "files-1");

  page.context.isConnected = false;
  page.mentions.refresh();
  assert.equal(page.suggestions.isOpen(), false);
  // A refresh with nothing open shows nothing.
  page.context.isConnected = true;
  page.mentions.refresh();
  assert.equal(page.suggestions.isOpen(), false);
});

test("a failed listing says why, and an empty one shows nothing", async () => {
  const failing = mountMentions({
    respond: () => {
      throw new Error("Session not found");
    },
  });
  failing.type("@");
  await settle();

  assert.deepEqual(failing.errors, ["Could not list the files: Session not found"]);
  assert.equal(failing.suggestions.isOpen(), false);

  const empty = mountMentions({ respond: () => ({ files: [], kind: "session_files" }) });
  empty.type("@");
  await settle();
  assert.equal(empty.suggestions.isOpen(), false);
});

test("files of another session are never shown, and answers after a reset are dropped", async () => {
  let release;
  const page = mountMentions({
    respond: () =>
      new Promise((resolve) => {
        release = () => resolve(FILES);
      }),
  });
  page.type("@");
  // Typing while the files load asks only once.
  page.type("@s");
  assert.equal(page.requests.length, 1);
  page.mentions.reset();
  release();
  await settle();
  assert.equal(page.suggestions.isOpen(), false);

  const other = mountMentions();
  other.type("@");
  await settle();
  other.context.session = { id: "s2" };
  other.mentions.refresh();
  assert.equal(other.suggestions.isOpen(), false);
  // A list for another session is never shown either, before it loads.
  other.mentions.close();
  other.input.value = "@s";
  assert.equal(other.mentions.update(), false);
});

test("a load that finishes after Escape or a close does not reopen the list", async () => {
  let release;
  const page = mountMentions({
    respond: () =>
      new Promise((resolve) => {
        release = () => resolve(FILES);
      }),
  });
  // The first lookup loads the files and shows them.
  page.type("@");
  release();
  await settle();
  assert.equal(page.suggestions.isOpen(), true);

  // A new lookup shows the earlier files at once and reads them again;
  // Escape before the answer keeps the list closed.
  page.mentions.close();
  page.type("@s");
  assert.equal(page.suggestions.isOpen(), true);
  page.press("Escape");
  release();
  await settle();
  assert.equal(page.suggestions.isOpen(), false);

  // So does a close, as on blur.
  page.type("@sr");
  page.mentions.close();
  release();
  await settle();
  assert.equal(page.suggestions.isOpen(), false);
});

test("the lookup leaves another provider's list alone", async () => {
  const page = mountMentions();
  page.type("@");
  await settle();
  page.suggestions.show({ items: [{ key: "/model", label: "/model" }], onPick: () => {}, owner: "slash" });

  page.input.value = "/";
  page.mentions.refresh();
  page.mentions.close();
  assert.equal(page.suggestions.isOpen("slash"), true);
});

test("the list shows at most the best matches", async () => {
  const many = Array.from({ length: MAX_MENTION_ITEMS + 5 }, (_value, index) => ({
    is_dir: false,
    path: "file" + String(index).padStart(3, "0") + ".rs",
  }));
  const page = mountMentions({ respond: () => ({ files: many, kind: "session_files" }) });
  page.type("@");
  await settle();

  assert.equal(page.shown().length, MAX_MENTION_ITEMS);
});
