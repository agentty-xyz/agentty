// An in-memory IndexedDB with the one object store the web client uses,
// and the faults a browser can show, for the Node scripts that run the
// client's key storage.
//
// `installFakeIndexedDb()` sets `globalThis.indexedDB` and returns the
// fake's state: `records`, and the switches `failReads`, `failWrites` and
// `losesKeyObjects`.

export function installFakeIndexedDb() {
  const fake = {
    failReads: false,
    failWrites: false,
    // Safari reads stored X25519 key objects back as null (WebKit bug
    // 312279).
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

  return fake;
}
