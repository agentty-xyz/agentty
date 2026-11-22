// Device side of the remote-control encryption: the Noise IK handshake and
// the encrypted session it produces, on the browser's WebCrypto.
//
// This must stay byte-compatible with crates/ag-noise, which runs the
// instance side. Private keys stay inside non-extractable CryptoKey objects.

const PROTOCOL_NAME = "Noise_IK_25519_AESGCM_SHA256";
// Must match DEVICE_PROLOGUE in crates/ag-noise/src/handshake.rs.
const PROLOGUE = "agentty-remote-device/1";
const HASH_BYTES = 32;
const KEY_BYTES = 32;
const TAG_BYTES = 16;
// Largest Noise message, and the frame bytes one message carries after its
// marker byte.
const MAX_MESSAGE_BYTES = 65535;
const CHUNK_BYTES = MAX_MESSAGE_BYTES - TAG_BYTES - 1;
const FINAL_CHUNK = 1;
const MORE_CHUNKS = 0;
// Largest frame accepted from the instance; a session transcript fits.
const MAX_FRAME_BYTES = 64 * 1024 * 1024;
const EMPTY = new Uint8Array(0);

const subtle = globalThis.crypto?.subtle;
const encoder = new TextEncoder();

/** Whether this browser can run the handshake at all. */
export async function isSupported() {
  try {
    await generateKeyPair();

    return true;
  } catch (_error) {
    return false;
  }
}

/** Creates a key pair whose private half scripts can use but never read. */
export function generateKeyPair() {
  return subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]);
}

/** Returns the raw 32 bytes of a public CryptoKey. */
export async function publicKeyBytes(publicKey) {
  return new Uint8Array(await subtle.exportKey("raw", publicKey));
}

/** Decodes a hexadecimal key of exactly 32 bytes, or returns null. */
export function keyFromHex(text) {
  if (!/^[0-9a-f]{64}$/i.test(text)) {
    return null;
  }
  const bytes = new Uint8Array(KEY_BYTES);
  for (let index = 0; index < KEY_BYTES; index += 1) {
    bytes[index] = Number.parseInt(text.slice(index * 2, index * 2 + 2), 16);
  }

  return bytes;
}

/** The identifier a relay routes by: the SHA-256 of the key, as hex. */
export async function fingerprint(keyBytes) {
  const digest = await sha256(keyBytes);

  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function concat(...parts) {
  const joined = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    joined.set(part, offset);
    offset += part.length;
  }

  return joined;
}

async function sha256(data) {
  return new Uint8Array(await subtle.digest("SHA-256", data));
}

async function hmac(keyBytes, data) {
  const key = await subtle.importKey(
    "raw",
    keyBytes,
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );

  return new Uint8Array(await subtle.sign("HMAC", key, data));
}

// Noise HKDF with two outputs.
async function hkdf(chainingKey, input) {
  const temporary = await hmac(chainingKey, input);
  const first = await hmac(temporary, Uint8Array.of(1));
  const second = await hmac(temporary, concat(first, Uint8Array.of(2)));

  return [first, second];
}

async function diffieHellman(privateKey, publicBytes) {
  const publicKey = await subtle.importKey("raw", publicBytes, { name: "X25519" }, false, []);

  return new Uint8Array(
    await subtle.deriveBits({ name: "X25519", public: publicKey }, privateKey, KEY_BYTES * 8),
  );
}

// One direction of AES-256-GCM with the Noise nonce: four zero bytes, then
// the message counter as a big-endian 64-bit integer.
class CipherState {
  constructor(key) {
    this.key = key;
    this.counter = 0;
  }

  static async fromBytes(keyBytes) {
    return new CipherState(
      await subtle.importKey("raw", keyBytes, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]),
    );
  }

  // The counter advances before the first await, so calls made in order
  // use nonces in order.
  parameters(associatedData) {
    const iv = new Uint8Array(12);
    new DataView(iv.buffer).setBigUint64(4, BigInt(this.counter));
    this.counter += 1;

    return { name: "AES-GCM", iv: iv, additionalData: associatedData, tagLength: TAG_BYTES * 8 };
  }

  async encrypt(associatedData, plaintext) {
    const parameters = this.parameters(associatedData);

    return new Uint8Array(await subtle.encrypt(parameters, this.key, plaintext));
  }

  async decrypt(associatedData, ciphertext) {
    const parameters = this.parameters(associatedData);

    return new Uint8Array(await subtle.decrypt(parameters, this.key, ciphertext));
  }
}

// Handshake hash and chaining key, as the Noise specification names them.
class SymmetricState {
  constructor(hash) {
    this.chainingKey = hash;
    this.cipher = null;
    this.hash = hash;
  }

  static async create() {
    // A protocol name no longer than the hash is zero-padded, not hashed.
    const hash = new Uint8Array(HASH_BYTES);
    hash.set(encoder.encode(PROTOCOL_NAME));
    const state = new SymmetricState(hash);
    await state.mixHash(encoder.encode(PROLOGUE));

    return state;
  }

  async mixHash(data) {
    this.hash = await sha256(concat(this.hash, data));
  }

  async mixKey(input) {
    const [chainingKey, keyBytes] = await hkdf(this.chainingKey, input);
    this.chainingKey = chainingKey;
    this.cipher = await CipherState.fromBytes(keyBytes);
  }

  async encryptAndHash(plaintext) {
    const ciphertext = await this.cipher.encrypt(this.hash, plaintext);
    await this.mixHash(ciphertext);

    return ciphertext;
  }

  async decryptAndHash(ciphertext) {
    const plaintext = await this.cipher.decrypt(this.hash, ciphertext);
    await this.mixHash(ciphertext);

    return plaintext;
  }

  async split() {
    const [first, second] = await hkdf(this.chainingKey, EMPTY);

    return [await CipherState.fromBytes(first), await CipherState.fromBytes(second)];
  }
}

/**
 * Starts the device side of the handshake.
 *
 * `deviceKey` is the device's CryptoKeyPair and `instanceKey` the instance's
 * raw public key. Resolves to the first message to send and a `finish`
 * function that takes the instance's answer and resolves to a Session. Any
 * failure rejects; the connection must then be dropped.
 */
export async function startHandshake(deviceKey, instanceKey) {
  const state = await SymmetricState.create();
  await state.mixHash(instanceKey);

  // -> e, es, s, ss
  const ephemeral = await generateKeyPair();
  const ephemeralPublic = await publicKeyBytes(ephemeral.publicKey);
  await state.mixHash(ephemeralPublic);
  await state.mixKey(await diffieHellman(ephemeral.privateKey, instanceKey));
  const sealedDevice = await state.encryptAndHash(await publicKeyBytes(deviceKey.publicKey));
  await state.mixKey(await diffieHellman(deviceKey.privateKey, instanceKey));
  const sealedPayload = await state.encryptAndHash(EMPTY);

  return {
    message: concat(ephemeralPublic, sealedDevice, sealedPayload),
    // <- e, ee, se
    finish: async (answer) => {
      const bytes = new Uint8Array(answer);
      if (bytes.length !== KEY_BYTES + TAG_BYTES) {
        throw new Error("Unexpected handshake answer");
      }
      const instanceEphemeral = bytes.subarray(0, KEY_BYTES);
      await state.mixHash(instanceEphemeral);
      await state.mixKey(await diffieHellman(ephemeral.privateKey, instanceEphemeral));
      await state.mixKey(await diffieHellman(deviceKey.privateKey, instanceEphemeral));
      await state.decryptAndHash(bytes.subarray(KEY_BYTES));
      const [sending, receiving] = await state.split();

      return new Session(sending, receiving);
    },
  };
}

/**
 * Encrypted channel after the handshake. A frame of any size travels as
 * ordered Noise messages, each starting with a byte that says whether the
 * frame continues. Call `seal` and `open` one at a time, in order.
 */
export class Session {
  constructor(sending, receiving) {
    this.pending = [];
    this.pendingBytes = 0;
    this.receiving = receiving;
    this.sending = sending;
  }

  /** Encrypts one frame into the messages to send, in order. */
  async seal(frame) {
    const messages = [];
    let offset = 0;
    do {
      const chunk = frame.subarray(offset, offset + CHUNK_BYTES);
      offset += chunk.length;
      const marker = offset < frame.length ? MORE_CHUNKS : FINAL_CHUNK;
      messages.push(await this.sending.encrypt(EMPTY, concat(Uint8Array.of(marker), chunk)));
    } while (offset < frame.length);

    return messages;
  }

  /**
   * Decrypts one received message. Resolves to the frame once its last
   * message arrives, and to null before that. A rejection means the session
   * must be dropped.
   */
  async open(message) {
    const payload = await this.receiving.decrypt(EMPTY, new Uint8Array(message));
    const marker = payload[0];
    const chunk = payload.subarray(1);
    if (marker !== FINAL_CHUNK && marker !== MORE_CHUNKS) {
      throw new Error("Invalid message marker");
    }
    if (this.pendingBytes + chunk.length > MAX_FRAME_BYTES) {
      throw new Error("Frame too large");
    }
    this.pending.push(chunk);
    this.pendingBytes += chunk.length;
    if (marker === MORE_CHUNKS) {
      return null;
    }
    const frame = concat(...this.pending);
    this.pending = [];
    this.pendingBytes = 0;

    return frame;
  }
}
