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
vulnerability.

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

Remote control is a preview feature that lets a paired browser follow and steer
sessions, directly or through a relay. A paired device can send prompts that agents run
with the session's permissions, so pairing, the limited set of things a paired device
may do, and the encryption between a device and Agentty are security boundaries. See
[Remote Control](https://agentty.xyz/docs/usage/remote-control/) for how to use it and
[Remote Control Design](https://agentty.xyz/docs/architecture/remote-control-design/)
for its trust model.

Report these as vulnerabilities:

- pairing a device without the one-time code or without approval in the terminal;
- a paired device doing more than listing sessions, reading their transcripts, sending
  messages, answering questions, and stopping a turn;
- a removed device still connecting;
- a relay that serves the web client unchanged, or anyone on the network path to a relay
  served over TLS, reading or changing what a device and Agentty exchange, or learning
  the pairing code;
- acting as a paired device without its key or token, or as an Agentty instance without
  its key;
- a web page from another site connecting to Agentty's listener, or a script running in
  the web client that neither Agentty nor the relay served; and
- one relay client taking service away from other instances or devices in a way the
  relay's limits should prevent.

These are known limits, not vulnerabilities:

- Whoever runs a relay serves the web client and can change its code. A changed client
  can read and send everything the device can, including the pairing code. Use a relay
  you trust or run your own, and serve it over TLS.
- A relay sees which instance a device asks for, the network addresses of both, when
  they connect, and how much they send.
- The relay does not limit plain TCP connections or slow request headers. It expects a
  reverse proxy that terminates TLS and limits connections.
- Safari, including every browser on iPhone and iPad, cannot yet store a device key that
  page scripts cannot read. There the key is kept in a weaker form until the browser
  can.
- Agentty's own listener does not encrypt. It relies on the private tunnel that reaches
  it.

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
