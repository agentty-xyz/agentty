+++
title = "Remote Control"
description = "Follow and steer Agentty sessions from a phone or another computer's browser through an Agentty Platform."
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
change a session's settings, create sessions, merge a session into its base branch,
publish its branch, sync a project's branch with its remote, ask for a graph proposal
with `/graph`, and act on an execution graph as the terminal's graph inspector does; the
web page offers each of these. This Agentty also lets a device merge a session's ready
GitHub pull request, as `m` does in the terminal, once the page offers it. A device
reaches every project in `Projects` and picks the one it shows with the
[project menu](#usage-remote-control-project).

Remote control is a preview feature that runs through an
[Agentty Platform](@/docs/usage/platform.md): Agentty connects out to the platform's
relay, and devices connect to the same relay from the page the platform serves.

<!-- more -->

## Start Remote Control

<a id="usage-remote-control-start"></a>

1. Sign in to a platform in the `Platform` tab; see
   [Sign In](@/docs/usage/platform.md#usage-platform-sign-in). Once the platform
   registers this Agentty, the sign-in ends with remote control running. Later, press
   `Enter` in the tab to start or stop it.
1. The tab shows the relay state, a pairing link, and a QR code of the link. The code
   sits beside the details, or below the relay state in a narrow terminal; when it fits
   nowhere, the tab asks you to enlarge the terminal to show it.
1. Scan the code or open the link on the other device. The platform's pairing page opens
   with the code filled in: name the device and press `Pair`. On this computer, press
   `o` to open the link; press `c` to copy it for another device. Copying uses the
   terminal clipboard; if the terminal does not support it, select the link instead.
1. The device shows a four-character confirmation code, and the tab shows a pairing
   request with the same code. Press `y` to approve it only when the codes match, or `x`
   to deny it.

The link carries a one-time pairing code, so it works once; press `n` for a new one.
Opening the tab while remote control runs also replaces the code. Pairing requests
appear only in the tab, so keep it open while you pair a device. The device stays
paired, so later visits to the platform's page connect without a link.

The tab lists the paired devices. It names no project: a paired device reaches every
project in `Projects`, whichever one the terminal has open.

Remote control stops when you press `Enter` in the tab again or quit Agentty. It starts
again on the next launch only with
[keep on](@/docs/usage/platform.md#usage-platform-keep-on). See
[Keybindings](@/docs/usage/keybindings.md#platform) for the tab's keys.

## Use the Page

<a id="usage-remote-control-page"></a> The platform's page lists sessions in a sidebar
and shows the open session beside it. On a phone or a narrow window, the menu button
opens the list; the back gesture, `Esc`, or a tap beside the list closes it. The address
names the open session, so the back gesture returns to the previous one and a reload
keeps it open.

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

The top of the sidebar names the project the page shows, its branch, and how far that
branch is from its remote: `↓2 ↑1` means two commits to pull and one to push, and `✓`
means none. When this Agentty lets devices sync the project, `Sync` beside it pulls and
pushes that branch as `s` does in the terminal. The line under it follows the sync and
shows its result for a few seconds. A sync that cannot start, such as while a merge in
any project runs or waits, says why.

<a id="usage-remote-control-project"></a> When this Agentty lists more than one project,
a menu takes the place of the project name at the top of the sidebar. It lists the
projects in the order the terminal's `p` popup does and marks the one the terminal has
open `· terminal`. The page starts on the terminal's project, or on the project you last
picked in this browser for this Agentty, and keeps showing its project when the terminal
switches to another. The session list, the project line with its branch, counts, and
`Sync`, `New session`, and `@` file lookup in a new session's first message all follow
the pick. The browser remembers only projects you pick yourself, for each Agentty, and
forgets them when you forget the pairing. If the project the page shows is no longer
listed, the page goes back to the terminal's project and says
`The project shown here is no longer listed, so this shows the terminal's project.` When
the page shows [several Agenttys](#usage-remote-control-several), each one has its own
menu.

When this Agentty lets devices create sessions, the sidebar offers `New session`. Home
then asks what the agent should work on in the project the page shows and its base
branch, with a choice of session type when Agentty offers more than one. Its message box
is the open session's: the line under it shows the new session's model, reasoning level,
answer style, and speed, and when the device may change session settings, the line and
the slash commands change them, with the agent that session type starts on, or another
one when this Agentty lets devices change it. Because the session has no worktree yet,
`@` lists the files in that project's checkout, as it does for a draft in the terminal.
Sending creates the session, applies the settings you changed, sends your text as its
first message, and opens it. If the settings or the message do not go through, the new
session opens with your text still in its box.

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

The open session shows its transcript, with each turn's summary of edits, failures, and
explorations in a box you can open or close, in the same columns and colors as the
terminal. While the session works or waits, the transcript ends with a status line, such
as `Working...` with what the agent does now, like `Running cargo test`, or
`Rebasing...`. When you scroll up, new messages leave your place alone and
`Jump to latest` returns to the end. With a keyboard and mouse, `Enter` sends and
`Shift+Enter` adds a line; on a touch screen, `Enter` adds a line and the button sends.
`Ctrl+Enter` or `Cmd+Enter` always sends. As in the terminal, `Up` on the first line
brings back your earlier messages to the session, newest first, and `Down` on the last
line goes forward and then back to the text you were typing. While a turn runs, a
message waits in the queue above the box for the next turn. The queue also lists, marked
`Action`, what waits for the turn besides messages, such as a sync, a publish, or a
model change. When this Agentty takes images, pasting a PNG or JPEG image into the
message box attaches it: an `[Image #n]` placeholder marks its place in the text, and a
chip under the box shows its size and removes it. A message carries up to 8 images, 6 MB
together, and goes with your text to the agent as a terminal paste does. A `/graph`
request takes no images. A draft session starts with a `Draft session` preview of the
messages staged for it; your next message starts the session with them. While the agent
asks questions, the box gives way to the questions: pick an option or type an answer,
then press `Send answers` once each has one. When the session's graph proposal or one of
its graph nodes asks instead, as the terminal's question panel shows, the page shows
those questions the same way and sends the answers to the graph work that asked; where
the session takes chat, `Reply in chat` closes them until you open the session again or
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
summary then shows. As in the terminal, a request for a session whose graph can still
run [adds work to that graph](@/docs/usage/workflow.md#adding-work-to-a-running-graph).
`Esc` or the `×` beside `Graph` returns to chat and keeps your text; a refused request
says why and keeps both. An unsent task stays a `Graph` request when you switch sessions
or the tab reloads. While the session or this Agentty cannot take a graph request, the
marker says so and the box sends nothing until you return to chat. Nothing runs until
you approve the proposal by replying in chat. Home's first message for a new session
cannot start with `/graph`; open the session first.

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

## Pair Several Agenttys

<a id="usage-remote-control-several"></a> One browser can pair with several Agenttys
signed in to the same platform: open the pairing link of another Agentty, and it joins
the ones already paired. Once one Agentty is paired, `Pair another Agentty` in the
device menu opens the pairing form in the main pane, with your sessions still in the
sidebar; choose a session or `Back to sessions` to leave it without pairing. The page
refuses a link for another platform's relay.

The sidebar then lists each Agentty's sessions under its own heading, which names it as
the platform does, or by the start of its instance identifier, and shows its connection
state, above its project line with `Sync` and `New session`. The filter, the `All`,
`Needs you`, and `Running` buttons, and the sessions waiting on home span them all, and
each waiting session names its Agentty. The address names the Agentty as well as the
session. Each Agentty connects on its own, so one that is offline or refuses this device
leaves the others working; the banner names the Agentty it is about, and the footer
counts how many are connected. The footer's `⋯` button offers `Forget on` each Agentty.
With one Agentty paired, the page looks as it does without the others.

The page may also list the Agenttys of your account that this browser has not paired
yet, with when each last connected and how to pair it. When the list says you are signed
out, sign in to the platform again; the paired Agenttys stay connected meanwhile. Being
listed grants nothing: each Agentty still pairs only through its own link and your
approval in its terminal.

## Security

<a id="usage-remote-control-security"></a> A paired device can send prompts that agents
run with the session's permissions, so treat a pairing like a login.

### Who you trust

<a id="usage-remote-control-trust"></a> Traffic between a device and Agentty is
encrypted end to end. The relay sees which Agentty a device asks for, the network
addresses of both, when they connect, and how much they send, but not what they send.

The platform also serves the page your devices run, and on the device's side the
encryption ends inside that page. Whoever operates the platform, or anyone who can
change what it serves, can change the page's code. A changed page can read and send
everything the device can, including the pairing code in the link, and can pass the
device's key to someone else, who can then act as that device until you remove it.
Encryption does not protect against the page itself, and Agentty has no page or relay of
its own to use instead, so sign in only to a platform you trust with your sessions.

### What a paired device can do

<a id="usage-remote-control-device-access"></a> A paired device reaches every project in
`Projects` and the project the terminal has open, whichever that is. Within those
projects and the sessions their lists show, it can:

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
- list the projects and how many active sessions each has;
- list the names of the files and directories in a project's checkout, for `@` lookup in
  the first message of a new session, without reading their contents;
- see a project's name and branch, how far the branch is ahead of and behind its remote,
  and the progress and result of its sync, including Git's message when it fails;
- send messages, with PNG or JPEG images that Agentty stores in its own temporary folder
  for the turn, which start, resume, or queue a turn that agents run with edit
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
- create a session in any of these projects, not only the one the terminal has open,
  which adds a worktree and branch in that project's checkout;
- publish a session, which pushes its branch and opens or refreshes its pull request
  with this computer's Git and forge credentials;
- merge a session's ready GitHub pull request on GitHub, or add it to its merge queue,
  with this computer's forge credentials, and, for a pull request ready with bypass,
  merge it past its branch protections with your administrator rights;
- sync any of these projects, which pulls its branch with a rebase and pushes it with
  this computer's Git credentials, and starts an agent to resolve any conflicts; and
- act on a session's execution graph wherever the terminal's graph inspector could:
  accept an output, which releases the nodes waiting for it; retry a node, or reply to
  it with guidance, which runs it again with the session's permissions; resume a paused
  graph; add provider calls to its spent budget, which lets it spend more; and cancel a
  graph, which is permanent and which the page asks to confirm first.

Anyone holding a paired device can do the same, so remove a device you no longer
control.

### Protections

- A pairing code works once, expires after 10 minutes, and stops working after five
  wrong attempts. Press `n` in the `Platform` tab for a new code.
- A device becomes paired only after you approve it in the terminal; an unanswered
  request expires after two minutes.
- Each device proves itself with its own key, and Agentty stores only a digest of the
  public key. Remove a device in the `Platform` tab with `d`; its open connection closes
  at once. Keys do not expire, so remove a device you no longer control. Agentty keeps
  its own instance key in its database; removing that key unpairs every device.
- Signing in to a platform grants no device any access, and Agentty never sends the
  platform's access token to the relay or to a device.
- A device reaches only the projects in `Projects` and the project the terminal has
  open, and only the sessions their lists show: active sessions and the archived ones
  the list shows. Agentty answers a request for any other project or session as if it
  did not exist, checking again as it serves the request, so a project that leaves
  `Projects` is out of reach at once. Which project the terminal has open does not
  change what a device reaches.
- A device that publishes a session pushes it to the branch it was published to before,
  or under the default name the first time, and opens or refreshes its pull request with
  this computer's Git and forge credentials; it cannot choose a branch name. See
  [Forge Authentication](@/docs/usage/forge-authentication.md) for what publishing
  needs.
- A device merges a pull request only while Agentty shows it ready or ready with bypass,
  and Agentty checks GitHub again first. It bypasses branch protections only when the
  device asks for a bypass and the pull request is ready with bypass; a request without
  one never bypasses.
- Agentty accepts no inbound connections: it reaches devices only through the relay of
  the platform it is signed in to.

## Limitations

<a id="usage-remote-control-limitations"></a>

- Remote control works only while Agentty is open in a terminal.
- Remote control needs a platform whose relay admits this Agentty; while your account
  [waits for access](@/docs/usage/platform.md#usage-platform-waitlist), devices cannot
  reach it.
- Pairing needs an up-to-date browser, from 2025 or later.
- Safari, including every browser on iPhone and iPad, cannot yet store a device key that
  page scripts cannot read, so there the platform's page keeps the key in a weaker form,
  which a script running in the page could read.
- A project without a current Git branch, such as a detached `HEAD`, cannot take new
  sessions; a device's request for the host details or a new session then fails.
- When a device first shows a project the terminal does not have open, ahead and behind
  counts and merge conflicts appear once Agentty has checked that project, shortly
  after.
- For a project the terminal does not have open, the archive shows its first page of
  archived sessions only.
- Pull request states, and the automatic follow-ups on review comments and failed
  checks, refresh only for the project the terminal has open. Another project's pull
  requests refresh when the terminal opens it or when that project syncs.
- Two saved projects that share one repository, such as a main checkout and a linked
  worktree, are not kept apart: a background fetch for one can collide with a fetch or
  sync of the other, and is retried.
- The transcript updates when a turn finishes; live streaming output is not shown.
- Answers render as Markdown: headings, lists, quotes, code blocks, tables, and links.
  Mermaid and other diagrams show as code, and images show as links. The page's own
  renderer can differ from the terminal at the edges; `View source` under an answer
  shows its text as the agent wrote it. An answer over 256 KB shows as plain text.
