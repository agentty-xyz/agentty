// The relay pairings this browser keeps, one for each Agentty it paired
// with, keyed by instance identifier: the hexadecimal SHA-256 digest of the
// instance key, which is also how the relay routes to the instance. Each
// holds the instance key and the relay address. Earlier versions of the
// page kept a single pairing under another key; reading the pairings moves
// it over, so a browser paired before keeps its pairing.
//
// Functions take the storage, so the Node tests in
// crates/ag-remote/tests/web/pairings.test.mjs pass their own. Storage that
// throws, as in some private windows, keeps the pairings for this page
// only: the caller holds them in memory as well.

import { fingerprint, keyFromHex } from "./noise.mjs";

export const PAIRINGS_KEY = "agentty.remote.pairings";
// The single pairing of earlier versions: the instance key and the relay
// address with a space between them.
export const LEGACY_PAIRING_KEY = "agentty.remote.relay";

const INSTANCE_ID = /^[0-9a-f]{64}$/;

// Returns the instance identifier of `instanceKey`, given as hexadecimal.
export async function instanceIdOf(instanceKey) {
  return fingerprint(keyFromHex(instanceKey));
}

// Reads the stored pairings in the order they were made, as
// `{ instanceId, instanceKey, relayUrl }`, after moving a pairing of an
// earlier version over. Entries that do not parse are left out.
export async function loadPairings(storage) {
  const stored = parsePairings(read(storage, PAIRINGS_KEY));
  const legacy = read(storage, LEGACY_PAIRING_KEY);
  if (legacy === null) {
    return stored;
  }
  const [instanceKey, relayUrl] = legacy.split(" ");
  if (instanceKey && relayUrl && keyFromHex(instanceKey.toLowerCase())) {
    const migrated = {
      instanceId: await instanceIdOf(instanceKey.toLowerCase()),
      instanceKey: instanceKey.toLowerCase(),
      relayUrl: relayUrl,
    };
    // The pairing of an earlier version came first.
    const pairings = [migrated, ...stored.filter((pairing) => pairing.instanceId !== migrated.instanceId)];
    if (!write(storage, PAIRINGS_KEY, serialize(pairings))) {
      // The old copy stays until the new one is saved.
      return pairings;
    }
    remove(storage, LEGACY_PAIRING_KEY);

    return pairings;
  }
  remove(storage, LEGACY_PAIRING_KEY);

  return stored;
}

// Saves `pairing`, replacing a stored one for the same instance in place,
// and returns the pairings now stored. The stored pairings are read again
// first, so another tab's changes stay. When storage cannot be read or
// written, `pairings`, the ones this page holds, stand in for them, and
// the change lasts for this page only.
export function savePairing(storage, pairings, pairing) {
  const add = (list) => {
    const index = list.findIndex((known) => known.instanceId === pairing.instanceId);

    return index < 0 ? [...list, pairing] : list.with(index, pairing);
  };
  const next = add(latest(storage, pairings));

  return write(storage, PAIRINGS_KEY, serialize(next)) ? next : add(pairings);
}

// Removes the pairing with `instanceId` and returns the pairings left,
// keeping another tab's changes as `savePairing` does.
export function forgetPairing(storage, pairings, instanceId) {
  const drop = (list) => list.filter((pairing) => pairing.instanceId !== instanceId);
  const next = drop(latest(storage, pairings));
  const isSaved =
    next.length === 0 ? remove(storage, PAIRINGS_KEY) : write(storage, PAIRINGS_KEY, serialize(next));

  return isSaved ? next : drop(pairings);
}

// The pairings stored now, or `fallback` when storage cannot be read.
function latest(storage, fallback) {
  try {
    return parsePairings(storage.getItem(PAIRINGS_KEY));
  } catch (_error) {
    return fallback;
  }
}

// The stored map, `{ "<instance id>": { key, relay } }`, as a list in
// insertion order.
function parsePairings(text) {
  let parsed;
  try {
    parsed = JSON.parse(text ?? "null");
  } catch (_error) {
    return [];
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return [];
  }
  const pairings = [];
  for (const [instanceId, entry] of Object.entries(parsed)) {
    if (
      INSTANCE_ID.test(instanceId) &&
      typeof entry?.key === "string" &&
      keyFromHex(entry.key) &&
      typeof entry.relay === "string" &&
      entry.relay !== ""
    ) {
      pairings.push({ instanceId: instanceId, instanceKey: entry.key, relayUrl: entry.relay });
    }
  }

  return pairings;
}

function serialize(pairings) {
  const map = {};
  for (const pairing of pairings) {
    map[pairing.instanceId] = { key: pairing.instanceKey, relay: pairing.relayUrl };
  }

  return JSON.stringify(map);
}

function read(storage, key) {
  try {
    return storage.getItem(key);
  } catch (_error) {
    return null;
  }
}

function write(storage, key, value) {
  try {
    storage.setItem(key, value);

    return true;
  } catch (_error) {
    return false;
  }
}

// Removes `key` and reports whether storage took the change.
function remove(storage, key) {
  try {
    storage.removeItem(key);

    return true;
  } catch (_error) {
    return false;
  }
}
