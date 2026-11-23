// The instance directory of the remote control client. A web application
// that serves this page may name a directory on its own origin that lists
// the Agenttys of the signed-in account. The page reads it with the
// browser's credentials, names the paired Agenttys by it, and lists the
// ones this browser has not paired, each with how to pair it. Being listed
// grants nothing: pairing still takes the link and the approval in the
// Agentty. Names are untrusted and only ever become text nodes. Functions
// take `document` and `fetch` as parameters, so the Node tests in
// crates/ag-remote/tests/web/directory.test.mjs pass their own.

import { relativeTime } from "./format.mjs";

const INSTANCE_ID = /^[0-9a-f]{64}$/;
// Longest name shown; a longer one is cut.
const MAX_NAME_CHARS = 80;

// The Agenttys a directory answer lists, as `{ instanceId, name,
// lastConnectedAt }` with the time in seconds or `null`. Entries that do
// not parse are left out, and an Agentty listed twice keeps its first
// entry.
export function parseDirectory(body) {
  const entries = [];
  for (const item of Array.isArray(body?.instances) ? body.instances : []) {
    const name = typeof item?.name === "string" ? item.name.trim().slice(0, MAX_NAME_CHARS) : "";
    if (
      typeof item?.instance_id !== "string" ||
      !INSTANCE_ID.test(item.instance_id) ||
      name === "" ||
      entries.some((entry) => entry.instanceId === item.instance_id)
    ) {
      continue;
    }
    const time = typeof item.last_connected_at === "string" ? Date.parse(item.last_connected_at) : NaN;
    entries.push({
      instanceId: item.instance_id,
      lastConnectedAt: Number.isFinite(time) ? Math.floor(time / 1000) : null,
      name: name,
    });
  }

  return entries;
}

// Reads the directory at `url` with `fetch`. Resolves to `{ status:
// "listed", entries }`, `{ status: "signed_out" }` for a `401` or a
// redirect, as to a sign-in page, or `{ status: "failed" }` for anything
// else.
export async function loadDirectory(fetch, url) {
  let response;
  try {
    response = await fetch(url, {
      cache: "no-store",
      credentials: "same-origin",
      headers: { accept: "application/json" },
      redirect: "manual",
    });
  } catch (_error) {
    // The page is offline or the directory unreachable; the list says so.
    return { status: "failed" };
  }
  const isRedirect = response.type === "opaqueredirect" || (response.status >= 300 && response.status < 400);
  if (response.status === 401 || isRedirect) {
    return { status: "signed_out" };
  }
  if (!response.ok) {
    return { status: "failed" };
  }
  try {
    return { entries: parseDirectory(await response.json()), status: "listed" };
  } catch (_error) {
    // An answer that is not JSON lists nothing; the list says so.
    return { status: "failed" };
  }
}

// What the page says about a directory that could not be listed.
export function directoryError(status) {
  if (status === "signed_out") {
    return "You are signed out. Sign in again to see your Agenttys.";
  }

  return status === "failed" ? "Could not list your Agenttys." : "";
}

// When an Agentty last reached the relay, as `nowSeconds` sees it.
export function lastConnectedText(lastConnectedAt, nowSeconds) {
  if (lastConnectedAt === null) {
    return "Never connected";
  }
  const ago = relativeTime(lastConnectedAt, nowSeconds);

  return ago === "now" ? "Connected just now" : "Last connected " + ago + " ago";
}

// Lists `entries` in `list`, each with its name, when it last connected,
// and how to pair it.
export function renderDirectory(document, list, entries, nowSeconds) {
  list.replaceChildren(
    ...entries.map((entry) => {
      const item = element(document, "li", "directory-item");
      item.dataset.instanceId = entry.instanceId;
      const name = element(document, "span", "directory-name", entry.name);
      name.title = entry.name;
      item.append(
        name,
        element(document, "span", "directory-meta", lastConnectedText(entry.lastConnectedAt, nowSeconds)),
        element(document, "span", "directory-hint", "To pair, use the pairing link from its Remote tab."),
      );

      return item;
    }),
  );
}

function element(document, tag, className, text) {
  const node = document.createElement(tag);
  node.className = className;
  if (text !== undefined) {
    node.textContent = text;
  }

  return node;
}
