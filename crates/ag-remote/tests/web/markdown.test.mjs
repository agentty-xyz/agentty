import assert from "node:assert/strict";
import test from "node:test";

import { createDocument } from "./fake_dom.mjs";
import {
  MARKDOWN_TAGS,
  MAX_MARKDOWN_LENGTH,
  MAX_NESTING,
  lookAheadReadCount,
  parseInlineMarkdown,
  parseMarkdown,
  renderMarkdown,
  safeLink,
} from "../../assets/markdown.mjs";

// Renders `text` into a fresh element and records every tag the renderer
// asks the document for.
function render(text, { inline = false, window = null } = {}) {
  const document = createDocument();
  const created = [];
  const createElement = document.createElement.bind(document);
  document.createElement = (tag) => {
    created.push(tag);

    return createElement(tag);
  };
  const root = createElement("div");
  const tree = inline ? parseInlineMarkdown(text) : parseMarkdown(text);
  root.append(renderMarkdown(tree, document, { window }));

  return { created, root };
}

// A compact view of an element tree: `[tag, ...children]`, with text as
// strings and the attributes that matter as an object after the tag.
function shape(node) {
  if (node.nodeType === 3) {
    return node.textContent;
  }
  const attributes = Object.fromEntries(node.attributes.map((attribute) => [attribute.name, attribute.value]));
  const head = Object.keys(attributes).length > 0 ? [node.localName, attributes] : [node.localName];

  return [...head, ...node.childNodes.map(shape)];
}

function body(text, options) {
  return render(text, options).root.childNodes.map(shape);
}

// The deepest element nesting under `root`, walked without recursion.
function depthOf(root) {
  let deepest = 0;
  const pending = [[root, 0]];
  while (pending.length > 0) {
    const [node, depth] = pending.pop();
    deepest = Math.max(deepest, depth);
    for (const child of node.children) {
      pending.push([child, depth + 1]);
    }
  }

  return deepest;
}

function timed(run) {
  const start = performance.now();
  const result = run();

  return { elapsed: performance.now() - start, result };
}

const LINK = { href: "https://example.com/", rel: "noopener noreferrer", target: "_blank" };

test("paragraphs split on blank lines and keep their line breaks", () => {
  assert.deepEqual(body("one\ntwo\n\nthree"), [["p", "one", ["br"], "two"], ["p", "three"]]);
  assert.deepEqual(body("hard  \nbreak\\\nend"), [["p", "hard", ["br"], "break", ["br"], "end"]]);
});

test("headings render levels one to three, deeper levels as three", () => {
  assert.deepEqual(body("# One\n## Two ##\n### Three\n#### Four\n###### Six\n#hashtag"), [
    ["h1", "One"],
    ["h2", "Two"],
    ["h3", "Three"],
    ["h3", "Four"],
    ["h3", "Six"],
    ["p", "#hashtag"],
  ]);
});

test("emphasis, strong, strikethrough and code spans nest", () => {
  assert.deepEqual(body("*em* _em_ **strong** __strong__ ***both*** ~~gone~~ `a *b*`"), [
    [
      "p",
      ["em", "em"],
      " ",
      ["em", "em"],
      " ",
      ["strong", "strong"],
      " ",
      ["strong", "strong"],
      " ",
      ["em", ["strong", "both"]],
      " ",
      ["del", "gone"],
      " ",
      ["code", "a *b*"],
    ],
  ]);
  assert.deepEqual(body("snake_case_name and 2 * 3 * 4 and ``a ` b``"), [
    ["p", "snake_case_name and 2 * 3 * 4 and ", ["code", "a ` b"]],
  ]);
  assert.deepEqual(body("\\*not em\\* and \\`not code\\`"), [["p", "*not em* and `not code`"]]);
});

test("fenced code keeps its text and shows the language with a copy button", async () => {
  const copied = [];
  const window = {
    navigator: { clipboard: { writeText: async (text) => copied.push(text) } },
    setTimeout: () => {},
  };
  const { root } = render("```rust\nfn main() {\n    <b>&amp;</b>\n}\n```\n~~~\nplain\n~~~", { window });
  assert.deepEqual(root.childNodes.map(shape), [
    [
      "div",
      { class: "code-block" },
      [
        "div",
        { class: "code-head" },
        ["span", "rust"],
        ["button", { "aria-label": "Copy code", class: "copy-btn", type: "button" }, "copy"],
      ],
      ["pre", ["code", "fn main() {\n    <b>&amp;</b>\n}"]],
    ],
    [
      "div",
      { class: "code-block" },
      [
        "div",
        { class: "code-head" },
        ["span", "text"],
        ["button", { "aria-label": "Copy code", class: "copy-btn", type: "button" }, "copy"],
      ],
      ["pre", ["code", "plain"]],
    ],
  ]);

  const button = root.querySelector(".copy-btn");
  button.click();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(copied, ["fn main() {\n    <b>&amp;</b>\n}"]);
  assert.equal(button.textContent, "copied");
});

test("a diagram fence renders as code", () => {
  assert.deepEqual(body("```mermaid\ngraph TD\n  A --> B\n```"), [
    ["div", { class: "code-block" }, ["div", { class: "code-head" }, ["span", "mermaid"]], ["pre", ["code", "graph TD\n  A --> B"]]],
  ]);
});

test("the copy button is absent without a clipboard and quiet when the write fails", async () => {
  assert.equal(render("```\nx\n```").root.querySelector(".copy-btn"), null);
  assert.equal(render("```\nx\n```", { window: { navigator: {} } }).root.querySelector(".copy-btn"), null);

  const window = {
    navigator: {
      clipboard: {
        writeText: () => {
          throw new Error("denied");
        },
      },
    },
  };
  const button = render("```\nx\n```", { window }).root.querySelector(".copy-btn");
  button.click();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(button.textContent, "copy");
});

test("lists keep their kind and start number, nest by indentation, and mark tasks", () => {
  assert.deepEqual(body("- one\n- two\n  - nested\n    1. deep\n* other list\n\n3) three\n4) four"), [
    ["ul", ["li", "one"], ["li", "two", ["ul", ["li", "nested", ["ol", ["li", "deep"]]]]]],
    ["ul", ["li", "other list"]],
    ["ol", { start: "3" }, ["li", "three"], ["li", "four"]],
  ]);
  assert.deepEqual(body("- [ ] todo\n- [x] done"), [
    [
      "ul",
      ["li", { class: "md-task-item" }, ["span", { "aria-label": "Not done", class: "md-task" }, "[ ]"], " ", "todo"],
      ["li", { class: "md-task-item" }, ["span", { "aria-label": "Done", class: "md-task" }, "[x]"], " ", "done"],
    ],
  ]);
  // A blank line between items keeps their paragraphs.
  assert.deepEqual(body("- one\n\n- two"), [["ul", ["li", ["p", "one"]], ["li", ["p", "two"]]]]);
  // A list interrupts a paragraph; a lazy line continues an item.
  assert.deepEqual(body("Steps:\n- a\nstill a"), [["p", "Steps:"], ["ul", ["li", "a", ["br"], "still a"]]]);
});

test("block quotes nest and hold other blocks", () => {
  assert.deepEqual(body("> quote\n> > inner\n> - item"), [
    ["blockquote", ["p", "quote"], ["blockquote", ["p", "inner"]], ["ul", ["li", "item"]]],
  ]);
});

test("tables align their columns and fill a missing cell", () => {
  assert.deepEqual(body("| Check | Scope | Result |\n|:--|:-:|--:|\n| a | b |\n| c | d | e | extra |"), [
    [
      "div",
      { class: "table-scroll" },
      [
        "table",
        [
          "thead",
          [
            "tr",
            ["th", { class: "align-left" }, "Check"],
            ["th", { class: "align-center" }, "Scope"],
            ["th", { class: "align-right" }, "Result"],
          ],
        ],
        [
          "tbody",
          [
            "tr",
            ["td", { class: "align-left" }, "a"],
            ["td", { class: "align-center" }, "b"],
            ["td", { class: "align-right" }],
          ],
          [
            "tr",
            ["td", { class: "align-left" }, "c"],
            ["td", { class: "align-center" }, "d"],
            ["td", { class: "align-right" }, "e"],
          ],
        ],
      ],
    ],
  ]);
  assert.deepEqual(body("a | b\n--- | ---\n`x \\| y` | **z**"), [
    [
      "div",
      { class: "table-scroll" },
      ["table", ["thead", ["tr", ["th", "a"], ["th", "b"]]], ["tbody", ["tr", ["td", ["code", "x | y"]], ["td", ["strong", "z"]]]]],
    ],
  ]);
  // A delimiter row with another cell count is no table.
  assert.equal(render("a | b\n--- | --- | ---").root.querySelector("table"), null);
});

test("thematic breaks split blocks", () => {
  assert.deepEqual(body("above\n***\n- - -\n___\nbelow"), [["p", "above"], ["hr"], ["hr"], ["hr"], ["p", "below"]]);
});

test("links, autolinks and bare URLs open in a new tab without a referrer", () => {
  assert.deepEqual(body("[site](https://example.com) <https://example.com> https://example.com."), [
    ["p", ["a", LINK, "site"], " ", ["a", LINK, "https://example.com"], " ", ["a", LINK, "https://example.com"], "."],
  ]);
  assert.deepEqual(body('[a *b*](<https://example.com> "title") (see https://example.com/x)'), [
    ["p", ["a", LINK, "a ", ["em", "b"]], " (see ", ["a", { ...LINK, href: "https://example.com/x" }, "https://example.com/x"], ")"],
  ]);
  assert.deepEqual(body("<dev@example.com> [mail](mailto:dev@example.com)"), [
    [
      "p",
      ["a", { href: "mailto:dev@example.com", rel: "noopener noreferrer", target: "_blank" }, "dev@example.com"],
      " ",
      ["a", { href: "mailto:dev@example.com", rel: "noopener noreferrer", target: "_blank" }, "mail"],
    ],
  ]);
  // A link inside a link text is only its text.
  const nested = render("[see https://example.com/x](https://example.com)").root;
  assert.deepEqual(
    Array.from(nested.querySelectorAll("a"), (link) => [link.getAttribute("href"), link.textContent]),
    [["https://example.com/", "see https://example.com/x"]],
  );
});

test("images render as a link with their alternative text and load nothing", () => {
  const { created, root } = render("![a diagram](https://example.com/d.png) ![](https://example.com/e.png)");
  assert.deepEqual(root.childNodes.map(shape), [
    [
      "p",
      ["a", { ...LINK, class: "md-image", href: "https://example.com/d.png" }, "a diagram"],
      " ",
      ["a", { ...LINK, class: "md-image", href: "https://example.com/e.png" }, "https://example.com/e.png"],
    ],
  ]);
  assert.ok(!created.includes("img"));
});

test("only http, https and mailto links survive", () => {
  assert.equal(safeLink("https://example.com/a b"), "https://example.com/a%20b");
  assert.equal(safeLink("mailto:dev@example.com"), "mailto:dev@example.com");
  for (const unsafe of [
    "javascript:alert(1)",
    "JaVaScRiPt:alert(1)",
    " javascript:alert(1)",
    "java\tscript:alert(1)",
    "data:text/html,<script>alert(1)</script>",
    "vbscript:msgbox",
    "file:///etc/passwd",
    "/relative/path",
    "#fragment",
    "",
    null,
  ]) {
    assert.equal(safeLink(unsafe), null, String(unsafe));
  }
  assert.deepEqual(body("[x](javascript:alert(1)) [y](data:text/html,hi) <javascript:alert(1)> [z](/local)"), [
    ["p", "[x](javascript:alert(1)) [y](data:text/html,hi) <javascript:alert(1)> [z](/local)"],
  ]);
  // A hand-built tree is checked again when it renders.
  const document = createDocument();
  const fragment = renderMarkdown(
    { children: [{ children: [{ text: "go", type: "text" }], href: "javascript:alert(1)", type: "link" }], type: "inline" },
    document,
  );
  assert.deepEqual(fragment.childNodes.map(shape), [["span", "go"]]);
});

test("raw HTML stays text", () => {
  const { created, root } = render(
    '<script>alert(1)</script>\n\n<img src=x onerror="alert(1)">\n\n<a href="javascript:alert(1)">x</a> <style>*{}</style>',
  );
  assert.deepEqual(root.childNodes.map(shape), [
    ["p", "<script>alert(1)</script>"],
    ["p", '<img src=x onerror="alert(1)">'],
    ["p", '<a href="javascript:alert(1)">x</a> <style>*{}</style>'],
  ]);
  for (const tag of ["script", "img", "style", "iframe"]) {
    assert.ok(!created.includes(tag), tag);
  }
  assert.equal(root.querySelector("a"), null);
});

test("an unclosed fence runs to the end of the message", () => {
  assert.deepEqual(body("before\n```js\nlet a = 1;\n\n# not a heading"), [
    ["p", "before"],
    ["div", { class: "code-block" }, ["div", { class: "code-head" }, ["span", "js"]], ["pre", ["code", "let a = 1;\n\n# not a heading"]]],
  ]);
});

test("unclosed brackets, runs and code spans stay text", () => {
  assert.deepEqual(body("[open *em ```code"), [["p", "[open *em ```code"]]);
  assert.deepEqual(body("[not a link] (x) ![no"), [["p", "[not a link] (x) ![no"]]);
});

test("ten thousand nested quotes or list markers stop at the nesting limit", () => {
  for (const text of [">".repeat(10_000) + " deep", "> ".repeat(10_000) + "deep", "* ".repeat(10_000) + "deep"]) {
    const { elapsed, result } = timed(() => render(text));
    assert.ok(elapsed < 1000, "took " + elapsed + " ms");
    assert.ok(depthOf(result.root) <= MAX_NESTING * 2 + 2, "nested " + depthOf(result.root));
    assert.ok(result.root.textContent.endsWith("deep"));
  }
});

test("ten thousand nested emphasis runs and brackets stop at the nesting limit", () => {
  for (const text of [
    "*".repeat(10_000) + "deep" + "*".repeat(10_000),
    "*a ".repeat(10_000) + "deep" + " a*".repeat(10_000),
    "_a ".repeat(5_000) + "*b ".repeat(5_000) + "deep" + " b*".repeat(5_000) + " a_".repeat(5_000),
    "[".repeat(10_000) + "deep" + "](https://example.com)".repeat(10_000),
    "`".repeat(10_000) + " ` `` ``` ".repeat(1_000),
  ]) {
    const { elapsed, result } = timed(() => render(text));
    assert.ok(elapsed < 1000, "took " + elapsed + " ms");
    assert.ok(depthOf(result.root) <= MAX_NESTING + 2, "nested " + depthOf(result.root));
    assert.ok(result.root.textContent.includes("deep") || text.startsWith("`"));
  }
});

test("link attempts that never close read the text about once", () => {
  // Each `](` once read up to 2,048 characters ahead, hundreds of times
  // the text's length for the first case. Now the look-ahead reads each
  // character a few times at most, which a count shows without timing
  // the parser on a busy machine.
  for (const text of ["[](".repeat(85_000), '[](x "'.repeat(40_000), "[](<".repeat(60_000), "[](()".repeat(50_000)]) {
    assert.ok(text.length < MAX_MARKDOWN_LENGTH);
    const before = lookAheadReadCount();
    const result = parseMarkdown(text);
    const reads = lookAheadReadCount() - before;
    assert.ok(reads <= 16 * text.length, "read " + reads + " characters of " + text.length);
    assert.equal(result.children.length, 1);
  }
  // Nested parentheses and an angle-bracket destination still link.
  assert.deepEqual(body("[a](https://example.com/a_(b_(c))) [b](<https://example.com/x y>)"), [
    [
      "p",
      ["a", { ...LINK, href: "https://example.com/a_(b_(c))" }, "a"],
      " ",
      ["a", { ...LINK, href: "https://example.com/x%20y" }, "b"],
    ],
  ]);
});

test("emphasis follows the rule of 3", () => {
  assert.deepEqual(body("*foo**bar**baz*"), [["p", ["em", "foo", ["strong", "bar"], "baz"]]]);
  assert.deepEqual(body("**foo*bar*baz**"), [["p", ["strong", "foo", ["em", "bar"], "baz"]]]);
  assert.deepEqual(body("**foo*bar**"), [["p", ["strong", "foo*bar"]]]);
  assert.deepEqual(body("**a*"), [["p", "*", ["em", "a"]]]);
  const { elapsed } = timed(() => parseMarkdown("*a**".repeat(60_000)));
  assert.ok(elapsed < 500, "took " + elapsed + " ms");
});

test("deep indentation nests lists only to the limit", () => {
  const text = Array.from({ length: 400 }, (_value, level) => "  ".repeat(level) + "- level " + level).join("\n");
  const { elapsed, result } = timed(() => render(text));
  assert.ok(elapsed < 1000, "took " + elapsed + " ms");
  assert.ok(depthOf(result.root) <= MAX_NESTING * 2 + 2);
  assert.ok(result.root.textContent.includes("level 399"));
});

test("a message over the size limit renders as plain text", () => {
  const text = "# Heading\n\n" + "**bold** `code` [x](https://example.com)\n".repeat(8_000);
  assert.ok(text.length > 300 * 1024 && text.length > MAX_MARKDOWN_LENGTH);
  const { elapsed, result } = timed(() => render(text));
  assert.ok(elapsed < 1000, "took " + elapsed + " ms");
  assert.deepEqual(
    Array.from(result.root.children, (child) => [child.localName, child.className]),
    [["div", "md-plain"]],
  );
  assert.equal(result.root.textContent, text);
  assert.deepEqual(parseInlineMarkdown(text).children, [{ text: text, type: "text" }]);
});

test("a large message just under the limit parses in linear time", () => {
  const unit = "## Step\n\nSome *em*, **strong** and `code` with https://example.com/x.\n\n- item [link](https://example.com)\n- item\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\ncode\n```\n\n";
  const text = unit.repeat(Math.floor(MAX_MARKDOWN_LENGTH / unit.length));
  const { elapsed, result } = timed(() => parseMarkdown(text));
  assert.ok(elapsed < 2000, "took " + elapsed + " ms");
  assert.ok(result.children.length > 1000);
});

test("inline Markdown keeps block syntax as text", () => {
  assert.deepEqual(body("# not a heading\n- not a list with `code`", { inline: true }), [
    "# not a heading",
    ["br"],
    "- not a list with ",
    ["code", "code"],
  ]);
});

test("the renderer creates only tags from its table and sets only allowed attributes", () => {
  const text = [
    "# H1",
    "## H2",
    "#### H4",
    "Para *em* **strong** ~~del~~ `code` [link](https://example.com) ![img](https://example.com/i.png)",
    "line  ",
    "break",
    "> quote",
    "- [ ] task",
    "1. one",
    "| a | b |",
    "|:-:|--:|",
    "| c |",
    "***",
    "```js",
    "x",
    "```",
    "<script>alert(1)</script><img onerror=x><iframe>",
  ].join("\n");
  const window = { navigator: { clipboard: { writeText: async () => {} } } };
  const { created, root } = render(text, { window });
  const used = new Set(created);
  for (const tag of used) {
    assert.ok(MARKDOWN_TAGS.has(tag), tag + " is not in the tag table");
  }
  // Every entry of the table is exercised by this input.
  assert.deepEqual([...used].sort(), [...MARKDOWN_TAGS].sort());
  const allowed = new Set(["aria-label", "class", "href", "rel", "start", "target", "type"]);
  for (const node of root.querySelectorAll("*")) {
    for (const attribute of node.attributes) {
      assert.ok(allowed.has(attribute.name), attribute.name + " on " + node.localName);
    }
  }
});
