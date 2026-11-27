# Security Policy

## Supported Versions

Security fixes are developed on the `main` branch and released for the latest stable
version. Older releases do not receive security updates; users should upgrade to the
latest release before reporting a vulnerability that may already have been fixed.

| Version                 | Supported |
| ----------------------- | --------- |
| `main` branch           | Yes       |
| Latest stable release   | Yes       |
| Older stable releases   | No        |
| Development feature PRs | No        |

## Reporting a Vulnerability

Report suspected vulnerabilities through
[GitHub's private vulnerability reporting](https://github.com/agentty-xyz/agentty/security/advisories/new).
Do not open a public issue, discussion, or pull request for an undisclosed security
vulnerability. Report a vulnerability in the remote-control web client that an Agentty
Platform serves through this same process, although its code is not in this repository.

A security vulnerability is behavior that compromises the confidentiality, integrity, or
availability of Agentty or the projects it manages. Examples include:

- executing commands or accessing paths outside the intended project or worktree;
- exposing credentials, tokens, prompts, file contents, or agent-provider data;
- allowing untrusted repository or terminal content to cause command injection or code
  execution; and
- compromising Agentty's installation, update, build, or release process.

Reports about a third-party agent or service should normally be sent to that provider.
Report them here when Agentty's integration exposes data, expands privileges, or
otherwise contributes to the vulnerability. Publicly known dependency vulnerabilities
without an Agentty-specific impact and ordinary correctness bugs should use the public
issue tracker instead.

Include as much of the following information as possible:

- the affected Agentty version, operating system, installation method, and agent
  backend;
- a description of the vulnerability and its potential impact;
- reproducible steps or a minimal proof of concept;
- relevant logs, configuration, or screenshots with secrets removed; and
- any known mitigations or suggested fixes.

## Remote Control

Remote control is a preview feature that lets a paired browser follow and steer sessions
through the relay of an Agentty Platform that Agentty is signed in to, using the web
client that platform serves. Agentty accepts no inbound connections for it. A paired
device can send prompts that agents run with the session's permissions, so pairing, the
limited set of things a paired device may do, and the encryption between a device and
Agentty are security boundaries. See
[Remote Control](https://agentty.xyz/docs/usage/remote-control/) for how to use it and
[Remote Control Design](https://agentty.xyz/docs/architecture/remote-control-design/)
for its trust model.

A paired device is an authenticated control surface, not a viewer. It reaches every
project the terminal lists, not only the one the terminal has open, and within the
sessions of those projects it can do what the
[usage page](https://agentty.xyz/docs/usage/remote-control/#usage-remote-control-device-access)
lists under "What a paired device can do": today that is listing the projects, reading
transcripts, focused review results, diffs, and graph proposals and execution graphs
with their captured instructions and outputs, listing the file and directory names in a
session's worktree and in a listed project's checkout, seeing each listed project's
branch and sync state, sending prompts with PNG or JPEG images, which Agentty stores in
its own temporary folder for the turn, answering questions, including those of a
session's graph proposal or graph nodes, stopping a turn, syncing a session branch,
changing session settings, canceling a session, merging a session into its base branch,
creating sessions in any listed project, publishing a session branch and opening its
pull request with the user's forge credentials, syncing any listed project's branch with
its Git remote, asking for a graph proposal with `/graph`, which starts model work that
reads the session's worktree and proposes a graph but launches nothing until a chat
reply approves the proposal, and acting on an execution graph as the terminal's graph
inspector does: accepting an output, retrying a node or replying to it with guidance,
resuming a paused graph, adding provider calls to its spent budget, and cancelling a
graph, which is permanent and which the device's page asks to confirm first. With those,
anyone holding a paired device can have code written, land it on the base branch, and
push it to the Git remote of any listed project, so remove a device you no longer
control.

What a paired device reaches:

- **Listed projects.** The project the terminal has open, and every saved project its
  `Projects` tab and project switcher list. Session worktrees are never listed. A
  project the terminal stops listing is out of reach from the next request Agentty
  serves.
- **Listed sessions.** A session that its listed project's session list shows: the
  project's active sessions and the archived sessions on the list's first page, or as
  many as the terminal has loaded for the project it has open. Agentty answers any other
  session, such as an older archived one or a deleted one, as if it did not exist. A
  session list is the one Agentty holds in memory, which it reloads when the project's
  stored sessions change, so a session changed from outside this Agentty leaves the list
  when it next reloads.
- **Project requests.** Listing sessions, describing a project for a new session,
  listing a checkout's file names, creating a session, and syncing either name a listed
  project or name none, which means the project the terminal has open when Agentty
  serves the request. Agentty answers a project it does not list as not found.
- **Operations.** The operations above are the same in every listed project, and
  operations outside them are refused in every project. A request that reaches a project
  uses that project's own checkout and project settings.

Agentty enforces these limits on the host, not only in the remote-control server. In the
same step that serves a device's request, the host checks again that a project the
request names is listed, and that a session it names is in a listed project's session
list, loading that list when it is not loaded yet, and refuses any operation outside the
list above. The remote-control server checks the same limits before it asks the host.
Which project the terminal has open does not change what a device reaches, and a project
that has left the list, or a session that has left its project's list, by the time the
host serves the request is refused.

Report these as vulnerabilities:

- pairing a device without the one-time code or without approval in the terminal;
- a paired device doing more than the usage page lists, or reaching a project or session
  outside the terminal's project list: a project the terminal does not list, or a
  session that a listed project's session list does not show;
- a request from a paired device using one project's checkout or project settings for a
  session or project it does not belong to;
- a removed device still connecting;
- the relay, or anyone on the network path between a device, the relay, and Agentty,
  reading or changing what a device and Agentty exchange, or learning the pairing code,
  from that traffic;
- acting as a paired device without its key, or as an Agentty instance without its key;
- Agentty accepting an inbound connection for remote control; and
- one relay client taking service away from other instances or devices in a way the
  limits of `ag-relay` should prevent.

These are known limits, not vulnerabilities:

- The platform serves the web client that paired devices run, so whoever operates the
  platform, or anyone who can change what it serves, can change that code. On the
  device's side, end-to-end encryption ends inside the web client, so it does not
  protect against it. A changed client can read and send everything the device can,
  including the pairing code, and can pass the device's key to someone else, who can
  then act as that device until it is removed in the terminal. Agentty has no web client
  or relay of its own to use instead: using remote control means trusting the platform
  you sign in to with every paired device's access.
- The relay sees which instance a device asks for, the network addresses of both, when
  they connect, and how much they send.
- `ag-relay` does not limit plain TCP connections or slow request headers. A platform
  that embeds it is expected to put it behind a proxy that terminates TLS and limits
  connections.

## Agentty Platform

Signing in to an Agentty Platform, a preview feature, is the only way paired devices
reach Agentty: through the platform's relay, from the web client the platform serves.
See [Agentty Platform](https://agentty.xyz/docs/usage/platform/). The sign-in grants no
device any access: devices still pair with the one-time code, terminal approval, and
end-to-end encryption described above. The platform's relay is untrusted with the
streams it carries; the platform's web client is trusted, as the known limits under
"Remote Control" state. The access token is stored in Agentty's database with the
platform that issued it, sent only to that platform, and never to the relay or logged.

A platform may offer Sync for the signed-in account, its shared record of the work the
account's sessions do; see
[Sync](https://agentty.xyz/docs/usage/platform/#usage-platform-sync). Only while the
latest account answer enables it, Agentty sends that platform, for each new session, the
first non-blank line of its first message cut to 200 characters, that message cut to
4,000 characters, the repository's `origin` remote as `host/path` without scheme or
credentials, and a label of this Agentty's name and the start of the session's
identifier, before the session starts; a fork sends nothing, since it takes no task of
its own. Afterwards it reports activity when each turn starts and ends, sends the
session's title and review request link when the session is merged or done, and gives
the task up when the session is canceled, or deleted while Sync is on. It sends no file,
diff, or later message. The claim that proves a session holds its task is stored in
Agentty's database with the platform that issued it and sent only to that platform,
under the sign-in that received it: an answer that arrives after that sign-in ended
keeps no claim. Agentty has no switch of its own for Sync: signing out is the way to
stop it.

Of the platform's answers, only the milestone of the task a session holds reaches an
agent: its number, its title of at most 200 characters, and its goal of at most 4,000
characters. People or agents of the same account write milestones to state the goal a
group of tasks serves; every turn of the session carries its task's milestone so that
the agent keeps its work aligned with that goal, and a fork of the session keeps it. It
arrives in one delimited block that says it is reference data, never instructions.
Agentty keeps only the goal's line breaks, drops every other control or invisible
formatting character, such as bidirectional overrides and tag characters, and encodes
the text as JSON that also escapes `<`, `>`, `&`, `@`, `[`, and `]`, so it cannot close
or forge the block, name a file, or stand in for an image. A milestone outside these
bounds is dropped, and the task is kept without it. Other answer text is shown only in
the session.

Report these as vulnerabilities:

- a platform access token, device code, or sign-in code reaching the relay, another
  platform, a log, or a paired device;
- a Sync claim reaching anything but the platform that issued it, or a log;
- repository credentials from the `origin` remote, or anything beyond what the first
  Sync paragraph above lists, reaching a platform;
- Agentty sending a Sync request while the latest account answer does not enable Sync;
- text from a platform's Sync answer reaching an agent's prompt, other than the held
  task's milestone inside its block, or that milestone closing, leaving, or forging its
  block;
- a device gaining access to a session because Agentty is signed in, without pairing;
- Agentty contacting a platform that the user has neither signed in to nor started
  signing in to, dialling any relay but the one named by the platform it is registered
  with, or dialling that relay before it registered with the platform; and
- an answer from a platform making the terminal run control sequences, or making Agentty
  open anything but a web page.

These are known limits, not vulnerabilities:

- The platform vouches for which account an instance belongs to; it does not verify that
  the account holds the instance's key.
- The platform decides which instances its relay admits, and can refuse this Agentty at
  any time.
- A Sync start the platform completed after Agentty stopped waiting, or that Agentty
  quit before it heard the answer, leaves a task held for the session until the platform
  reads it as stale or another session takes it over. Signing out forgets held claims
  without telling the platform, and so does deleting a session while Sync is off or
  before the account answer of a launch, with the same effect.
- Agentty treats an answer whose task it cannot use, such as one naming a project it
  will not put in a request path, as a failed start, and the task stays held on the
  platform until the platform reads it as stale.
- Any session of the account can take a task over from another, and the platform, not
  Agentty, decides which work counts as the same.
- Anyone who can write the account's milestones writes text that the agents of its
  sessions read. The block tells an agent never to follow it, but a model can still be
  swayed by it, as by any file it reads.

## Response and Disclosure Process

Maintainers will:

1. acknowledge the report within 3 business days;
1. provide an initial assessment within 7 business days;
1. send a status update at least every 14 calendar days while remediation is in
   progress; and
1. coordinate public disclosure, normally within 90 calendar days of the initial report.

The disclosure timeline may be shortened when a fix is available, the vulnerability is
already public, or active exploitation is suspected. It may be extended by agreement
with the reporter when remediation or downstream coordination requires more time.

Maintainers will validate the report, determine its severity and affected versions,
develop and test a fix privately, and publish a patched release. When appropriate, the
project will also publish a GitHub security advisory, request a CVE, credit the reporter
if desired, and document mitigations. Please keep vulnerability details confidential
until coordinated disclosure or until the 90-day timeline has elapsed.

## Safe Harbor

The project will not pursue legal action against good-faith security research that
follows this policy. Researchers must avoid privacy violations, data destruction,
service disruption, social engineering, and testing against data or systems they do not
own or have permission to use. Access only the data needed to demonstrate the
vulnerability, do not retain or disclose it, and allow a reasonable opportunity to
remediate the issue before disclosure.
