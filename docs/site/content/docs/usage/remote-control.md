+++
title = "Remote Control"
description = "Follow and steer Agentty sessions from a phone or another computer's browser."
weight = 4
+++

<a id="usage-remote-control"></a> Remote control lets another device follow your
sessions and reply to them while Agentty runs. A paired device can list sessions, read
transcripts, send messages, answer questions, and stop a running turn. While an
execution graph waits for you, a message replies to it as a terminal reply would; see
[Generated Graph Proposals](@/docs/usage/workflow.md#generated-graph-proposals).
Merging, publishing, and session settings stay in the terminal.

Remote control is a preview feature. Start Agentty with `--experimental-remote` to offer
`/remote-control`.

<!-- more -->

## Start Remote Control

<a id="usage-remote-control-start"></a>

1. Start Agentty with `agentty --experimental-remote`.
1. Open a session and type `/remote-control`.
1. The popup shows the address Agentty listens on and a one-time pairing code.
1. Open the address on the other device, enter the code, and name the device.
1. The device shows a four-character confirmation code, and the popup shows a pairing
   request with the same code. Press `y` to approve it only when the codes match, or `x`
   to deny it.

The device stays paired, so later visits connect without a code. The status bar shows
`Remote control on` and the number of connected devices while the listener runs, and
asks you to approve when a device waits to pair.

Remote control stays off until you start it, and it stops when you press `s` in the
popup or quit Agentty. It does not start again on the next launch. See
[Keybindings](@/docs/usage/keybindings.md#remote-control-popup) for the popup keys.

## Reach Agentty From Another Device

<a id="usage-remote-control-reach"></a> Agentty listens only on `127.0.0.1`, by default
on port `4646`. A browser on the same computer can open the address directly. Other
devices need a private tunnel that forwards that port, such as:

- `ssh -L 4646:127.0.0.1:4646 <host>` from a computer that can reach this one over SSH.
- `tailscale serve 4646` on a Tailscale network.

Agentty accepts requests only for `localhost`, `127.0.0.1`, and `::1`. When a tunnel
reaches it under another name, such as a Tailscale machine name, add
`--remote-control-host <HOST>` for that name; repeat the flag for several. Avoid public
tunnels: anyone who finds the address can try to pair.

Add `--remote-control-port <PORT>` to use another port, or `0` to let the system choose
a free one.

## Connect Through a Relay

<a id="usage-remote-control-relay"></a> A relay lets devices reach Agentty without a
tunnel: Agentty connects out to the relay, and devices connect to it too. Start Agentty
with `--remote-relay <URL>`, such as `--remote-relay wss://relay.example`. No hosted
relay is offered yet, so run your own `ag-relay` behind a reverse proxy that serves it
over TLS. From a checkout of the Agentty repository,
`podman build --file container/relay.Containerfile --tag agentty-relay .` builds its
container image, which listens on port `8787`.

With a relay, the popup also shows the relay state, a pairing link, and a QR code of the
link when the terminal is large enough. Scan the code or open the link on the other
device. The relay's pairing page opens with the code filled in: name the device, press
`Pair`, then approve the pairing as usual. On this computer, press `o` to open the link;
press `c` to copy it for another device. Copying uses the terminal clipboard, which
`tmux` passes on only with `set-clipboard on`; otherwise select the link. The link
carries the pairing code, so it works once.

The browser remembers the pairing, so later visits to the relay's address connect
without a link. Pairing through a relay needs an up-to-date browser, from 2025 or later,
and a relay served over HTTPS.

Traffic through the relay is encrypted between the device and Agentty. The relay sees
when devices connect and how much they send, but not what they send. The relay also
serves the pairing page, so use a relay you trust or run your own.

A device paired through a relay proves itself with its own key instead of a stored
token. The browser keeps that key where scripts on the page cannot read it. Safari,
including every browser on iPhone and iPad, cannot store such a key yet, so there the
key is kept in a form that a script running in the page could read; the page switches to
the stronger form by itself once Safari supports it. Agentty keeps its relay key in its
database; removing it unpairs every relay device.

## Security

<a id="usage-remote-control-security"></a> A paired device can send prompts that agents
run with the session's permissions, so treat a pairing like a login.

- A pairing code works once, expires after 10 minutes, and stops working after five
  wrong attempts. Press `n` in the popup for a new code.
- A device becomes paired only after you approve it in the terminal; an unanswered
  request expires after two minutes.
- Each device gets its own key, which Agentty stores only as a hash. Remove a device in
  the popup with `d`; its open connection closes at once. Keys do not expire, so remove
  a device you no longer control.
- Web pages from other sites cannot connect, including pages that point their own domain
  at this computer.
- Agentty does not encrypt the connection itself. Use a tunnel that does, such as
  Tailscale or SSH, when the traffic leaves this computer.

## Limitations

<a id="usage-remote-control-limitations"></a>

- Remote control works only while Agentty is open in a terminal.
- Remote devices see the sessions of the active project.
- The transcript updates when a turn finishes; live streaming output is not shown.
- Image attachments cannot be sent from a remote device.
- A browser remembers one relay pairing for each relay address.
