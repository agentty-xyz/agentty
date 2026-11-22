import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { FakeEvent, MemoryStorage, createDocument, createWindow } from "./fake_dom.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

test("createDocument reads the client's page with its ids, attributes and text", () => {
  // Arrange
  const html = INDEX_HTML;

  // Act
  const document = createDocument(html);

  // Assert
  const transport = document.querySelector('meta[name="agentty-transport"]');
  assert.equal(transport.content, "local");
  assert.equal(document.querySelector(".code-field span").textContent, ">");
  assert.equal(document.getElementById("shell").hidden, true);
  assert.equal(document.getElementById("pair-code").getAttribute("maxlength"), "16");
  assert.equal(document.getElementById("message").value, "");
  assert.equal(document.querySelector("script").getAttribute("src"), "app.js");
  assert.equal(document.querySelectorAll("symbol").length, 15);
  assert.equal(document.querySelector("#menu use").getAttribute("href"), "#i-menu");
  assert.match(document.getElementById("pair-hint-local").textContent, /Run \/remote-control in Agentty/);
});

test("createDocument handles comments, entities, self-closing and raw text elements", () => {
  // Arrange
  const html =
    '<!DOCTYPE html><html><body><!-- note --><svg><use href="#i-plus" /></svg>' +
    "<p class='a b' data-row-id=7>&lt;b&gt; &amp; &#x41;&#66;</p><style>a > b {}</style></body></html>";

  // Act
  const document = createDocument(html);

  // Assert
  assert.equal(document.querySelector("svg > use").getAttribute("href"), "#i-plus");
  const paragraph = document.querySelector("p");
  assert.equal(paragraph.textContent, "<b> & AB");
  assert.equal(paragraph.children.length, 0);
  assert.deepEqual([...paragraph.classList], ["a", "b"]);
  assert.equal(paragraph.dataset.rowId, "7");
  assert.equal(document.querySelector("style").textContent, "a > b {}");
  assert.throws(() => createDocument("<div></span>"), /Unmatched/);
});

test("markup sinks, inline styles and inline handlers throw", () => {
  // Arrange
  const document = createDocument();
  const node = document.createElement("div");

  // Act and Assert
  assert.throws(() => {
    node.innerHTML = "<b>x</b>";
  }, /innerHTML is not allowed/);
  assert.throws(() => node.innerHTML, /innerHTML is not allowed/);
  assert.throws(() => node.outerHTML, /outerHTML is not allowed/);
  assert.throws(() => {
    node.outerHTML = "";
  }, /outerHTML is not allowed/);
  assert.throws(() => node.insertAdjacentHTML("beforeend", "<b>"), /insertAdjacentHTML/);
  assert.throws(() => document.write("<b>"), /document.write/);
  assert.throws(() => node.setAttribute("style", "color: red"), /style attribute/);
  assert.throws(() => node.style, /element.style/);
  assert.throws(() => node.setAttribute("onclick", "x()"), /onclick attribute/);
});

test("textContent keeps markup as text", () => {
  // Arrange
  const document = createDocument();
  const node = document.createElement("p");

  // Act
  node.textContent = "<img src=x onerror=alert(1)>";

  // Assert
  assert.equal(node.childNodes.length, 1);
  assert.equal(node.firstChild.nodeType, 3);
  assert.equal(node.children.length, 0);
  assert.equal(node.textContent, "<img src=x onerror=alert(1)>");
});

test("tree edits move nodes and keep parents consistent", () => {
  // Arrange
  const document = createDocument();
  const list = document.createElement("ol");
  const [first, second, third] = ["a", "b", "c"].map((text) => {
    const item = document.createElement("li");
    item.textContent = text;

    return item;
  });

  // Act
  list.append(first, third);
  list.insertBefore(second, third);
  const fragment = document.createDocumentFragment();
  fragment.append("x", "y");
  list.prepend(fragment);
  third.replaceWith("z");
  document.body.append(list);
  second.remove();

  // Assert
  assert.equal(list.textContent, "xyaz");
  assert.equal(list.children.length, 1);
  assert.equal(first.nextSibling.textContent, "z");
  assert.equal(first.previousSibling.textContent, "y");
  assert.equal(second.parentNode, null);
  assert.equal(third.parentNode, null);
  assert.equal(list.isConnected, true);
  assert.equal(second.isConnected, false);
  assert.equal(document.body.contains(first), true);
  assert.throws(() => list.append(document.body), /into itself/);
  list.replaceChildren();
  assert.equal(list.childNodes.length, 0);
});

test("node lists and children have no Array methods, as in a browser", () => {
  // Arrange
  const document = createDocument("<html><body><ul id=\"list\"><li>A</li><li>B</li></ul></body></html>");
  const list = document.getElementById("list");

  // Act
  const found = list.querySelectorAll("li");
  const children = list.children;
  const visited = [];
  found.forEach((node, index) => visited.push([index, node.textContent]));

  // Assert
  for (const collection of [found, children, document.children]) {
    assert.equal(Array.isArray(collection), false);
    assert.equal(collection.map, undefined);
    assert.equal(collection.filter, undefined);
    assert.equal(collection.slice, undefined);
  }
  assert.deepEqual(visited, [[0, "A"], [1, "B"]]);
  assert.equal(children.forEach, undefined);
  assert.deepEqual([found.length, children.length], [2, 2]);
  assert.equal(found[1].textContent, "B");
  assert.equal(found.item(0).textContent, "A");
  assert.equal(found.item(5), null);
  assert.equal(children.item(1), found[1]);
  assert.deepEqual(Array.from(children, (node) => node.textContent), ["A", "B"]);
  assert.deepEqual([...found.keys()], [0, 1]);
  assert.deepEqual(Array.from(found.values(), (node) => node.textContent), ["A", "B"]);
  assert.deepEqual(Array.from(found.entries(), ([index]) => index), [0, 1]);
  assert.equal(document.getElementById("list").children.namedItem("missing"), null);
  // A list is a snapshot: later changes leave it as it was.
  list.append(document.createElement("li"));
  assert.equal(found.length, 2);
});

test("selectors match tags, ids, classes, attributes and combinators", () => {
  // Arrange
  const document = createDocument(
    '<html><body><ul id="list"><li class="row open" data-id="1"><span>A</span></li>' +
      '<li class="row" data-id="2"><b><span>B</span></b></li></ul></body></html>',
  );

  // Act
  const open = document.querySelector("li.row.open");
  const second = document.querySelector('[data-id="2"]');
  const direct = document.querySelectorAll("li > span");
  const nested = document.querySelectorAll("#list span");
  const either = document.querySelectorAll("b, .open");

  // Assert
  assert.equal(open.dataset.id, "1");
  assert.equal(second.textContent, "B");
  assert.deepEqual(
    Array.from(direct, (node) => node.textContent),
    ["A"],
  );
  assert.deepEqual(
    Array.from(nested, (node) => node.textContent),
    ["A", "B"],
  );
  assert.equal(either.length, 2);
  assert.equal(second.querySelector("span").closest("li"), second);
  assert.equal(open.matches("ul li"), true);
  assert.throws(() => document.querySelector("li:first-child"), /Unsupported selector/);
});

test("element properties reflect attributes", () => {
  // Arrange
  const document = createDocument();
  const button = document.createElement("button");
  const input = document.createElement("input");

  // Act
  button.className = "primary";
  button.classList.add("wide");
  button.classList.toggle("primary");
  button.hidden = true;
  button.disabled = true;
  button.dataset.sessionId = "s1";
  input.value = "typed";
  input.placeholder = "Your answer";

  // Assert
  assert.equal(button.getAttribute("class"), "wide");
  assert.equal(button.hasAttribute("hidden"), true);
  assert.equal(button.getAttribute("data-session-id"), "s1");
  assert.deepEqual(Object.keys(button.dataset), ["sessionId"]);
  assert.equal(button.type, "submit");
  assert.equal(input.type, "text");
  assert.equal(input.value, "typed");
  assert.equal(input.hasAttribute("value"), false);
  assert.equal(input.getAttribute("placeholder"), "Your answer");
  delete button.dataset.sessionId;
  assert.equal(button.hasAttribute("data-session-id"), false);
});

test("events bubble to the window, and a submit button submits its form", () => {
  // Arrange
  const document = createDocument(
    '<html><body><form id="f"><button id="send">Send</button></form></body></html>',
  );
  const window = createWindow(document);
  const seen = [];
  document.getElementById("f").addEventListener("submit", (event) => {
    event.preventDefault();
    seen.push("submit");
  });
  document.addEventListener("click", () => seen.push("document click"));
  window.addEventListener("click", () => seen.push("window click"), { once: true });
  const button = document.getElementById("send");

  // Act
  button.click();
  button.click();
  button.disabled = true;
  button.click();

  // Assert
  assert.deepEqual(seen, ["document click", "window click", "submit", "document click", "submit"]);
});

test("a dialog opens modally and fires close", () => {
  // Arrange
  const document = createDocument();
  const dialog = document.createElement("dialog");
  let closed = 0;
  dialog.addEventListener("close", () => {
    closed += 1;
  });

  // Act
  dialog.showModal();
  const wasOpen = dialog.open;
  dialog.close("confirm");
  dialog.close();

  // Assert
  assert.equal(wasOpen, true);
  assert.equal(dialog.open, false);
  assert.equal(dialog.returnValue, "confirm");
  assert.equal(closed, 1);
});

test("a dialog form closes its dialog with the value of the button that submitted it", () => {
  // Arrange
  const document = createDocument(
    '<html><body><dialog id="d"><form method="dialog">' +
      '<button value="keep">Keep</button><button value="confirm">Go</button>' +
      "</form></dialog></body></html>",
  );
  const dialog = document.getElementById("d");
  const submitters = [];
  dialog.querySelector("form").addEventListener("submit", (event) => {
    submitters.push(event.submitter.textContent);
  });

  // Act
  dialog.showModal();
  dialog.querySelector('button[value="confirm"]').click();

  // Assert
  assert.equal(dialog.open, false);
  assert.equal(dialog.returnValue, "confirm");
  assert.deepEqual(submitters, ["Go"]);
});

test("the window keeps history, fires popstate and hashchange, and has storage", () => {
  // Arrange
  const document = createDocument();
  const window = createWindow(document, "http://127.0.0.1:4646/");
  const events = [];
  window.addEventListener("popstate", (event) => events.push("pop " + JSON.stringify(event.state)));
  window.addEventListener("hashchange", () => events.push("hash " + window.location.hash));

  // Act
  window.history.pushState({ drawer: true }, "", "#s=one");
  window.history.replaceState({ drawer: false }, "", "#s=two");
  window.history.back();
  window.location.hash = "relay=x";
  window.localStorage.setItem("k", "v");

  // Assert
  assert.equal(window.location.host, "127.0.0.1:4646");
  assert.equal(window.location.hash, "#relay=x");
  assert.equal(window.history.length, 2);
  assert.deepEqual(events, ["pop null", "hash #relay=x"]);
  assert.equal(window.localStorage.getItem("k"), "v");
  assert.equal(document.defaultView, window);
  assert.throws(() => new MemoryStorage({ failing: true }).getItem("k"), /unavailable/);
});

test("FakeEvent carries keyboard fields and honours cancelable", () => {
  // Arrange
  const event = new FakeEvent("keydown", { isComposing: true, key: "Enter", shiftKey: true });
  const fixed = new FakeEvent("close", { cancelable: false });

  // Act
  event.preventDefault();
  fixed.preventDefault();

  // Assert
  assert.equal(event.key, "Enter");
  assert.equal(event.shiftKey, true);
  assert.equal(event.isComposing, true);
  assert.equal(event.defaultPrevented, true);
  assert.equal(fixed.defaultPrevented, false);
});
