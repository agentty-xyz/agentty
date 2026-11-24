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
its base branch, publish its branch, sync the active project's branch with its remote,
ask for a graph proposal with `/graph`, and act on an execution graph as the terminal's
graph inspector does; the web page offers each of these.

Remote control is a preview feature. Start Agentty with `--experimental-remote`, or with
`--remote-relay <URL>` alone to [use a relay](#usage-remote-control-relay), to add the
`Remote` tab to the right of `Settings`.

<!-- more -->

## Start Remote Control

<a id="usage-remote-control-start"></a>

1. Start Agentty with `agentty --experimental-remote`.
1. Press `Tab` in a list until the `Remote` tab opens, then press `Enter`.
1. The tab shows the address Agentty listens on and a one-time pairing code.
1. Open the address on the other device, enter the code, and name the device.
1. The device shows a four-character confirmation code, and the tab shows a pairing
   request with the same code. Press `y` to approve it only when the codes match, or `x`
   to deny it.

The device stays paired, so later visits connect without a code. While the listener
runs, the tab reads `Remote (1 online)` with the number of connected devices. Pairing
requests appear only in the tab, so keep it open while you pair a device.

The tab belongs to the active project: its title names the project whose sessions paired
devices follow, and it lists the paired devices. Opening the tab while the listener runs
replaces the pairing code, because a code works once.

Remote control stays off until you start it, and it stops when you press `Enter` in the
tab again or quit Agentty. It does not start again on the next launch. See
[Keybindings](@/docs/usage/keybindings.md#remote) for the tab's keys.

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
the session's size and model. After the status, `↑` marks a published branch, and
`⊙ #12`, `✓ #12`, or `✗ #12` its open, merged, or closed pull request, as in the
terminal's status column. In place of the model, `merge conflict` marks a session whose
branch conflicts with its base, and `PR ready` or `PR ready (bypass)` a pull request
that can merge; see [Project Sync](@/docs/usage/workflow.md#project-sync) for what the
terminal's same labels mean. With no session open, the page offers the sessions waiting
for you, with their changed lines.

The top of the sidebar names the active project, its branch, and how far that branch is
from its remote: `↓2 ↑1` means two commits to pull and one to push, and `✓` means none.
When this Agentty lets devices sync the project, `Sync` beside it pulls and pushes that
branch as `s` does in the terminal. The line under it follows the sync and shows its
result for a few seconds. A sync that cannot start, such as while a merge runs, says
why.

When this Agentty lets devices create sessions, the sidebar offers `New session`. Home
then asks what the agent should work on in the active project and base branch, with a
choice of session type when Agentty offers more than one. Its message box is the open
session's: the line under it shows the new session's model, reasoning level, answer
style, and speed, and when the device may change session settings, the line and the
slash commands change them, with the agent that session type starts on, or another one
when this Agentty lets devices change it. Because the session has no worktree yet, `@`
lists the files in the active project's checkout, as it does for a draft in the
terminal. Sending creates the session, applies the settings you changed, sends your text
as its first message, and opens it. If the settings or the message do not go through,
the new session opens with your text still in its box.

The header of the open session shows its size and changed lines, a merge conflict with
its base branch, the session branch and its base branch, the remote branch it was
published to, the model and reasoning level, a link to its pull request, and, as in the
terminal, `Timer:` with the session's active time, counting while a turn runs, and when
it started, such as `Started 3h 5m ago`. As in the terminal's footer, `↓1 ↑3` after the
base branch means the session branch is one commit behind it and three ahead, and the
same counts after the remote branch compare with that branch; `✓` means even. The header
shows what fits on its line, most important first; the `⋯` button lists the session's
branch, remote branch, model, timer, age, and pull request in full. The line under the
message box shows the model, reasoning level, answer style, and speed. An older Agentty
sends fewer details, and the page leaves out what it does not know.

The open session shows its transcript, with each activity summary in a box you can open
or close. While the session works or waits, the transcript ends with the same status
line as the terminal, such as `Working...` with the agent's latest step, or
`Rebasing...`. When you scroll up, new messages leave your place alone and
`Jump to latest` returns to the end. With a keyboard and mouse, `Enter` sends and
`Shift+Enter` adds a line; on a touch screen, `Enter` adds a line and the button sends.
`Ctrl+Enter` or `Cmd+Enter` always sends. As in the terminal, `Up` on the first line
brings back your earlier messages to the session, newest first, and `Down` on the last
line goes forward and then back to the text you were typing. While a turn runs, a
message waits in the queue above the box for the next turn. The queue also lists, marked
`Action`, what waits for the turn besides messages, such as a sync, a publish, or a
model change. A draft session starts with a `Draft session` preview of the messages
staged for it; your next message starts the session with them. While the agent asks
questions, the box gives way to the questions: pick an option or type an answer, then
press `Send answers` once each has one. When the session's graph proposal or one of its
graph nodes asks instead, as the terminal's question panel shows, the page shows those
questions the same way and sends the answers to the graph work that asked; where the
session takes chat, `Reply in chat` closes them until you open the session again or
other graph work asks, and a chat message answers them too. A session that is merging or
finished shows why you cannot chat. Unsent text stays in the box for each session until
Agentty accepts it, even after a reload of the tab.

Once a session is in review, its latest
[focused review](@/docs/usage/workflow.md#focused-review) follows the transcript under a
`Focused review` heading, as in the terminal: the findings render like an answer, with
`View source`, and a review that failed says why. While a review runs, the status line
says so. Applying the review's suggestions and regenerating the review stay in the
terminal.

The `⋯` button in the session header offers `Cancel session`. When this Agentty lets
devices stop a turn or merge, the page also shows `Stop` next to the send button while a
turn runs, and `Merge` in the header, or in the `⋯` sheet on a phone. `Stop` returns the
session to review and keeps its changes; `Merge` asks before it merges the session into
its base branch, and names that branch and the changed lines. `Merge` is off for a
session with a pull request, which merges on the forge.

When this Agentty lets devices publish, `Publish` sits beside `Merge`, or in the `⋯`
sheet on a phone, while the session is in review or works. It asks first and names the
remote branch: the one the session was published to before, or the session branch the
first time. Publishing pushes the branch and opens its pull request, or refreshes the
one it has. During a turn, the publish waits and runs after it, as `p` does in the
terminal, and the button says `Publish queued` until the turn ends, even if the
connection drops meanwhile. Then it says `Publishing…` until Agentty answers, also when
you open another session meanwhile, and then the page links the pull request under the
header or shows why publishing failed. When this Agentty lets devices sync, the `⋯`
sheet offers `Sync with` the base branch, which asks first and rebases the session
branch, after the running turn if there is one.

When this Agentty lets devices change settings, the line under the message box is a
button. It opens a sheet with the agent, such as `claude` or `harness`, the chosen
agent's models, the reasoning level, the answer style, and the speed when the chosen
model can run fast. An older Agentty offers only the models of the session's own agent.
`Apply` sends only what you changed; during a running turn, the changes apply from the
next turn. Typing `/` at the start of the message box also lists `/model`, `/reasoning`,
`/style`, and, when the model can run fast, `/fast`, as in the terminal: type any of a
command's letters in order to narrow the list, pick one with the arrow keys and `Enter`,
or tap it, then pick a value. `/model` asks for the agent first, then the model; `/fast`
toggles the speed at once.

When this Agentty lets devices ask for graph proposals, the `/` list in an open session
also offers `/graph`, except while the session runs a turn or a sync, as in the
terminal. It turns the box into a `Graph` request: describe the task and send it, and
Agentty starts generating a
[graph proposal](@/docs/usage/workflow.md#generated-graph-proposals), which the graph
summary then shows. `Esc` or the `×` beside `Graph` returns to chat and keeps your text;
a refused request says why and keeps both. An unsent task stays a `Graph` request when
you switch sessions or the tab reloads. While the session or this Agentty cannot take a
graph request, the marker says so and the box sends nothing until you return to chat.
Nothing runs until you approve the proposal by replying in chat. Home's first message
for a new session cannot start with `/graph`; open the session first.

When this Agentty lets devices list a session's files, typing `@` at the start of a word
lists the files and directories in the session's worktree, best match first, as in the
terminal. Picking one puts `@path` in the message, or in a typed answer to a question,
for the agent to read.

When this Agentty lets devices read diffs, `Diff` in the header, or in the `⋯` sheet on
a phone, shows the session's changed files in place of the chat, each with its status
letter and changed lines. Open a file to read its patch; press `Diff` again to return to
the chat. A very large file list or patch shows only its beginning.

When this Agentty lets devices read graphs, a session with a
[generated graph proposal](@/docs/usage/workflow.md#generated-graph-proposals) shows a
summary above the message box, as the terminal's sidebar does: `Execution graph · v2`,
how many nodes are done, the proposal or graph status, and each output that awaits your
approval. Tap it, or `Graphs` in the header or the `⋯` sheet, to open the graphs in
place of the chat, like the terminal's
[inspector](@/docs/usage/workflow.md#execution-graph-inspector). Step through the
proposal and each graph with `‹` and `›`; the proposal shows its preview, and a graph
lists its nodes with their status, model, objective, and dependencies. Tap a node for
its details and outputs, which render like an answer; a very large preview or output
shows only its beginning and says so. The panel follows new progress, moving from the
proposal to its graph once you approve it, and `Refresh` reads it again; the back
gesture or `Graphs` returns to the chat.

When this Agentty also lets devices act on graphs, the panel offers the inspector's
actions where they apply, as its keys do in the terminal: `Accept` on the selected
output, `Retry` and `Reply` on the selected node, and `Resume`, `Add provider calls`,
and `Cancel graph` on the graph. `Reply` opens a box for guidance that revises the
node's output, and `Add provider calls` asks for a whole number of calls to add to the
spent budget it names: the selected node's, or else the first one. Cancelling a graph is
permanent, so the page asks first. Each action applies to the graph as the panel last
showed it; if the graph changed meanwhile, Agentty refuses the action, the panel shows
why and reloads, and a typed reply or number stays while the panel still shows the node
or budget it was typed for. A proposal stays a chat reply: approve, revise, or cancel it
by replying in chat. Questions a proposal or a node asks open the question panel, as
described above.

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
with `--remote-relay <URL>`, such as `--remote-relay wss://relay.example`, and run your
own `ag-relay` behind a reverse proxy that serves it over TLS, or, as a preview, sign in
to the [Agentty Platform](@/docs/usage/platform.md) and use its hosted relay.
`--remote-relay` alone enables remote control and the `Remote` tab, and accepts
`--remote-control-port` and `--remote-control-host`; a launch with it does not contact
the platform. The relay's container image is published for `amd64` and `arm64`: each
release tags it with the version and `latest`, and each commit to `main` tags it `main`.
The relay listens on port `8787`:

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
shows the relay as `Not admitted` and keeps retrying, with waits that grow to 30
seconds, so it connects once the relay admits it.

The identifier is the SHA-256 digest of the instance key that the pairing link carries
after `key=`. This prints it:

```sh
printf '%s' <key> | xxd -r -p | shasum -a 256
```

### Pair Through the Relay

With a relay, the tab also shows the relay state, a pairing link, and a QR code of the
link when the terminal is large enough. Scan the code or open the link on the other
device. The relay's pairing page opens with the code filled in: name the device, press
`Pair`, then approve the pairing as usual. On this computer, press `o` to open the link;
press `c` to copy it for another device. Copying uses the terminal clipboard; if the
terminal does not support it, select the link instead. The link carries the pairing
code, so it works once.

The browser remembers the pairing, so later visits to the relay's address connect
without a link. Pairing through a relay needs an up-to-date browser, from 2025 or later,
and a relay served over HTTPS.

One browser can pair with several Agenttys through the same relay: open the pairing link
of another Agentty, and it joins the ones already paired; `Back to sessions` on the
pairing page returns to them without pairing. The sidebar then lists each Agentty's
sessions under its own heading, which names it by the start of its instance identifier,
or by the name the page's directory gives it, and shows its connection state, above its
project line with `Sync` and `New session`. The filter, the `All`, `Needs you`, and
`Running` buttons, and the sessions waiting on home span them all, and each waiting
session names its Agentty. The address names the Agentty as well as the session. Each
Agentty connects on its own, so one that is offline or refuses this device leaves the
others working; the banner names the Agentty it is about, and the footer counts how many
are connected. The footer's `⋯` button offers `Forget on` each Agentty. With one Agentty
paired, the page looks as it does without the others.

Traffic through the relay is encrypted between the device and Agentty. The relay sees
when devices connect and how much they send, but not what they send. The relay also
serves the pairing page, so use a relay you trust or run your own.

A device paired through a relay proves itself with its own key instead of a stored
token. The browser keeps that key where scripts on the page cannot read it. Safari,
including every browser on iPhone and iPad, cannot store such a key yet, so there the
key is kept in a form that a script running in the page could read; the page switches to
the stronger form by itself once Safari supports it. Agentty keeps its relay key in its
database; removing it unpairs every relay device.

### Serve the Page From Another Web Application

<a id="usage-remote-control-embedded"></a> Another web application can serve the same
page on its own address, as one section of its site, and reach Agenttys through a relay
at another address; the `ag-relay` crate's `WebClientPage` renders the page for it and
names the content security policy it needs. The relay accepts device connections from
any page, since what they carry is encrypted end to end. Such a page works as the
relay's own:

- Links to the application's other sections sit at the top of the sidebar, its own badge
  stands beside the brand, and a link to your account sits under the theme switch.
- The page pairs in its main pane, beside that sidebar, so the links, the theme switch,
  and your account stay while it pairs instead of giving way to a full-screen card.
  Until an Agentty is paired, the sidebar leaves out the session list and the device;
  they appear once one is.
- When the application is the [platform](@/docs/usage/platform.md) Agentty signs in to,
  the pairing link that Agentty's `Platform` tab shows, or its QR code, opens the page
  directly. The link that the `Remote` tab shows opens the relay's own page, so copy it
  and paste it into the page's pairing form instead. Once one Agentty is paired,
  `Pair another Agentty` in the device menu opens the same form in the main pane, with
  your sessions still in the sidebar; choose a session or `Back to sessions` to leave
  it. A link to the page itself may leave out the relay, and the page refuses a link for
  another relay.
- The page may list the Agenttys of your account that this browser has not paired yet,
  with when each last connected and how to pair it, and name the paired ones as the
  application does. When the list says you are signed out, sign in to the application
  again; the paired Agenttys stay connected meanwhile.

Being listed grants nothing: each Agentty still pairs only through its own link and your
approval in its terminal. Whoever serves the page controls the code it runs, as for a
relay's page, so use only an application you trust with your sessions.

## Security

<a id="usage-remote-control-security"></a> A paired device can send prompts that agents
run with the session's permissions, so treat a pairing like a login.

### What a paired device can do

<a id="usage-remote-control-device-access"></a> Within the sessions it lists, a paired
device can:

- list sessions and read their transcripts, focused review results, questions, queued
  messages and actions, staged draft messages, agent and model settings, branches, and
  linked pull request;
- see each listed session's diff size, changed lines, model, active time, merge
  conflicts, pull request state, whether its branch is published, and how far its branch
  is ahead of and behind its base branch and its remote branch;
- read a session's diff against its base branch, which shows the repository content the
  session changed;
- read a session's graph proposals and execution graphs: the proposal preview with the
  captured request and each proposed node's captured instructions, and each node's
  status, objective, errors, questions, and outputs;
- list the names of the files and directories in a session's worktree, as `@` lookup in
  the terminal does, without reading their contents;
- list the names of the files and directories in the active project's checkout, for `@`
  lookup in the first message of a new session, without reading their contents;
- see the active project's name and branch, how far the branch is ahead of and behind
  its remote, and the progress and result of its sync, including Git's message when it
  fails;
- send messages, which start, resume, or queue a turn that agents run with edit
  permissions;
- answer an agent's questions, and the questions of a session's graph proposal or graph
  nodes, which go to the graph work that asked them as a terminal answer would;
- ask for a graph proposal with `/graph`, which starts model work that reads the
  session's worktree and proposes an execution graph; nothing launches until a chat
  reply approves the proposal;
- stop a running turn, which returns the session to review with its worktree and changes
  kept but discards the messages queued behind the turn;
- sync a session branch, which rebases it onto its base branch and starts an agent turn
  to resolve any conflicts;
- change a session's agent, model, reasoning level, response style, and speed wherever
  the terminal's composer could, choosing among the agents and models `/model` offers,
  including the most expensive model and fast mode; reasoning and style apply at once,
  while a model or speed change waits for the running turn as it does in the terminal;
- cancel a session, which deletes its worktree and branch;
- merge a session, which lands its changes on the base branch;
- create a session in the active project, which adds a worktree and branch;
- publish a session, which pushes its branch and opens or refreshes its pull request
  with this computer's Git and forge credentials;
- sync the active project, which pulls its branch with a rebase and pushes it with this
  computer's Git credentials, and starts an agent to resolve any conflicts; and
- act on a session's execution graph wherever the terminal's graph inspector could:
  accept an output, which releases the nodes waiting for it; retry a node, or reply to
  it with guidance, which runs it again with the session's permissions; resume a paused
  graph; add provider calls to its spent budget, which lets it spend more; and cancel a
  graph, which is permanent and which the page asks to confirm first.

Anyone holding a paired device can do the same, so remove a device you no longer
control.

### Protections

- A pairing code works once, expires after 10 minutes, and stops working after five
  wrong attempts. Press `n` in the `Remote` tab for a new code.
- A device becomes paired only after you approve it in the terminal; an unanswered
  request expires after two minutes.
- Each device gets its own key, which Agentty stores only as a hash. Remove a device in
  the `Remote` tab with `d`; its open connection closes at once. Keys do not expire, so
  remove a device you no longer control.
- A device can read and act on only the sessions it lists, which are the sessions of the
  active project, and creates sessions in and syncs only that project. Agentty refuses a
  request for any other session.
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
