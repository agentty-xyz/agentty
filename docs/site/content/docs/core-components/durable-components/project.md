+++
title = "Project"
description = "Repository identity and shared configuration for related sessions."
weight = 2
+++

A Project represents a repository known to Agentty. It groups related
[sessions](@/docs/core-components/durable-components/session.md) and provides shared
configuration for work in that repository.

## Ownership

Project identity, repository location, and project settings are persisted across
application restarts. Sessions retain their own conversation history, lifecycle, and
workspace relationships, allowing multiple sessions to work within the same project.
