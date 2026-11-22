+++
title = "Remote Control"
description = "Follow and steer Agentty sessions from a phone or another computer's browser."
weight = 4
+++

<a id="usage-remote-control"></a> Remote control lets another device follow your
sessions and reply to them while Agentty runs. A paired device can list sessions, read
transcripts, send messages, answer questions, and cancel a session. Canceling a session
stops its agent and deletes its worktree and branch, so changes that are not merged or
published are lost; the device asks before it cancels. While an execution graph waits
for you, a message replies to it as a terminal reply would; see
[Generated Graph Proposals](@/docs/usage/workflow.md#generated-graph-proposals). A
paired device can also stop a running turn, sync a session branch with its base branch,
change a session's settings, create sessions in the active project, merge a session into
its base branch, and publish its branch; the web page offers each of these.

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

## Use the Page

<a id="usage-remote-control-page"></a> The page lists sessions in a sidebar and shows
the open session beside it. On a phone or a narrow window, the menu button opens the
list; the back gesture, `Esc`, or a tap beside the list closes it. The address names the
open session, so the back gesture returns to the previous one and a reload keeps it
open.

The list groups sessions as the terminal does, with the archive collapsed. Type in
`Filter sessions`, or press `/` to reach it, to match titles; `Esc` clears it. The
`All`, `Needs you`, and `Running` buttons narrow the list by status. A dot marks a
session that changed since you last opened it. Each browser tracks what it has seen on
its own, so another device or a private window shows its own dots. Each row also shows
the session's size, changed lines, model, and `PR ready` when its pull request can
merge. With no session open, the page offers the sessions waiting for you.

When this Agentty lets devices create sessions, the sidebar names the active project and
offers `New session`. Home then asks what the agent should work on in the active project
and base branch, with a choice of session type when Agentty offers more than one. Its
message box is the open session's: the line under it shows the new session's model,
reasoning level, answer style, and speed, and when the device may change session
settings, the line and the slash commands change them, with models from the agent that
session type starts on. Because the session has no worktree yet, `@` lists the files in
the active project's checkout, as it does for a draft in the terminal. Sending creates
the session, applies the settings you changed, sends your text as its first message, and
opens it. If the settings or the message do not go through, the new session opens with
your text still in its box.

The header of the open session shows its size and changed lines, the session branch and
its base branch, the model and reasoning level, a link to its pull request, and the
active time while a turn runs. The line under the message box shows the model, reasoning
level, answer style, and speed. An older Agentty sends fewer details, and the page
leaves out what it does not know.

The open session shows its transcript, with each activity summary in a box you can open
or close. While the session works or waits, the transcript ends with the same status
line as the terminal, such as `Working...` with the agent's latest step, or
`Rebasing...`. When you scroll up, new messages leave your place alone and
`Jump to latest` returns to the end. With a keyboard and mouse, `Enter` sends and
`Shift+Enter` adds a line; on a touch screen, `Enter` adds a line and the button sends.
`Ctrl+Enter` or `Cmd+Enter` always sends. While a turn runs, a message waits in the
queue above the box for the next turn. While the agent asks questions, the box gives way
to the questions: pick an option or type an answer, then press `Send answers` once each
has one. A session that is merging or finished shows why you cannot chat. Unsent text
stays in the box for each session until Agentty accepts it, even after a reload of the
tab.

The `⋯` button in the session header offers `Cancel session`. When this Agentty lets
devices stop a turn or merge, the page also shows `Stop` next to the send button while a
turn runs, and `Merge` in the header, or in the `⋯` sheet on a phone. `Stop` returns the
session to review and keeps its changes; `Merge` asks before it merges the session into
its base branch, and names that branch and the changed lines.

When this Agentty lets devices publish, `Publish` sits beside `Merge`, or in the `⋯`
sheet on a phone, while the session is in review. It asks first and names the remote
branch: the one the session was published to before, or the session branch the first
time. Publishing pushes the branch and opens its pull request, or refreshes the one it
has. The button says `Publishing…` until Agentty answers, also when you open another
session meanwhile, and then the page links the pull request under the header or shows
why publishing failed. When this Agentty lets devices sync, the `⋯` sheet offers
`Sync with` the base branch, which asks first and rebases the session branch, after the
running turn if there is one.

When this Agentty lets devices change settings, the line under the message box is a
button. It opens a sheet with the models of the session's agent, the reasoning level,
the answer style, and the speed when the chosen model can run fast. `Apply` sends only
what you changed; during a running turn, the changes apply from the next turn. Typing
`/` at the start of the message box also lists `/model`, `/reasoning`, `/style`, and,
when the model can run fast, `/fast`, as in the terminal: pick one with the arrow keys
and `Enter`, or tap it, then pick a value; `/fast` toggles the speed at once.

When this Agentty lets devices list a session's files, typing `@` at the start of a word
lists the files and directories in the session's worktree, best match first, as in the
terminal. Picking one puts `@path` in the message for the agent to read.

When this Agentty lets devices read diffs, `Diff` in the header, or in the `⋯` sheet on
a phone, shows the session's changed files in place of the chat, each with its status
letter and changed lines. Open a file to read its patch; press `Diff` again to return to
the chat. A very large file list or patch shows only its beginning.

The sidebar footer shows this device and its connection. Its `green` / `dark` switch
picks one of the site's two themes, and the browser remembers the choice. The `⋯` button
next to the device offers `Forget this device`. While the connection is down, a banner
says when the page reconnects, and a message you are typing stays in the box until it is
back.

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
over TLS. Its container image is published for `amd64` and `arm64`: each release tags it
with the version and `latest`, and each commit to `main` tags it `main`. The relay
listens on port `8787`:

```sh
podman run --detach --name agentty-relay --publish 127.0.0.1:8787:8787 \
  ghcr.io/agentty-xyz/agentty-relay:latest --client-address-header X-Real-IP
```

To build the image yourself instead, run this from a checkout of the Agentty repository:

```sh
podman build --file container/relay.Containerfile \
  --ignorefile container/relay.containerignore --tag agentty-relay .
```

Arguments after the image name go to the relay: behind a proxy, pass
`--client-address-header` with the header your proxy sets to the client address, so the
relay's limits count each client separately.

### Run the Relay on Fly.io

<a id="usage-remote-control-relay-fly"></a> Fly.io serves the relay over TLS and limits
connections, so it needs no other proxy. With `flyctl` signed in, create an app and
deploy the published image with `relay.fly.toml` from the repository's
[`container` directory](https://github.com/agentty-xyz/agentty/tree/main/container),
using a checkout or a downloaded copy of that file:

```sh
fly apps create <name>
fly deploy --config container/relay.fly.toml --app <name> --ha=false
```

Keep the app on exactly one machine: the relay joins a device to Agentty only when both
reach the same machine. `--ha=false` creates one instead of Fly's default two; do not
scale it up. Then start Agentty with `--remote-relay wss://<name>.fly.dev`. For your own
domain, run `fly certs add <domain>` and add the DNS records it prints.

### Admit Only Your Instances

<a id="usage-remote-control-relay-admission"></a> A relay registers every Agentty that
proves its key. To serve only your own, pass `--allow-instance` with each instance's
identifier, repeated for several; on Fly.io, add it to the `app` line under
`[processes]` in `relay.fly.toml`. The relay refuses any other instance, which then
shows the relay as `Connecting` and keeps retrying, with waits that grow to 30 seconds.

The identifier is the SHA-256 digest of the instance key that the pairing link carries
after `key=`. This prints it:

```sh
printf '%s' <key> | xxd -r -p | shasum -a 256
```

### Pair Through the Relay

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

### What a paired device can do

<a id="usage-remote-control-device-access"></a> Within the sessions it lists, a paired
device can:

- list sessions and read their transcripts, questions, queued messages, agent and model
  settings, branches, and linked pull request;
- see each listed session's diff size, changed lines, model, active time, merge
  conflicts, and pull request state;
- read a session's diff against its base branch, which shows the repository content the
  session changed;
- list the names of the files and directories in a session's worktree, as `@` lookup in
  the terminal does, without reading their contents;
- list the names of the files and directories in the active project's checkout, for `@`
  lookup in the first message of a new session, without reading their contents;
- send messages, which start, resume, or queue a turn that agents run with edit
  permissions;
- answer an agent's questions;
- stop a running turn, which returns the session to review with its worktree and changes
  kept but discards the messages queued behind the turn;
- sync a session branch, which rebases it onto its base branch and starts an agent turn
  to resolve any conflicts;
- change a session's model, reasoning level, response style, and speed wherever the
  terminal's composer could, choosing among the models `/model` offers for the session's
  agent, including the most expensive model and fast mode; reasoning and style apply at
  once, while a model or speed change waits for the running turn as it does in the
  terminal;
- cancel a session, which deletes its worktree and branch;
- merge a session, which lands its changes on the base branch;
- create a session in the active project, which adds a worktree and branch; and
- publish a session, which pushes its branch and opens or refreshes its pull request
  with this computer's Git and forge credentials.

Anyone holding a paired device can do the same, so remove a device you no longer
control.

### Protections

- A pairing code works once, expires after 10 minutes, and stops working after five
  wrong attempts. Press `n` in the popup for a new code.
- A device becomes paired only after you approve it in the terminal; an unanswered
  request expires after two minutes.
- Each device gets its own key, which Agentty stores only as a hash. Remove a device in
  the popup with `d`; its open connection closes at once. Keys do not expire, so remove
  a device you no longer control.
- A device can read and act on only the sessions it lists, which are the sessions of the
  active project, and creates sessions only in that project. Agentty refuses a request
  for any other session.
- A device that publishes a session pushes it to the branch it was published to before,
  or under the default name the first time, and opens or refreshes its pull request with
  this computer's Git and forge credentials; it cannot choose a branch name. See
  [Forge Authentication](@/docs/usage/forge-authentication.md) for what publishing
  needs.
- Web pages from other sites cannot connect, including pages that point their own domain
  at this computer.
- Agentty does not encrypt the connection itself. Use a tunnel that does, such as
  Tailscale or SSH, when the traffic leaves this computer.

## Limitations

<a id="usage-remote-control-limitations"></a>

- Remote control works only while Agentty is open in a terminal.
- Remote devices see and create the sessions of the active project. A project without a
  current Git branch, such as a detached `HEAD`, cannot take new sessions; a device's
  request for the host details or a new session then fails.
- The transcript updates when a turn finishes; live streaming output is not shown.
- Image attachments cannot be sent from a remote device.
- Answers render as Markdown: headings, lists, quotes, code blocks, tables, and links.
  Mermaid and other diagrams show as code, and images show as links. The page's own
  renderer can differ from the terminal at the edges; `View source` under an answer
  shows its text as the agent wrote it. An answer over 256 KB shows as plain text.
- A browser remembers one relay pairing for each relay address.
