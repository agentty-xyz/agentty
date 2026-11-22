// Drives the web client's relay modules from Node for the integration tests
// in crates/ag-remote/tests/server/web_client.rs.
//
// Usage: node relay_device.mjs <pair|hello> <pairing link> [message bytes]
//
// Prints one JSON line per step and exits with a failure when the stream
// ends early.

import { generateKeyPair } from "../../assets/noise.mjs";
import { openRelayTransport, parsePairingLink } from "../../assets/relay.mjs";

const PROTOCOL_VERSION = 1;
const SESSION_ID = "session-1";

if (typeof WebSocket === "undefined") {
  throw new Error("Node.js 22 or newer is required: this one has no WebSocket");
}

const [mode, link, messageBytes] = process.argv.slice(2);
const pairing = parsePairingLink(new URL(link).hash);
if (!pairing) {
  throw new Error("Not a pairing link");
}

// Opens a stream as `deviceKey`, sends `firstFrame`, and returns the frames
// that arrive, one per `next()` call.
function connect(deviceKey, firstFrame) {
  const frames = [];
  const waiting = [];
  const deliver = (frame) => {
    const resolve = waiting.shift();
    if (resolve) {
      resolve(frame);
    } else {
      frames.push(frame);
    }
  };
  const transport = openRelayTransport({
    deviceKey: deviceKey,
    instanceKey: pairing.instanceKey,
    onclose: () => deliver(null),
    onmessage: (text) => deliver(JSON.parse(text)),
    onopen: () => transport.send(JSON.stringify(firstFrame)),
    relayUrl: pairing.relayUrl,
  });

  return {
    close: () => transport.close(),
    next: async () => {
      const frame =
        frames.length > 0 ? frames.shift() : await new Promise((resolve) => waiting.push(resolve));
      if (!frame) {
        throw new Error("Stream ended");
      }

      return frame;
    },
    send: (frame) => transport.send(JSON.stringify(frame)),
  };
}

function report(step) {
  process.stdout.write(JSON.stringify(step) + "\n");
}

const hello = { type: "hello", protocol_version: PROTOCOL_VERSION };
const deviceKey = await generateKeyPair();

if (mode === "pair") {
  const first = connect(deviceKey, {
    type: "pair",
    code: pairing.code,
    device_name: "Node device",
    protocol_version: PROTOCOL_VERSION,
  });
  report(await first.next());
  report(await first.next());

  // A message larger than one Noise message crosses the stream in several
  // parts, and comes back the same way in the session snapshot.
  const message = "m".repeat(Number(messageBytes));
  first.send({
    type: "request",
    id: 1,
    request: { method: "send_message", session_id: SESSION_ID, message: message },
  });
  report(await first.next());
  first.send({
    type: "request",
    id: 2,
    request: { method: "get_session", session_id: SESSION_ID },
  });
  const loaded = await first.next();
  report({ echoed: loaded.result.ok.session.queued_messages[0] === message });
  first.close();

  const returning = connect(deviceKey, hello);
  report(await returning.next());
  returning.close();
} else {
  const stranger = connect(deviceKey, hello);
  report(await stranger.next());
  stranger.close();
}
