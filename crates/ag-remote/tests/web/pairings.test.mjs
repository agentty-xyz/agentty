import assert from "node:assert/strict";
import test from "node:test";

import { MemoryStorage } from "./fake_dom.mjs";
import {
  LEGACY_PAIRING_KEY,
  PAIRINGS_KEY,
  forgetPairing,
  instanceIdOf,
  loadPairings,
  savePairing,
} from "../../assets/pairings.mjs";

const FIRST_KEY = "ab".repeat(32);
const SECOND_KEY = "cd".repeat(32);
const RELAY = "wss://relay.example";

async function pairing(instanceKey, relayUrl = RELAY) {
  return { instanceId: await instanceIdOf(instanceKey), instanceKey: instanceKey, relayUrl: relayUrl };
}

test("instanceIdOf is the hexadecimal SHA-256 of the key, as the relay routes", async () => {
  // Arrange
  const zeros = "00".repeat(32);

  // Act
  const id = await instanceIdOf(zeros);

  // Assert
  assert.equal(id, "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925");
});

test("pairings save in the order they were made, replace their own Agentty, and are forgotten one at a time", async () => {
  // Arrange
  const storage = new MemoryStorage();
  const first = await pairing(FIRST_KEY);
  const second = await pairing(SECOND_KEY);

  // Act
  let pairings = savePairing(storage, [], first);
  pairings = savePairing(storage, pairings, second);
  pairings = savePairing(storage, pairings, { ...first, relayUrl: "wss://other.example" });
  const loaded = await loadPairings(storage);
  pairings = forgetPairing(storage, pairings, first.instanceId);
  const afterForget = await loadPairings(storage);
  forgetPairing(storage, pairings, second.instanceId);

  // Assert
  assert.deepEqual(loaded, [{ ...first, relayUrl: "wss://other.example" }, second]);
  assert.deepEqual(afterForget, [second]);
  assert.equal(storage.getItem(PAIRINGS_KEY), null);
});

test("a pairing of an earlier version moves over once and comes first", async () => {
  // Arrange
  const storage = new MemoryStorage();
  const second = await pairing(SECOND_KEY);
  savePairing(storage, [], second);
  storage.setItem(LEGACY_PAIRING_KEY, FIRST_KEY.toUpperCase() + " " + RELAY);

  // Act
  const migrated = await loadPairings(storage);
  const again = await loadPairings(storage);

  // Assert
  assert.deepEqual(migrated, [await pairing(FIRST_KEY), second]);
  assert.deepEqual(again, migrated);
  assert.equal(storage.getItem(LEGACY_PAIRING_KEY), null);
});

test("an earlier pairing stays where it was until the new form is saved", async () => {
  // Arrange
  const storage = new MemoryStorage();
  storage.setItem(LEGACY_PAIRING_KEY, FIRST_KEY + " " + RELAY);
  const setItem = storage.setItem.bind(storage);
  storage.setItem = () => {
    throw new Error("quota");
  };

  // Act
  const loaded = await loadPairings(storage);
  storage.setItem = setItem;

  // Assert
  assert.deepEqual(loaded, [await pairing(FIRST_KEY)]);
  assert.equal(storage.getItem(LEGACY_PAIRING_KEY), FIRST_KEY + " " + RELAY);
});

test("damaged entries are left out, and storage that throws holds nothing", async () => {
  // Arrange
  const damaged = new MemoryStorage();
  const id = await instanceIdOf(FIRST_KEY);
  damaged.setItem(
    PAIRINGS_KEY,
    JSON.stringify({
      [id]: { key: FIRST_KEY, relay: RELAY },
      short: { key: FIRST_KEY, relay: RELAY },
      ["ef".repeat(32)]: { key: "not a key", relay: RELAY },
      ["01".repeat(32)]: { key: SECOND_KEY, relay: "" },
    }),
  );
  damaged.setItem(LEGACY_PAIRING_KEY, "garbage");
  const notJson = new MemoryStorage();
  notJson.setItem(PAIRINGS_KEY, "{not json");
  const list = new MemoryStorage();
  list.setItem(PAIRINGS_KEY, "[]");
  const failing = new MemoryStorage({ failing: true });

  // Act
  const loaded = await loadPairings(damaged);
  const saved = savePairing(failing, [], await pairing(FIRST_KEY));

  // Assert
  assert.deepEqual(loaded, [await pairing(FIRST_KEY)]);
  assert.equal(damaged.getItem(LEGACY_PAIRING_KEY), null);
  assert.deepEqual(await loadPairings(notJson), []);
  assert.deepEqual(await loadPairings(list), []);
  assert.deepEqual(await loadPairings(failing), []);
  // The pairing still lasts for this page.
  assert.equal(saved.length, 1);
  assert.deepEqual(forgetPairing(failing, saved, saved[0].instanceId), []);
});
