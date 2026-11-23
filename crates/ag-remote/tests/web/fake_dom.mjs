// A small DOM for the web client's Node tests. It holds the subset of the
// browser API the client may use, and refuses the rest: `innerHTML`,
// `outerHTML`, `insertAdjacentHTML`, `document.write`, inline styles and
// inline event handler attributes all throw, so a test fails as soon as
// client code tries to build markup from a string. `querySelectorAll` and
// `children` return lists without Array methods, as a browser does.
//
// `createDocument(html)` builds a document from markup such as the
// client's index.html. `createWindow(document, url)` adds the window the
// client reads: storage, location, history, timers, and window events.
// Modules take the document as a parameter, as in
// `renderSomething(model, document)`, so tests pass this one.

const VOID_ELEMENTS = new Set(["br", "hr", "img", "input", "link", "meta", "source", "wbr"]);
const RAW_TEXT_ELEMENTS = new Set(["script", "style"]);
const ENTITIES = { amp: "&", apos: "'", gt: ">", larr: "←", lt: "<", nbsp: " ", quot: '"', rarr: "→" };
// Attributes reflected as properties. Boolean ones are present or absent.
const BOOLEAN_PROPERTIES = { disabled: "disabled", hidden: "hidden", open: "open", required: "required" };
const STRING_PROPERTIES = {
  className: "class",
  content: "content",
  href: "href",
  id: "id",
  name: "name",
  placeholder: "placeholder",
  title: "title",
};
// Elements whose `value` is a property of their own once read or set.
const VALUE_ELEMENTS = new Set(["input", "select", "textarea"]);

function refuse(member) {
  throw new Error(member + " is not allowed: build nodes and set textContent instead");
}

// A snapshot list of nodes, as a browser returns it: indexable, with
// `length` and `item()`, and iterable, but without the Array methods, so
// client code that calls `.map` or `.filter` on one fails here as it does
// in a browser. Spread it or use `Array.from` first.
class FakeNodeCollection {
  #items;

  constructor(items) {
    this.#items = items;
    items.forEach((item, index) => {
      Object.defineProperty(this, index, { enumerable: true, value: item });
    });
  }

  get length() {
    return this.#items.length;
  }

  item(index) {
    return this.#items[index] ?? null;
  }

  [Symbol.iterator]() {
    return this.#items.values();
  }
}

// What `querySelectorAll` returns: a `NodeList`, which also has
// `forEach`, `entries`, `keys` and `values`.
class FakeNodeList extends FakeNodeCollection {
  forEach(callback, thisArg) {
    for (let index = 0; index < this.length; index += 1) {
      callback.call(thisArg, this[index], index, this);
    }
  }

  entries() {
    return Array.from(this).entries();
  }

  keys() {
    return Array.from(this).keys();
  }

  values() {
    return this[Symbol.iterator]();
  }
}

// What `children` returns: an `HTMLCollection`, which has no `forEach`.
class FakeHTMLCollection extends FakeNodeCollection {
  namedItem(name) {
    return Array.from(this).find((node) => node.id === name || node.getAttribute("name") === name) ?? null;
  }
}

export class FakeEvent {
  constructor(type, init = {}) {
    Object.assign(this, init);
    this.type = type;
    this.bubbles = init.bubbles ?? true;
    this.cancelable = init.cancelable ?? true;
    this.defaultPrevented = false;
    this.target = null;
    this.currentTarget = null;
    this.propagationStopped = false;
  }

  preventDefault() {
    if (this.cancelable) {
      this.defaultPrevented = true;
    }
  }

  stopPropagation() {
    this.propagationStopped = true;
  }
}

// Event listeners shared by nodes, the document, and the window.
class FakeEventTarget {
  #listeners = new Map();

  addEventListener(type, listener, options = {}) {
    const listeners = this.#listeners.get(type) || [];
    listeners.push({ listener: listener, once: Boolean(options?.once) });
    this.#listeners.set(type, listeners);
  }

  removeEventListener(type, listener) {
    const listeners = this.#listeners.get(type) || [];
    this.#listeners.set(
      type,
      listeners.filter((entry) => entry.listener !== listener),
    );
  }

  // Runs the listeners of this target only; `dispatchEvent` adds bubbling.
  notify(event) {
    event.currentTarget = this;
    for (const entry of [...(this.#listeners.get(event.type) || [])]) {
      if (entry.once) {
        this.removeEventListener(event.type, entry.listener);
      }
      if (typeof entry.listener === "function") {
        entry.listener.call(this, event);
      } else {
        entry.listener.handleEvent(event);
      }
    }
  }

  dispatchEvent(event) {
    event.target ??= this;
    this.notify(event);

    return !event.defaultPrevented;
  }
}

class FakeNode extends FakeEventTarget {
  constructor(ownerDocument) {
    super();
    this.ownerDocument = ownerDocument;
    this.parentNode = null;
    this.childNodes = [];
  }

  get parentElement() {
    return this.parentNode instanceof FakeElement ? this.parentNode : null;
  }

  get firstChild() {
    return this.childNodes[0] ?? null;
  }

  get lastChild() {
    return this.childNodes.at(-1) ?? null;
  }

  get nextSibling() {
    const siblings = this.parentNode?.childNodes || [];

    return siblings[siblings.indexOf(this) + 1] ?? null;
  }

  get previousSibling() {
    const siblings = this.parentNode?.childNodes || [];
    const index = siblings.indexOf(this);

    return index > 0 ? siblings[index - 1] : null;
  }

  get textContent() {
    return this.childNodes.map((child) => child.textContent).join("");
  }

  set textContent(value) {
    this.replaceChildren();
    const text = String(value ?? "");
    if (text) {
      this.append(text);
    }
  }

  get isConnected() {
    let node = this;
    while (node.parentNode) {
      node = node.parentNode;
    }

    return node === this.ownerDocument;
  }

  contains(other) {
    for (let node = other; node; node = node.parentNode) {
      if (node === this) {
        return true;
      }
    }

    return false;
  }

  append(...nodes) {
    for (const node of nodes) {
      this.insertBefore(this.#adopt(node), null);
    }
  }

  prepend(...nodes) {
    const first = this.firstChild;
    for (const node of nodes) {
      this.insertBefore(this.#adopt(node), first);
    }
  }

  appendChild(node) {
    return this.insertBefore(node, null);
  }

  insertBefore(node, reference) {
    if (node instanceof FakeDocumentFragment) {
      for (const child of [...node.childNodes]) {
        this.insertBefore(child, reference);
      }

      return node;
    }
    if (node.contains(this)) {
      throw new Error("A node cannot be inserted into itself");
    }
    node.remove();
    const index = reference ? this.childNodes.indexOf(reference) : this.childNodes.length;
    if (index < 0) {
      throw new Error("The reference node is not a child of this node");
    }
    this.childNodes.splice(index, 0, node);
    node.parentNode = this;

    return node;
  }

  removeChild(node) {
    const index = this.childNodes.indexOf(node);
    if (index < 0) {
      throw new Error("The node is not a child of this node");
    }
    this.childNodes.splice(index, 1);
    node.parentNode = null;

    return node;
  }

  replaceChildren(...nodes) {
    for (const child of [...this.childNodes]) {
      this.removeChild(child);
    }
    this.append(...nodes);
  }

  remove() {
    this.parentNode?.removeChild(this);
  }

  replaceWith(...nodes) {
    const parent = this.parentNode;
    if (!parent) {
      return;
    }
    const next = this.nextSibling;
    this.remove();
    for (const node of nodes) {
      parent.insertBefore(this.#adopt(node), next);
    }
  }

  // Events bubble from the target through its ancestors to the document
  // and, when there is one, its window.
  dispatchEvent(event) {
    event.target ??= this;
    const path = [];
    for (let node = this; node; node = node.parentNode) {
      path.push(node);
    }
    if (path.at(-1) === this.ownerDocument && this.ownerDocument.defaultView) {
      path.push(this.ownerDocument.defaultView);
    }
    for (const node of event.bubbles ? path : [this]) {
      node.notify(event);
      if (event.propagationStopped) {
        break;
      }
    }

    return !event.defaultPrevented;
  }

  #adopt(node) {
    return typeof node === "string" ? this.ownerDocument.createTextNode(node) : node;
  }
}

class FakeText extends FakeNode {
  constructor(ownerDocument, data) {
    super(ownerDocument);
    this.data = String(data);
  }

  get nodeType() {
    return 3;
  }

  get textContent() {
    return this.data;
  }

  set textContent(value) {
    this.data = String(value ?? "");
  }

  get nodeValue() {
    return this.data;
  }
}

class FakeDocumentFragment extends FakeNode {
  get nodeType() {
    return 11;
  }
}

class FakeClassList {
  #element;

  constructor(element) {
    this.#element = element;
  }

  #names() {
    return (this.#element.getAttribute("class") || "").split(/\s+/).filter(Boolean);
  }

  #write(names) {
    this.#element.setAttribute("class", names.join(" "));
  }

  get length() {
    return this.#names().length;
  }

  add(...names) {
    const current = this.#names();
    this.#write([...current, ...names.filter((name) => !current.includes(name))]);
  }

  remove(...names) {
    this.#write(this.#names().filter((name) => !names.includes(name)));
  }

  contains(name) {
    return this.#names().includes(name);
  }

  toggle(name, force) {
    const isPresent = force ?? !this.contains(name);
    if (isPresent) {
      this.add(name);
    } else {
      this.remove(name);
    }

    return isPresent;
  }

  values() {
    return this.#names().values();
  }

  [Symbol.iterator]() {
    return this.values();
  }
}

function datasetAttribute(key) {
  return "data-" + key.replace(/[A-Z]/g, (letter) => "-" + letter.toLowerCase());
}

function datasetKey(attribute) {
  return attribute.slice("data-".length).replace(/-([a-z])/g, (_match, letter) => letter.toUpperCase());
}

export class FakeElement extends FakeNode {
  #attributes = new Map();
  #value = null;
  #selection = null;

  constructor(ownerDocument, tagName, namespaceURI = null) {
    super(ownerDocument);
    this.localName = tagName.toLowerCase();
    this.namespaceURI = namespaceURI;
    this.classList = new FakeClassList(this);
    this.dataset = new Proxy(
      {},
      {
        deleteProperty: (_target, key) => {
          this.removeAttribute(datasetAttribute(key));

          return true;
        },
        get: (_target, key) =>
          typeof key === "string" ? (this.getAttribute(datasetAttribute(key)) ?? undefined) : undefined,
        getOwnPropertyDescriptor: (_target, key) => {
          const value = this.getAttribute(datasetAttribute(key));

          return value === null
            ? undefined
            : { configurable: true, enumerable: true, value: value, writable: true };
        },
        has: (_target, key) => this.hasAttribute(datasetAttribute(key)),
        ownKeys: () =>
          [...this.#attributes.keys()].filter((name) => name.startsWith("data-")).map(datasetKey),
        set: (_target, key, value) => {
          this.setAttribute(datasetAttribute(key), value);

          return true;
        },
      },
    );
    // Layout does not exist here; tests set these to model scrolling.
    this.clientHeight = 0;
    this.scrollHeight = 0;
    this.scrollTop = 0;
  }

  get nodeType() {
    return 1;
  }

  get tagName() {
    return this.localName.toUpperCase();
  }

  get attributes() {
    return [...this.#attributes].map(([name, value]) => ({ name: name, value: value }));
  }

  getAttribute(name) {
    return this.#attributes.get(name.toLowerCase()) ?? null;
  }

  hasAttribute(name) {
    return this.#attributes.has(name.toLowerCase());
  }

  setAttribute(name, value) {
    const lowered = name.toLowerCase();
    if (lowered === "style") {
      refuse("The style attribute");
    }
    if (lowered.startsWith("on")) {
      refuse("The " + lowered + " attribute");
    }
    this.#attributes.set(lowered, String(value));
  }

  removeAttribute(name) {
    this.#attributes.delete(name.toLowerCase());
  }

  toggleAttribute(name, force) {
    const isPresent = force ?? !this.hasAttribute(name);
    if (isPresent) {
      this.setAttribute(name, "");
    } else {
      this.removeAttribute(name);
    }

    return isPresent;
  }

  get children() {
    return new FakeHTMLCollection(elementChildren(this));
  }

  get firstElementChild() {
    return elementChildren(this)[0] ?? null;
  }

  get lastElementChild() {
    return elementChildren(this).at(-1) ?? null;
  }

  get childElementCount() {
    return this.children.length;
  }

  get type() {
    const type = this.getAttribute("type");
    if (type !== null) {
      return type;
    }

    return this.localName === "button" ? "submit" : this.localName === "input" ? "text" : "";
  }

  set type(value) {
    this.setAttribute("type", value);
  }

  get value() {
    if (this.#value !== null) {
      return this.#value;
    }

    return this.localName === "textarea" ? this.textContent : (this.getAttribute("value") ?? "");
  }

  set value(value) {
    if (!VALUE_ELEMENTS.has(this.localName)) {
      this.setAttribute("value", value);

      return;
    }
    this.#value = String(value ?? "");
    // As in a browser, setting the value puts the caret at its end.
    this.#selection = null;
  }

  // The caret of a text field: at the end of the value unless
  // `setSelectionRange` placed it, until the value changes.
  get selectionStart() {
    return Math.min(this.#selection?.start ?? this.value.length, this.value.length);
  }

  get selectionEnd() {
    return Math.min(this.#selection?.end ?? this.value.length, this.value.length);
  }

  setSelectionRange(start, end = start) {
    this.#selection = { end: Math.max(start, end), start: start };
  }

  get rows() {
    return Number(this.getAttribute("rows") ?? 2);
  }

  set rows(value) {
    this.setAttribute("rows", value);
  }

  get start() {
    return Number(this.getAttribute("start") ?? 1);
  }

  set start(value) {
    this.setAttribute("start", value);
  }

  get form() {
    for (let node = this.parentNode; node; node = node.parentNode) {
      if (node.localName === "form") {
        return node;
      }
    }

    return null;
  }

  get style() {
    return refuse("element.style");
  }

  get innerHTML() {
    return refuse("innerHTML");
  }

  set innerHTML(_value) {
    refuse("innerHTML");
  }

  get outerHTML() {
    return refuse("outerHTML");
  }

  set outerHTML(_value) {
    refuse("outerHTML");
  }

  insertAdjacentHTML() {
    refuse("insertAdjacentHTML");
  }

  // There is no layout: every box is empty at the origin, unless a test
  // sets `document.layout(element)` to place it.
  getBoundingClientRect() {
    const box = this.ownerDocument?.layout?.(this) ?? {};

    return { height: 0, left: 0, top: 0, width: 0, ...box };
  }

  matches(selector) {
    return parseSelector(selector).some((complex) => matchesComplex(this, complex));
  }

  closest(selector) {
    for (let node = this; node instanceof FakeElement; node = node.parentNode) {
      if (node.matches(selector)) {
        return node;
      }
    }

    return null;
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] ?? null;
  }

  querySelectorAll(selector) {
    return new FakeNodeList(descendants(this).filter((node) => node.matches(selector)));
  }

  // A click on an enabled submit button submits its form, as in a browser.
  click() {
    if (this.disabled) {
      return;
    }
    const isAllowed = this.dispatchEvent(new FakeEvent("click"));
    if (isAllowed && this.localName === "button" && this.type === "submit") {
      this.form?.requestSubmit(this);
    }
  }

  // Fires `submit`. A form with `method="dialog"` that is not prevented
  // then closes its dialog with the submitter's value.
  requestSubmit(submitter = null) {
    const isAllowed = this.dispatchEvent(new FakeEvent("submit", { submitter: submitter }));
    if (isAllowed && this.getAttribute("method") === "dialog") {
      this.closest("dialog")?.close(submitter?.value);
    }
  }

  focus() {
    this.ownerDocument.activeElement = this;
    this.dispatchEvent(new FakeEvent("focus", { bubbles: false, cancelable: false }));
  }

  blur() {
    if (this.ownerDocument.activeElement === this) {
      this.ownerDocument.activeElement = this.ownerDocument.body;
    }
  }

  // `<dialog>` support: `showModal()` opens, `close()` closes and fires
  // `close`.
  showModal() {
    this.open = true;
  }

  show() {
    this.open = true;
  }

  close(returnValue) {
    if (!this.open) {
      return;
    }
    this.open = false;
    this.returnValue = returnValue ?? "";
    this.dispatchEvent(new FakeEvent("close", { bubbles: false, cancelable: false }));
  }

  scrollIntoView() {}

  scrollTo(optionsOrX, y) {
    this.scrollTop = typeof optionsOrX === "object" ? (optionsOrX.top ?? this.scrollTop) : y;
  }
}

for (const [property, attribute] of Object.entries(BOOLEAN_PROPERTIES)) {
  Object.defineProperty(FakeElement.prototype, property, {
    get() {
      return this.hasAttribute(attribute);
    },
    set(value) {
      this.toggleAttribute(attribute, Boolean(value));
    },
  });
}

for (const [property, attribute] of Object.entries(STRING_PROPERTIES)) {
  Object.defineProperty(FakeElement.prototype, property, {
    get() {
      return this.getAttribute(attribute) ?? "";
    },
    set(value) {
      this.setAttribute(attribute, value);
    },
  });
}

// The element children of `node`, as an array for this file's own use.
function elementChildren(node) {
  return node.childNodes.filter((child) => child instanceof FakeElement);
}

function descendants(root) {
  const found = [];
  const visit = (node) => {
    for (const child of elementChildren(node)) {
      found.push(child);
      visit(child);
    }
  };
  visit(root);

  return found;
}

// Selectors: compound selectors of a tag, `#id`, `.class`, `[name]` and
// `[name="value"]`, joined by descendant spaces or `>`, in a comma list.
// Anything else throws, so a test notices instead of matching nothing.
const SELECTOR_TOKEN =
  /\s*(>)\s*|\s+|([a-zA-Z][\w-]*|\*)|#([\w-]+)|\.([\w-]+)|\[([\w-]+)(?:=(?:"([^"]*)"|'([^']*)'|([\w-]+)))?\]/y;

function parseSelector(selector) {
  return selector.split(",").map((part) => parseComplex(part.trim(), selector));
}

function parseComplex(text, selector) {
  const steps = [{ combinator: null, compound: [] }];
  SELECTOR_TOKEN.lastIndex = 0;
  while (SELECTOR_TOKEN.lastIndex < text.length) {
    const start = SELECTOR_TOKEN.lastIndex;
    const match = SELECTOR_TOKEN.exec(text);
    if (!match || match.index !== start) {
      throw new Error("Unsupported selector: " + selector);
    }
    const [, child, tag, id, className, attribute, doubleQuoted, singleQuoted, bare] = match;
    if (child || (!tag && !id && !className && !attribute)) {
      steps.push({ combinator: child ? ">" : " ", compound: [] });
      continue;
    }
    const value = doubleQuoted ?? singleQuoted ?? bare ?? null;
    steps.at(-1).compound.push({ attribute, className, id, tag, value });
  }
  if (steps.some((step) => step.compound.length === 0)) {
    throw new Error("Unsupported selector: " + selector);
  }

  return steps;
}

function matchesCompound(element, compound) {
  return compound.every((part) => {
    if (part.tag) {
      return part.tag === "*" || element.localName === part.tag.toLowerCase();
    }
    if (part.id) {
      return element.id === part.id;
    }
    if (part.className) {
      return element.classList.contains(part.className);
    }

    return part.value === null
      ? element.hasAttribute(part.attribute)
      : element.getAttribute(part.attribute) === part.value;
  });
}

function matchesComplex(element, steps, last = steps.length - 1) {
  if (!matchesCompound(element, steps[last].compound)) {
    return false;
  }
  if (last === 0) {
    return true;
  }
  const isChild = steps[last].combinator === ">";
  for (let node = element.parentNode; node instanceof FakeElement; node = node.parentNode) {
    if (matchesComplex(node, steps, last - 1)) {
      return true;
    }
    if (isChild) {
      return false;
    }
  }

  return false;
}

export class FakeDocument extends FakeNode {
  constructor() {
    super(null);
    this.ownerDocument = this;
    this.defaultView = null;
    this.visibilityState = "visible";
    this.activeElement = null;
  }

  get nodeType() {
    return 9;
  }

  get hidden() {
    return this.visibilityState !== "visible";
  }

  get documentElement() {
    return this.childNodes.find((node) => node instanceof FakeElement) ?? null;
  }

  get head() {
    return this.querySelector("head");
  }

  get body() {
    return this.querySelector("body");
  }

  get children() {
    return new FakeHTMLCollection(elementChildren(this));
  }

  createElement(tagName) {
    return new FakeElement(this, tagName);
  }

  createElementNS(namespaceURI, tagName) {
    return new FakeElement(this, tagName, namespaceURI);
  }

  createTextNode(data) {
    return new FakeText(this, data);
  }

  createDocumentFragment() {
    return new FakeDocumentFragment(this);
  }

  getElementById(id) {
    return descendants(this).find((node) => node.id === id) ?? null;
  }

  querySelector(selector) {
    return this.querySelectorAll(selector)[0] ?? null;
  }

  querySelectorAll(selector) {
    return new FakeNodeList(descendants(this).filter((node) => node.matches(selector)));
  }

  write() {
    refuse("document.write");
  }

  writeln() {
    refuse("document.writeln");
  }
}

function decodeEntities(text) {
  return text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (entity, name) => {
    if (name[0] === "#") {
      const isHex = name[1] === "x" || name[1] === "X";

      return String.fromCodePoint(Number.parseInt(name.slice(isHex ? 2 : 1), isHex ? 16 : 10));
    }

    return ENTITIES[name.toLowerCase()] ?? entity;
  });
}

const TAG = /<(\/?)([a-zA-Z][\w-]*)((?:\s+[^\s"'>/=]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'>]+))?)*)\s*(\/?)>/y;
const ATTRIBUTE = /([^\s"'>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'>]+)))?/g;

// Builds a document from `html`. It reads the markup a page author writes:
// elements, attributes, text, comments and a doctype, with void elements
// and `/>` closing. Text inside `<script>` and `<style>` is kept as is.
export function createDocument(html = "<!DOCTYPE html><html><head></head><body></body></html>") {
  const document = new FakeDocument();
  const open = [document];
  let index = 0;
  while (index < html.length) {
    const next = html.indexOf("<", index);
    const textEnd = next < 0 ? html.length : next;
    if (textEnd > index) {
      appendText(document, open.at(-1), html.slice(index, textEnd));
    }
    if (next < 0) {
      break;
    }
    index = next;
    if (html.startsWith("<!--", index)) {
      const end = html.indexOf("-->", index);
      index = end < 0 ? html.length : end + 3;
      continue;
    }
    if (html.startsWith("<!", index)) {
      const end = html.indexOf(">", index);
      index = end < 0 ? html.length : end + 1;
      continue;
    }
    TAG.lastIndex = index;
    const match = TAG.exec(html);
    if (!match) {
      throw new Error("Unreadable markup at offset " + index);
    }
    index = TAG.lastIndex;
    const [, closing, rawName, rawAttributes, selfClosing] = match;
    const name = rawName.toLowerCase();
    if (closing) {
      const position = open.findLastIndex((node) => node.localName === name);
      if (position < 1) {
        throw new Error("Unmatched </" + name + ">");
      }
      open.length = position;
      continue;
    }
    const element = document.createElement(name);
    for (const attribute of rawAttributes.matchAll(ATTRIBUTE)) {
      const [, attributeName, doubleQuoted, singleQuoted, bare] = attribute;
      element.setAttribute(attributeName, decodeEntities(doubleQuoted ?? singleQuoted ?? bare ?? ""));
    }
    open.at(-1).append(element);
    if (RAW_TEXT_ELEMENTS.has(name)) {
      const end = html.indexOf("</" + name, index);
      const stop = end < 0 ? html.length : end;
      if (stop > index) {
        element.append(html.slice(index, stop));
      }
      index = stop;
      open.push(element);
    } else if (!selfClosing && !VOID_ELEMENTS.has(name)) {
      open.push(element);
    }
  }

  return document;
}

// Whitespace between tags is dropped; other text keeps its spaces.
function appendText(document, parent, raw) {
  if (raw.trim() === "") {
    return;
  }
  parent.append(document.createTextNode(decodeEntities(raw)));
}

// In-memory `Storage`. `failing: true` makes every access throw, as a
// browser does when site data is blocked.
export class MemoryStorage {
  #items = new Map();
  #isFailing;

  constructor({ failing = false } = {}) {
    this.#isFailing = failing;
  }

  #check() {
    if (this.#isFailing) {
      throw new Error("The storage is unavailable");
    }
  }

  get length() {
    this.#check();

    return this.#items.size;
  }

  key(index) {
    this.#check();

    return [...this.#items.keys()][index] ?? null;
  }

  getItem(key) {
    this.#check();

    return this.#items.get(String(key)) ?? null;
  }

  setItem(key, value) {
    this.#check();
    this.#items.set(String(key), String(value));
  }

  removeItem(key) {
    this.#check();
    this.#items.delete(String(key));
  }

  clear() {
    this.#check();
    this.#items.clear();
  }
}

// Returns custom property `name` of `element` as `stylesheet`, such as the
// generated tokens.css, sets it: only rules on the root element count, as
// `:root` or `:root.<class>...`, a rule applies when the root has every
// class it names, and a later rule wins. Without a value it is empty, as
// in a browser.
function customProperty(stylesheet, element, name) {
  let value = "";
  if (element !== element.ownerDocument.documentElement) {
    return value;
  }
  const rules = stylesheet.replace(/\/\*[\s\S]*?\*\//g, "").matchAll(/([^{}]+)\{([^}]*)\}/g);
  for (const [, selector, body] of rules) {
    const [base, ...classes] = selector.trim().split(".");
    if (base !== ":root" || !classes.every((name) => element.classList.contains(name))) {
      continue;
    }
    for (const [, property, declared] of body.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
      if (property === name) {
        value = declared.trim();
      }
    }
  }

  return value;
}

// Builds the window of `document` at `url`. History entries change the
// location; `history.back()` fires `popstate`, and a fragment change
// through `location.hash` fires `hashchange`. Timers are real.
// `getComputedStyle` resolves only the root's custom properties, from
// `stylesheet`.
export function createWindow(
  document,
  url = "http://127.0.0.1/",
  { matches = () => false, stylesheet = "" } = {},
) {
  const window = new FakeEventTarget();
  let current = new URL(url);
  const entries = [{ state: null, url: current.href }];
  let position = 0;

  const navigate = (next, state, replace) => {
    current = new URL(next, current);
    if (replace) {
      entries[position] = { state: state, url: current.href };
    } else {
      entries.splice(position + 1, entries.length, { state: state, url: current.href });
      position += 1;
    }
  };
  const location = {
    get hash() {
      return current.hash;
    },
    set hash(value) {
      const before = current.href;
      const hash = String(value);
      navigate("#" + hash.replace(/^#/, ""), null, false);
      window.dispatchEvent(new FakeEvent("hashchange", { newURL: current.href, oldURL: before }));
    },
    get host() {
      return current.host;
    },
    get hostname() {
      return current.hostname;
    },
    get href() {
      return current.href;
    },
    get origin() {
      return current.origin;
    },
    get pathname() {
      return current.pathname;
    },
    get protocol() {
      return current.protocol;
    },
    get search() {
      return current.search;
    },
    reload() {
      window.dispatchEvent(new FakeEvent("reload"));
    },
  };
  const history = {
    get length() {
      return entries.length;
    },
    get state() {
      return entries[position].state;
    },
    back() {
      history.go(-1);
    },
    forward() {
      history.go(1);
    },
    go(delta) {
      const target = position + delta;
      if (delta === 0 || target < 0 || target >= entries.length) {
        return;
      }
      position = target;
      current = new URL(entries[position].url);
      window.dispatchEvent(new FakeEvent("popstate", { state: entries[position].state }));
    },
    pushState(state, _title, next = current.href) {
      navigate(next, state, false);
    },
    replaceState(state, _title, next = current.href) {
      navigate(next, state, true);
    },
  };

  Object.assign(window, {
    clearInterval: clearInterval,
    clearTimeout: clearTimeout,
    document: document,
    getComputedStyle: (element) => ({
      getPropertyValue: (name) => customProperty(stylesheet, element, name),
    }),
    history: history,
    innerHeight: 800,
    innerWidth: 1280,
    localStorage: new MemoryStorage(),
    location: location,
    matchMedia: (query) => ({
      addEventListener() {},
      matches: matches(query),
      media: query,
      removeEventListener() {},
    }),
    scrollTo() {},
    scrollY: 0,
    sessionStorage: new MemoryStorage(),
    setInterval: setInterval,
    setTimeout: setTimeout,
  });
  window.window = window;
  document.defaultView = window;

  return window;
}
