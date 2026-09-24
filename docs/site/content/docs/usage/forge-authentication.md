+++
title = "Forge Authentication"
description = "GitHub and GitLab CLI setup for branch publishing and review-request publishing."
weight = 3
+++

<a id="usage-forge-authentication"></a> Agentty uses plain Git for branch publishing and
then uses the forge CLI for pull-request or merge-request actions.

`p` pushes the branch before creating a review request. Authenticate both Git and the
forge CLI: HTTPS remotes need Git credentials; SSH remotes need an SSH key.

For the session-view publish flow, see [Workflow](@/docs/usage/workflow.md).

<!-- more -->

## GitHub

<a id="usage-forge-authentication-github"></a> Use this setup when the repository remote
is on GitHub.

1. Run `gh auth login`.
1. Choose the target host such as `github.com`.
1. Pick the Git protocol Agentty should use for branch publishing: `https` or `ssh`.
1. If you picked `https`, run `gh auth setup-git` after login so Git can use `gh` as a
   credential helper for branch pushes.
1. Verify the setup with `gh auth status`.

## GitLab

<a id="usage-forge-authentication-gitlab"></a> Use this setup when the repository remote
is on GitLab.

1. Run `glab auth login`.
1. Choose the target host such as `gitlab.com` or your self-hosted hostname.
1. Pick the Git protocol Agentty should use for branch publishing: `https` or `ssh`.
1. Verify the CLI login with `glab auth status --hostname <hostname>`.
1. If you picked `https`, also configure Git itself for HTTPS pushes: either a
   credential helper or a personal access token that Git can use when `git push` prompts
   for credentials.

If `glab auth status` says you are authenticated but Agentty still reports a
push-authentication failure, the missing piece is Git HTTPS credentials rather than
`glab` authentication.
