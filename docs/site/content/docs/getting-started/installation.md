+++
title = "Installation"
description = "Install Agentty, launch your first session, and review generated changes."
weight = 1
+++

<a id="installation-introduction"></a> Install `agentty`, authenticate an agent backend,
and launch your first session.

<!-- more -->

## Install

<a id="installation-options"></a> Agentty requires [Git](https://git-scm.com/) on your
`PATH`.

{{ install_methods() }}

- If `pnpm` reports a global bin-directory error, run `pnpm setup`, reopen your
  terminal, and retry.
- If package install scripts are blocked, the first launch downloads the binary from
  GitHub Releases, so keep network access available.
- Cargo installs on macOS need Xcode Command Line Tools for native process bindings.

Global `npm`, Bun, and `pnpm` installs and shell installs update automatically; see
[Auto-Update](@/docs/usage/workflow.md#auto-update). Shell installs of `v0.19.5` or
earlier need one manual update first. To update manually, use your original method:

| Method | Update command                        |
| ------ | ------------------------------------- |
| npm    | `npm install --global agentty@latest` |
| Bun    | `bun add --global agentty@latest`     |
| pnpm   | `pnpm add --global agentty@latest`    |
| npx    | `npx agentty@latest`                  |
| Cargo  | `cargo install agentty --locked`      |
| Shell  | Rerun the shell installer above.      |

<details id="verify-a-github-release">
<summary>Verify a GitHub Release</summary>

Install the [GitHub CLI](https://cli.github.com/) and download a release artifact. Each
artifact has keyless Sigstore build provenance identifying Agentty's release workflow:

```bash
gh attestation verify PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

Release immutability also protects the published tag and asset set. Verify a release and
a downloaded asset with:

```bash
gh release verify vX.Y.Z --repo agentty-xyz/agentty
gh release verify-asset vX.Y.Z PATH_TO_ARTIFACT --repo agentty-xyz/agentty
```

</details>

## Prepare an Agent Backend

Install and authenticate at least one supported agent CLI on your `PATH` before
launching Agentty:

- **Codex** (`codex`, recommended; supports subscription usage): install the
  [Codex CLI](https://github.com/openai/codex), then run `codex login`.
- **Claude** (`claude`): install
  [Claude Code](https://github.com/anthropics/claude-code), then run
  `claude auth login`.
- **Antigravity** (`agy` 1.1.18 or newer): install the
  [Antigravity CLI](https://github.com/google-antigravity/antigravity-cli), then run
  `agy` and follow its sign-in flow.

Provider subscription and OAuth rules differ, and not every interactive sign-in suits
third-party invocation. See
[Authentication and Usage](@/docs/agents/backends.md#authentication-and-usage) before
choosing credentials.

## Start a Session

1. Run `agentty` from the Git repository you want to work in. From elsewhere, add a
   repository in the **Projects** tab with `a` and press `Enter` to select it.
1. In the **Sessions** tab, press `a` and choose `Regular`.
1. Type your first prompt and press `Enter`. The agent works in its own worktree branch.

Only one Agentty instance can use an Agentty root at a time. If startup reports another
running instance, close it first; a crash releases the lock automatically, so do not
delete the lock file. Separate `AGENTTY_ROOT` directories run independently.

## Review Changes

<a id="installation-review-changes"></a> In session view, press `d` to inspect the diff
before you merge, publish, or discard the changes.

## Next Steps

- [Overview](@/docs/getting-started/overview.md) — understand sessions, projects, and
  worktree isolation.
- [Workflow](@/docs/usage/workflow.md) — learn how to create, review, and finish
  sessions.
