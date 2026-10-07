+++
title = "Installation"
description = "Install Agentty, launch your first session, and review generated changes."
weight = 1
+++

<a id="installation-introduction"></a> Install `agentty`, authenticate an agent backend,
and launch your first session.

<!-- more -->

## Install

<a id="installation-options"></a>

{{ install_methods() }}

For Cargo installation on macOS, install Xcode Command Line Tools for native process
bindings.

<details id="verify-a-github-release">
<summary>Verify a GitHub Release</summary>

Install the [GitHub CLI](https://cli.github.com/), then download a GitHub release
artifact. Each artifact has keyless Sigstore build provenance that identifies Agentty's
release workflow:

```bash
gh attestation verify PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

Release immutability also protects the published tag and complete asset set. Verify a
specific release and a downloaded asset with:

```bash
gh release verify vX.Y.Z --repo agentty-xyz/agentty
gh release verify-asset vX.Y.Z PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

</details>

## Prepare an Agent Backend

Agentty also requires at least one supported agent CLI on your `PATH`. Install and
authenticate one backend before launching Agentty:

- **Codex** (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- **Claude** (`claude`): install
  [Claude Code](https://github.com/anthropics/claude-code), then run
  `claude auth login`.
- **Antigravity** (`agy` 1.1.18 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow.
- **Gemini** (`gemini`): install the
  [Gemini CLI](https://github.com/google-gemini/gemini-cli), then configure an API key
  or Vertex AI authentication.

See [Agents & Models](@/docs/agents/backends.md) before choosing credentials. Provider
subscription and OAuth rules differ, and not every interactive CLI sign-in is suitable
for third-party invocation through Agentty.

## Start a Session

Agentty automatically discovers Git repositories under your home directory.

1. Run `agentty` from any directory.
1. In the **Projects** tab, select a repository and press `Enter`.
1. In the **Sessions** tab, press `a` and choose `Regular`.
1. Type your first prompt and press `Enter` to start the agent.
1. Let the agent modify files in its dedicated worktree branch.

Only one Agentty instance can use a given Agentty root at a time. If startup reports
that another instance is running, close that instance first. A crash releases ownership
automatically; do not delete the lock file. Separate `AGENTTY_ROOT` directories can run
independently.

## Review Changes

<a id="installation-review-changes"></a> Inside `agentty`, open the diff view (`d`) to
inspect the generated `git diff` before you keep or discard edits.

## Next Steps

- [Overview](@/docs/getting-started/overview.md) — understand sessions, projects, and
  worktree isolation.
- [Workflow](@/docs/usage/workflow.md) — learn how to create, review, and finish
  sessions.
