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
