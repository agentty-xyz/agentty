// Relay transport of the remote control client: the pairing link and one
// encrypted stream to an Agentty instance through a relay. The relay only
// sees ciphertext; see noise.mjs.

import { fingerprint, keyFromHex, startHandshake } from "./noise.mjs";

const CONNECT_PATH = "/v1/connect/";

const decoder = new TextDecoder();
const encoder = new TextEncoder();

/**
 * Reads the fragment of a pairing link, `#relay=…&key=…&code=…`. Returns
 * the relay address, the instance key as hexadecimal, and the one-time
 * code, or null when the fragment is not a pairing link. A link without a
 * relay names `defaultRelay`, the relay of a page that names one.
 */
export function parsePairingLink(fragment, defaultRelay = null) {
  const parameters = new URLSearchParams(fragment.replace(/^#/, ""));
  const code = parameters.get("code") || "";
  const instanceKey = (parameters.get("key") || "").toLowerCase();
  const relayUrl = parameters.get("relay") || defaultRelay || "";
  if (!code || !keyFromHex(instanceKey) || !/^wss?:\/\/\S+$/.test(relayUrl)) {
    return null;
  }

  return { code: code, instanceKey: instanceKey, relayUrl: relayUrl };
}

/**
 * Opens an encrypted stream to the instance that holds `instanceKey`
 * (hexadecimal) through the relay at `relayUrl`, as `deviceKey`.
 *
 * `onopen` runs once the stream is encrypted, `onmessage` receives each
 * frame as text, and `onclose` runs once when the stream ends or fails,
 * unless `close` ended it. The returned transport has `send(text)`, which
 * reports whether the frame was accepted, and `close()`.
 */
export function openRelayTransport(options) {
  const transport = new RelayTransport(options);
  transport.start();

  return transport;
}

class RelayTransport {
  constructor({ deviceKey, instanceKey, onclose, onmessage, onopen, relayUrl }) {
    this.deviceKey = deviceKey;
    this.handshake = null;
    this.instanceKey = keyFromHex(instanceKey);
    this.isClosed = false;
    this.onclose = onclose;
    this.onmessage = onmessage;
    this.onopen = onopen;
    // Runs encryption steps one at a time, so messages keep their order.
    this.queue = Promise.resolve();
    this.relayUrl = relayUrl.endsWith("/") ? relayUrl.slice(0, -1) : relayUrl;
    this.session = null;
    this.socket = null;
  }

  // A failed step leaves the encryption state unusable, so it ends the
  // stream.
  enqueue(step) {
    this.queue = this.queue.then(step).catch(() => this.finish());
  }

  start() {
    this.enqueue(async () => {
      const instanceId = await fingerprint(this.instanceKey);
      if (this.isClosed) {
        return;
      }
      const socket = new WebSocket(this.relayUrl + CONNECT_PATH + instanceId);
      socket.binaryType = "arraybuffer";
      socket.onopen = () => this.enqueue(() => this.sendHandshake());
      socket.onmessage = (event) => this.enqueue(() => this.receive(event.data));
      // Queued behind the messages that arrived first, such as the reason
      // for a rejection that the instance sends just before it closes.
      socket.onclose = () => this.enqueue(() => this.finish());
      this.socket = socket;
    });
  }

  async sendHandshake() {
    if (this.isClosed) {
      return;
    }
    this.handshake = await startHandshake(this.deviceKey, this.instanceKey);
    this.socket.send(this.handshake.message);
  }

  async receive(data) {
    if (this.isClosed || !(data instanceof ArrayBuffer)) {
      return;
    }
    if (!this.session) {
      const session = await this.handshake.finish(data);
      if (!this.isClosed) {
        this.session = session;
        this.onopen();
      }

      return;
    }
    const frame = await this.session.open(data);
    if (frame && !this.isClosed) {
      this.onmessage(decoder.decode(frame));
    }
  }

  send(text) {
    if (this.isClosed || !this.session) {
      return false;
    }
    this.enqueue(async () => {
      if (this.isClosed) {
        return;
      }
      for (const message of await this.session.seal(encoder.encode(text))) {
        this.socket.send(message);
      }
    });

    return true;
  }

  close() {
    this.isClosed = true;
    this.socket?.close();
  }

  finish() {
    if (this.isClosed) {
      return;
    }
    this.isClosed = true;
    this.socket?.close();
    this.onclose();
  }
}
