import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { createDocument } from "./fake_dom.mjs";
import { createInstanceView, fallbackName } from "../../assets/instance.mjs";

const INDEX_HTML = readFileSync(new URL("../../assets/index.html", import.meta.url), "utf8");

function mount() {
  const document = createDocument(INDEX_HTML);
  const byId = (id) => document.getElementById(id);
  const create = () => createInstanceView({ document: document, prototype: byId("instance-prototype") });

  return { byId, create, document };
}

test("each view is its own copy of the prototype, with its parts found", () => {
  // Arrange
  const page = mount();

  // Act
  const first = page.create();
  const second = page.create();

  // Assert
  assert.notEqual(first.list, second.list);
  assert.notEqual(first.project.sync, second.project.sync);
  assert.equal(first.root.className, "instance");
  assert.equal(first.project.line.hidden, true);
  assert.equal(first.project.syncLabel.textContent, "Sync");
  assert.equal(first.newSession.hidden, true);
  assert.equal(first.newSession.textContent.trim(), "New session");
  assert.equal(first.empty.textContent, "No sessions in this project.");
  assert.equal(first.error.getAttribute("role"), "alert");
  // The project picker starts hidden and takes its name from its label.
  assert.equal(first.project.select.localName, "select");
  assert.equal(first.project.select.hidden, true);
  assert.equal(first.project.select.getAttribute("aria-label"), "Project");
  assert.notEqual(first.project.select, second.project.select);
  // The prototype itself stays hidden and whole.
  assert.equal(page.byId("instance-prototype").hidden, true);
  assert.equal(page.byId("instance-prototype").querySelectorAll("[data-part]").length, 18);
  // Copies carry no ids, so the page never holds one twice.
  assert.equal(first.root.querySelectorAll("[id]").length, 0);
});

test("alone, a view puts its project and New session in the slot and leaves its heading out", () => {
  // Arrange
  const page = mount();
  const view = page.create();
  const slot = page.byId("instance-slot");

  // Act
  const alone = view.mount(true, slot);
  const inSlot = Array.from(slot.children, (child) => child.className);
  const among = view.mount(false, slot);

  // Assert
  assert.deepEqual(alone, [view.list, view.empty, view.error]);
  assert.deepEqual(inSlot, ["instance-top"]);
  assert.deepEqual(among, [view.root]);
  // Among others the project line returns under the view's heading.
  assert.equal(slot.children.length, 0);
  assert.equal(view.root.contains(view.project.line), true);
  assert.equal(view.root.querySelector(".instance-head").nextSibling.className, "instance-top");
  assert.deepEqual(
    Array.from(view.root.children, (child) => child.className),
    ["instance-head", "instance-top", "instance-sessions", "list-empty", "error"],
  );
});

test("the heading names the Agentty as text and shows its state in the state's tone", () => {
  // Arrange
  const page = mount();
  const view = page.create();
  view.mount(false, page.byId("instance-slot"));
  const name = '<img src=x onerror="alert(1)">';

  // Act
  view.setName(name);
  view.setState("reconnecting", "Reconnecting");
  const reconnecting = view.root.querySelector(".instance-head .status").className;
  view.setState("connected", "Connected");
  const status = view.root.querySelector(".instance-head .status");
  const connected = status.className;
  const label = status.lastChild.firstChild;
  view.setState("connected", "Connected");
  const isLabelKept = status.lastChild.firstChild === label;
  view.setState("unknown", "Odd");

  // Assert
  assert.equal(view.root.querySelector(".instance-name").textContent, name);
  assert.equal(view.root.querySelectorAll("img").length, 0);
  assert.equal(view.root.getAttribute("aria-label"), name);
  assert.equal(reconnecting, "status status--working");
  assert.equal(connected, "status status--ready");
  // An unchanged label is not written again.
  assert.equal(isLabelKept, true);
  assert.equal(status.className, "status status--idle");
  assert.equal(status.textContent, "Odd");
});

test("fallbackName shows the start of the instance identifier", () => {
  assert.equal(fallbackName("0123456789abcdef".repeat(4)), "Agentty 01234567");
});
