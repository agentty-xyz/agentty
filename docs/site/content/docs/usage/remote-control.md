+++
title = "Remote Control"
description = "Follow and steer Agentty sessions from a phone or another computer's browser."
weight = 4
+++

<a id="usage-remote-control"></a> Remote control lets another device follow your
sessions and reply to them while Agentty runs. A paired device can list sessions, read
transcripts, send messages, answer questions, and stop a running turn. Merging,
publishing, and session settings stay in the terminal.

Remote control is a preview feature. Start Agentty with `--experimental-remote` to offer
`/remote-control`.

<!-- more -->

## Start Remote Control

<a id="usage-remote-control-start"></a>

1. Start Agentty with `agentty --experimental-remote`.
1. Open a session and type `/remote-control`.
1. The popup shows the address Agentty listens on and a one-time pairing code.
1. Open the address on the other device, enter the code, and name the device.

The device stays paired, so later visits connect without a code. The status bar shows
`Remote control on` and the number of connected devices while the listener runs.

Remote control stays off until you start it, and it stops when you press `s` in the
popup or quit Agentty. It does not start again on the next launch. See
[Keybindings](@/docs/usage/keybindings.md#remote-control-popup) for the popup keys.

## Reach Agentty From Another Device

<a id="usage-remote-control-reach"></a> Agentty listens only on `127.0.0.1`, by default
on port `4646`. A browser on the same computer can open the address directly. Other
devices need a tunnel that forwards that port, such as:

- `tailscale serve 4646` on a Tailscale network.
- `ssh -L 4646:127.0.0.1:4646 <host>` from a computer that can reach this one over SSH.
- `cloudflared tunnel --url http://127.0.0.1:4646`.

Add `--remote-control-port <PORT>` to use another port, or `0` to let the system choose
a free one.

## Security

<a id="usage-remote-control-security"></a> A paired device can send prompts that agents
run with the session's permissions, so treat a pairing like a login.

- A pairing code works once, expires after 10 minutes, and stops working after five
  wrong attempts. Press `n` in the popup for a new code.
- Each device gets its own key, which Agentty stores only as a hash. Remove a device in
  the popup with `d`; its open connection closes at once.
- Agentty does not encrypt the connection itself. Use a tunnel that does, such as
  Tailscale or SSH, when the traffic leaves this computer.

## Limitations

<a id="usage-remote-control-limitations"></a>

- Remote control works only while Agentty is open in a terminal.
- Remote devices see the sessions of the active project.
- The transcript updates when a turn finishes; live streaming output is not shown.
- Image attachments cannot be sent from a remote device.
