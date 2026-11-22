// This browser's device key for relay streams, kept in IndexedDB.
//
// The browser stores the key object itself, so page scripts can use the
// private half but never read it. Safari loses X25519 key objects it stores
// (WebKit bug 312279), so there the private half is saved encrypted under a
// second key instead. Page scripts could decrypt that copy, so it is the
// weaker form, and it is replaced as soon as the browser can store the key
// object.

import { generateKeyPair } from "./noise.mjs";

const DATABASE = "agentty-remote";
const STORE = "keys";
// The key pair as key objects.
const DEVICE_KEY_ID = "device";
// The fallback: the private half encrypted under a stored AES key.
const WRAPPED_KEY_ID = "device-wrapped";
const X25519 = { name: "X25519" };

function completed(operation) {
  return new Promise((resolve, reject) => {
    operation.onsuccess = () => resolve(operation.result);
    operation.onerror = () => reject(operation.error);
  });
}

async function store(mode) {
  const opening = globalThis.indexedDB.open(DATABASE, 1);
  opening.onupgradeneeded = () => opening.result.createObjectStore(STORE);
  const database = await completed(opening);

  return database.transaction(STORE, mode).objectStore(STORE);
}

async function read(id) {
  return completed((await store("readonly")).get(id));
}

async function write(id, value) {
  await completed((await store("readwrite")).put(value, id));
}

async function remove(id) {
  await completed((await store("readwrite")).delete(id));
}

// Saves `keyPair` as key objects and reports whether the browser can read
// them back. A failure only means the key objects are not saved.
async function saveKeyObjects(keyPair) {
  try {
    await write(DEVICE_KEY_ID, keyPair);
    const stored = await read(DEVICE_KEY_ID);
    if (stored?.privateKey && stored?.publicKey) {
      return true;
    }
    await remove(DEVICE_KEY_ID);
  } catch (_error) {
    // Reported as not saved below.
  }

  return false;
}

// Creates a key pair and saves its private half encrypted. The private half
// never passes through page memory as bytes, and the returned private key is
// not extractable.
async function createWrapped() {
  const subtle = globalThis.crypto.subtle;
  const created = await subtle.generateKey(X25519, true, ["deriveBits"]);
  const wrappingKey = await subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
    "wrapKey",
    "unwrapKey",
  ]);
  const iv = globalThis.crypto.getRandomValues(new Uint8Array(12));
  const wrapped = {
    iv: iv,
    privateKey: await subtle.wrapKey("pkcs8", created.privateKey, wrappingKey, {
      name: "AES-GCM",
      iv: iv,
    }),
    publicKey: await subtle.exportKey("raw", created.publicKey),
    wrappingKey: wrappingKey,
  };
  await write(WRAPPED_KEY_ID, wrapped);

  return unwrap(wrapped);
}

async function unwrap(wrapped) {
  const subtle = globalThis.crypto.subtle;

  return {
    privateKey: await subtle.unwrapKey(
      "pkcs8",
      wrapped.privateKey,
      wrapped.wrappingKey,
      { name: "AES-GCM", iv: wrapped.iv },
      X25519,
      false,
      ["deriveBits"],
    ),
    publicKey: await subtle.importKey("raw", wrapped.publicKey, X25519, true, []),
  };
}

// Returns the saved key pair, or null when none is saved. Rejects when the
// storage cannot be read, because a saved key may then still exist.
async function readSaved() {
  const stored = await read(DEVICE_KEY_ID);
  if (stored?.privateKey && stored?.publicKey) {
    return stored;
  }
  const wrapped = await read(WRAPPED_KEY_ID);
  if (!wrapped) {
    return null;
  }
  let keyPair;
  try {
    keyPair = await unwrap(wrapped);
  } catch (_error) {
    // A copy that cannot be decrypted is lost; a new key replaces it.
    return null;
  }
  // A browser that has learned to store key objects takes the key over,
  // and the weaker copy goes away. The key works either way.
  if (await saveKeyObjects(keyPair)) {
    await remove(WRAPPED_KEY_ID).catch(() => {});
  }

  return keyPair;
}

// Creates a key pair and saves it in the strongest form the browser keeps.
// Without usable storage the key lasts for this page only.
async function create() {
  const created = await generateKeyPair();
  if (await saveKeyObjects(created)) {
    return created;
  }
  try {
    return await createWrapped();
  } catch (_error) {
    return created;
  }
}

/**
 * Loads this browser's device key, creating and saving it on first use.
 *
 * Resolves to `{ keyPair, origin }`. The origin is `"saved"` for a key that
 * was already stored, `"created"` when none was stored and a new one was
 * made, and `"unreadable"` when the storage could not be read: the key is
 * then a temporary one, and a saved key may still exist.
 */
export async function loadDeviceKey() {
  let saved;
  try {
    saved = await readSaved();
  } catch (_error) {
    return { keyPair: await generateKeyPair(), origin: "unreadable" };
  }
  if (saved) {
    return { keyPair: saved, origin: "saved" };
  }

  return { keyPair: await create(), origin: "created" };
}

/** Deletes the saved device key, which unpairs this browser for good. */
export async function forgetDeviceKey() {
  for (const id of [DEVICE_KEY_ID, WRAPPED_KEY_ID]) {
    try {
      await remove(id);
    } catch (_error) {
      // Nothing was saved when storage is unavailable.
    }
  }
}
