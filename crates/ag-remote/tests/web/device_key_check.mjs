// Exercises the web client's device key storage in Node with an in-memory
// IndexedDB, for the integration tests in
// crates/ag-remote/tests/server/web_client.rs.
//
// Prints one JSON line per scenario. The scenarios run in order and build on
// each other's stored state.

import { forgetDeviceKey, loadDeviceKey } from "../../assets/device_key.mjs";

// The one object store the client uses, with the faults a browser can show.
const fake = {
  failReads: false,
  failWrites: false,
  // Safari reads stored X25519 key objects back as null (WebKit bug 312279).
  losesKeyObjects: false,
  records: new Map(),
};

function operation(run) {
  const request = {};
  queueMicrotask(() => {
    try {
      request.result = run();
      request.onupgradeneeded?.();
      request.onsuccess?.();
    } catch (error) {
      request.error = error;
      request.onerror?.();
    }
  });

  return request;
}

const objectStore = {
  delete: (id) =>
    operation(() => {
      if (fake.failWrites) {
        throw new Error("write failed");
      }
      fake.records.delete(id);
    }),
  get: (id) =>
    operation(() => {
      if (fake.failReads) {
        throw new Error("read failed");
      }
      const value = fake.records.get(id);
      const isKeyObject = value?.privateKey?.algorithm?.name === "X25519";

      return fake.losesKeyObjects && isKeyObject ? null : value;
    }),
  put: (value, id) =>
    operation(() => {
      if (fake.failWrites) {
        throw new Error("write failed");
      }
      fake.records.set(id, value);
    }),
};

globalThis.indexedDB = {
  open: () =>
    operation(() => ({
      createObjectStore: () => {},
      transaction: () => ({ objectStore: () => objectStore }),
    })),
};

async function load() {
  const { keyPair, origin } = await loadDeviceKey();
  const publicKey = new Uint8Array(await crypto.subtle.exportKey("raw", keyPair.publicKey));

  return {
    extractable: keyPair.privateKey.extractable,
    key: Array.from(publicKey, (byte) => byte.toString(16).padStart(2, "0")).join(""),
    origin: origin,
  };
}

function stored() {
  return [...fake.records.keys()].sort();
}

function report(scenario, outcome) {
  process.stdout.write(JSON.stringify({ scenario: scenario, ...outcome }) + "\n");
}

// A browser that stores key objects keeps the key as created.
const created = await load();
const reloaded = await load();
report("key_objects", {
  extractable: reloaded.extractable,
  origins: [created.origin, reloaded.origin],
  sameKey: created.key === reloaded.key,
  stored: stored(),
});

// A browser that loses key objects keeps an encrypted copy instead.
fake.records.clear();
fake.losesKeyObjects = true;
const wrapped = await load();
const unwrapped = await load();
report("fallback", {
  extractable: unwrapped.extractable,
  origins: [wrapped.origin, unwrapped.origin],
  sameKey: wrapped.key === unwrapped.key,
  stored: stored(),
});

// A failed write while trying to upgrade does not cost the working key.
fake.failWrites = true;
const duringWriteFailure = await load();
fake.failWrites = false;
report("write_failure", {
  origin: duringWriteFailure.origin,
  sameKey: duringWriteFailure.key === wrapped.key,
});

// Unreadable storage yields a temporary key and leaves the saved one alone.
fake.failReads = true;
const unreadable = await load();
fake.failReads = false;
const recovered = await load();
report("read_failure", {
  origins: [unreadable.origin, recovered.origin],
  recoveredSameKey: recovered.key === wrapped.key,
  stored: stored(),
  temporaryKeyDiffers: unreadable.key !== wrapped.key,
});

// Once the browser stores key objects, the same key moves over.
fake.losesKeyObjects = false;
const upgraded = await load();
report("upgrade", {
  extractable: upgraded.extractable,
  origin: upgraded.origin,
  sameKey: upgraded.key === wrapped.key,
  stored: stored(),
});

// An encrypted copy that cannot be decrypted is replaced by a new key.
fake.records.clear();
fake.losesKeyObjects = true;
const beforeDamage = await load();
fake.records.get("device-wrapped").wrappingKey = null;
const replacement = await load();
const afterReplacement = await load();
report("broken_copy", {
  newKey: replacement.key !== beforeDamage.key,
  origins: [replacement.origin, afterReplacement.origin],
  sameKey: replacement.key === afterReplacement.key,
});

// Forgetting removes every copy.
await forgetDeviceKey();
const storedAfterForget = stored();
const afterForget = await load();
report("forget", {
  newKey: afterForget.key !== replacement.key,
  origin: afterForget.origin,
  storedAfterForget: storedAfterForget,
});

// Storage that takes no writes still yields a key for this page.
fake.records.clear();
fake.failWrites = true;
const unsaved = await load();
report("unsaved", { origin: unsaved.origin, stored: stored() });
