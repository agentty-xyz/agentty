// Deterministic stand-ins for the gallery check's browser: a small DOM that
// `collectPage` can read, a page that runs it against that DOM, and a
// browser process that answers the DevTools protocol over its pipes.

import { EventEmitter } from "node:events";
import { PassThrough } from "node:stream";

import { SELECTORS } from "../gallery-check.mjs";

// An element with fixed computed styles, optional styles in forced states,
// and the selectors it matches beyond the specimen selector.
export class FakeElement {
  constructor(localName, options = {}) {
    this.attributes = options.attributes ?? {};
    this.children = [];
    this.classList = options.classes ?? [];
    this.localName = localName;
    this.parentElement = null;
    this.pseudo = options.pseudo ?? {};
    this.selectors = new Set(options.selectors ?? []);
    this.states = options.states ?? {};
    this.styles = options.styles ?? {};
  }

  append(...children) {
    for (const child of children) {
      child.parentElement = this;
      this.children.push(child);
    }

    return this;
  }

  contains(other) {
    for (let node = other; node; node = node.parentElement) {
      if (node === this) {
        return true;
      }
    }

    return false;
  }

  getAttribute(name) {
    return this.attributes[name] ?? null;
  }

  hasAttribute(name) {
    return name in this.attributes;
  }

  matches(selector) {
    if (selector === SELECTORS.specimens) {
      for (let node = this; node; node = node.parentElement) {
        if (node.hasAttribute("data-specimen")) {
          return true;
        }
      }

      return false;
    }
    if (selector === "input, textarea") {
      return this.localName === "input" || this.localName === "textarea";
    }

    return this.selectors.has(selector);
  }
}

// A computed style: an iterable list of property names with their values.
class FakeStyle {
  constructor(values) {
    this.content = values.content ?? "normal";
    this.values = values;
  }

  [Symbol.iterator]() {
    return Object.keys(this.values)[Symbol.iterator]();
  }

  getPropertyValue(name) {
    return this.values[name];
  }
}

// A window over `body`, whose stylesheets are `sheets`. Forced states and
// a frozen motion style change what `getComputedStyle` returns.
export function fakeWindow(body, sheets = []) {
  const forced = new Map();
  const head = { appended: [], append: (node) => head.appended.push(node) };
  const all = () => {
    const elements = [];
    const walk = (node) => {
      elements.push(node);
      node.children.forEach(walk);
    };
    body.children.forEach(walk);

    return elements;
  };
  const document = {
    createElement: (localName) => ({ localName: localName, textContent: "" }),
    head: head,
    querySelectorAll: (selector) => all().filter((element) => element.matches(selector)),
    styleSheets: sheets,
  };

  return {
    document: document,
    forced: forced,
    getComputedStyle(element, pseudo) {
      if (pseudo) {
        return new FakeStyle(element.pseudo[pseudo] ?? { content: "none" });
      }
      const values = { ...element.styles };
      for (const state of forced.get(element) ?? []) {
        Object.assign(values, element.states[state] ?? {});
      }
      if (head.appended.length > 0) {
        for (const name of Object.keys(values).filter((key) => key.startsWith("transition"))) {
          values[name] = "none";
        }
      }

      return new FakeStyle(values);
    },
  };
}

// A stylesheet owned by a style element, the stylesheet chain when `chain`.
export function fakeSheet(rules, chain = true) {
  return { cssRules: rules, ownerNode: { hasAttribute: (name) => chain && name === "data-gallery-chain" } };
}

// A style rule that sets `declarations`.
export function styleRule(selector, declarations) {
  return { selectorText: selector, style: declarations };
}

// A page whose documents `build(html, viewport)` creates, and which runs
// collectors against them as the browser would, by value.
export class FakePage {
  constructor(build) {
    this.build = build;
    this.forcedCalls = [];
    this.loads = [];
  }

  async load(html, viewport) {
    this.loads.push({ html: html, viewport: viewport });
    this.window = this.build(html, viewport);
    this.elements = this.window.document.querySelectorAll(SELECTORS.specimens);
  }

  async evaluate(collect, options) {
    return JSON.parse(JSON.stringify(collect(this.window, options)));
  }

  async nodes(selector) {
    return this.window.document.querySelectorAll(selector).map((_element, index) => 100 + index);
  }

  async force(nodeId, pseudoClasses) {
    this.forcedCalls.push([nodeId, pseudoClasses]);
    this.window.forced.set(this.elements[nodeId - 100], pseudoClasses);
  }
}

// A browser that opens one fake page and records its closing.
export function fakeBrowser(page) {
  return {
    closed: false,
    async close() {
      this.closed = true;
    },
    async newPage() {
      return page;
    },
  };
}

// A browser process whose pipes carry protocol messages: `answer(message)`
// returns the reply's body, or undefined to stay silent. Replies arrive in
// two chunks, to exercise reassembly.
export function fakeProcess(answer) {
  const child = new EventEmitter();
  const input = new PassThrough();
  const output = new PassThrough();
  child.killed = false;
  child.received = [];
  child.stdio = [null, null, null, input, output];
  child.kill = () => {
    child.killed = true;
    setImmediate(() => child.emit("exit", null, "SIGTERM"));
  };
  let buffer = "";
  input.on("data", (chunk) => {
    buffer += chunk.toString();
    let end = buffer.indexOf("\0");
    while (end >= 0) {
      const message = JSON.parse(buffer.slice(0, end));
      buffer = buffer.slice(end + 1);
      child.received.push(message);
      const reply = answer(message, child);
      if (reply !== undefined) {
        const text = JSON.stringify({ id: message.id, ...reply }) + "\0";
        output.write(text.slice(0, 5));
        output.write(text.slice(5));
      }
      end = buffer.indexOf("\0");
    }
  });

  return child;
}
