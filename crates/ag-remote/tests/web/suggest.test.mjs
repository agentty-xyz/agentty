import assert from "node:assert/strict";
import test from "node:test";

import { FakeEvent, createDocument } from "./fake_dom.mjs";
import { createSuggestions } from "../../assets/suggest.mjs";

const ITEMS = [
  { detail: "First item", key: "a", label: "Alpha" },
  { detail: "", key: "b", label: "Beta" },
  { detail: "Third item", isCurrent: true, key: "c", label: "Gamma" },
];

// Mounts a popup for a text field in a form; picks and dismissals are
// recorded.
function mountSuggestions() {
  const document = createDocument(
    "<!DOCTYPE html><html><body><form id=\"form\"><textarea id=\"field\"></textarea>" +
      "<button id=\"send\">Send</button></form></body></html>",
  );
  const input = document.getElementById("field");
  const suggestions = createSuggestions({ document: document, id: "field-suggestions", input: input });
  // The field's owner hands its keys to the popup, as chat.mjs does.
  input.addEventListener("keydown", (event) => suggestions.handleKey(event));
  const picked = [];
  let dismissals = 0;
  const show = (items = ITEMS, title = "") =>
    suggestions.show({
      items: items,
      onDismiss: () => {
        dismissals += 1;
      },
      onPick: (item) => picked.push(item.key),
      title: title,
    });

  return { dismissals: () => dismissals, document, input, picked, show, suggestions };
}

function key(input, name, init = {}) {
  const event = new FakeEvent("keydown", { key: name, ...init });
  input.dispatchEvent(event);

  return event;
}

function options(document) {
  return Array.from(document.getElementById("field-suggestions").children);
}

test("the popup sits after its field, closed, and makes the field a combobox", () => {
  const { document, input, suggestions } = mountSuggestions();

  assert.equal(input.nextSibling, suggestions.element);
  assert.equal(suggestions.isOpen(), false);
  assert.equal(suggestions.element.hidden, true);
  const list = document.getElementById("field-suggestions");
  assert.equal(list.getAttribute("role"), "listbox");
  assert.equal(list.getAttribute("aria-label"), "Suggestions");
  assert.equal(input.getAttribute("role"), "combobox");
  assert.equal(input.getAttribute("aria-autocomplete"), "list");
  assert.equal(input.getAttribute("aria-controls"), "field-suggestions");
  assert.equal(input.getAttribute("aria-expanded"), "false");
  assert.equal(input.hasAttribute("aria-activedescendant"), false);
});

test("shown items render as options with their label, detail and current mark as text", () => {
  const { document, input, show, suggestions } = mountSuggestions();
  show([...ITEMS, { detail: "<b>x</b>", key: "d", label: "<img>" }], "Things");

  assert.equal(suggestions.isOpen(), true);
  assert.equal(input.getAttribute("aria-expanded"), "true");
  assert.equal(suggestions.element.querySelector(".suggest-title").textContent, "Things");
  assert.equal(document.getElementById("field-suggestions").getAttribute("aria-label"), "Things");
  const shown = options(document);
  assert.deepEqual(
    shown.map((option) => [option.id, option.getAttribute("role"), option.dataset.key, option.textContent]),
    [
      ["field-suggestions-0", "option", "a", "AlphaFirst item"],
      ["field-suggestions-1", "option", "b", "Beta"],
      ["field-suggestions-2", "option", "c", "GammacurrentThird item"],
      ["field-suggestions-3", "option", "d", "<img><b>x</b>"],
    ],
  );
  assert.equal(shown[2].classList.contains("is-current"), true);
  assert.equal(shown[3].querySelector("img"), null);
  // The first item is highlighted.
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-0");
  assert.equal(shown[0].getAttribute("aria-selected"), "true");
  assert.equal(shown[1].getAttribute("aria-selected"), "false");
});

test("a list without a title hides it and keeps the popup's own name", () => {
  const { document, show, suggestions } = mountSuggestions();
  show(ITEMS, "Things");
  show(ITEMS.slice(0, 2));

  assert.equal(suggestions.element.querySelector(".suggest-title").hidden, true);
  assert.equal(document.getElementById("field-suggestions").getAttribute("aria-label"), "Suggestions");
});

test("arrow keys move the highlight and wrap around, and Enter picks it", () => {
  const { document, input, picked, show } = mountSuggestions();
  show();

  assert.equal(key(input, "ArrowDown").defaultPrevented, true);
  key(input, "ArrowDown");
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-2");
  key(input, "ArrowDown");
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-0");
  key(input, "ArrowUp");
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-2");
  assert.equal(options(document)[2].classList.contains("is-highlighted"), true);
  assert.equal(options(document)[0].classList.contains("is-highlighted"), false);

  const enter = key(input, "Enter");
  assert.equal(enter.defaultPrevented, true);
  assert.deepEqual(picked, ["c"]);
});

test("Tab picks, Shift+Tab and other keys pass, and composition is left alone", () => {
  const { input, picked, show, suggestions } = mountSuggestions();
  show();

  assert.equal(suggestions.handleKey(new FakeEvent("keydown", { key: "Tab" })), true);
  assert.deepEqual(picked, ["a"]);
  const shiftTab = key(input, "Tab", { shiftKey: true });
  assert.equal(shiftTab.defaultPrevented, false);
  assert.equal(key(input, "a").defaultPrevented, false);
  assert.equal(suggestions.handleKey(new FakeEvent("keydown", { isComposing: true, key: "Enter" })), false);
  assert.equal(suggestions.handleKey(new FakeEvent("keydown", { key: "Enter", keyCode: 229 })), false);
  assert.deepEqual(picked, ["a"]);
});

test("Escape closes the popup and tells the provider", () => {
  const { dismissals, input, show, suggestions } = mountSuggestions();
  show();

  const escape = key(input, "Escape");
  assert.equal(escape.defaultPrevented, true);
  assert.equal(suggestions.isOpen(), false);
  assert.equal(input.getAttribute("aria-expanded"), "false");
  assert.equal(input.hasAttribute("aria-activedescendant"), false);
  assert.equal(dismissals(), 1);
  // A closed popup uses no keys.
  assert.equal(suggestions.handleKey(new FakeEvent("keydown", { key: "Enter" })), false);
  assert.equal(suggestions.handleKey(new FakeEvent("keydown", { key: "Escape" })), false);
  assert.equal(dismissals(), 1);
});

test("a click picks that item, and a press on the list keeps the field's focus", () => {
  const { document, picked, show, suggestions } = mountSuggestions();
  show();

  const press = new FakeEvent("mousedown");
  options(document)[1].dispatchEvent(press);
  assert.equal(press.defaultPrevented, true);
  options(document)[1].querySelector(".suggest-label").click();
  assert.deepEqual(picked, ["b"]);
  assert.equal(suggestions.isOpen(), true);
});

test("an empty list closes the popup, and close forgets the items", () => {
  const { document, input, picked, show, suggestions } = mountSuggestions();
  show();
  show([]);

  assert.equal(suggestions.isOpen(), false);
  assert.equal(options(document).length, 0);
  show();
  suggestions.close();
  assert.equal(suggestions.isOpen(), false);
  assert.equal(key(input, "Enter").defaultPrevented, false);
  assert.deepEqual(picked, []);
});

test("showing the same list again keeps its nodes and highlight, and a new one starts over", () => {
  const { document, input, picked, show, suggestions } = mountSuggestions();
  show();
  key(input, "ArrowDown");
  const nodes = options(document);

  show();
  assert.deepEqual(options(document), nodes);
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-1");
  // The newest callbacks still answer for the same list.
  const later = [];
  suggestions.show({ items: ITEMS, onPick: (item) => later.push(item.key), title: "" });
  key(input, "Enter");
  assert.deepEqual(later, ["b"]);
  assert.deepEqual(picked, []);

  show(ITEMS.slice(1));
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-0");
  assert.equal(options(document)[0].dataset.key, "b");
});

test("an owner closes and asks about only its own list", () => {
  const { input, suggestions } = mountSuggestions();
  suggestions.show({ items: ITEMS, onPick: () => {}, owner: "slash" });

  assert.equal(suggestions.isOpen(), true);
  assert.equal(suggestions.isOpen("slash"), true);
  assert.equal(suggestions.isOpen("mention"), false);
  suggestions.close("mention");
  assert.equal(suggestions.isOpen("slash"), true);
  // The same items from another owner are a new list.
  key(input, "ArrowDown");
  suggestions.show({ items: ITEMS, onPick: () => {}, owner: "mention" });
  assert.equal(input.getAttribute("aria-activedescendant"), "field-suggestions-0");
  assert.equal(suggestions.isOpen("slash"), false);
  suggestions.close("mention");
  assert.equal(suggestions.isOpen(), false);
});
