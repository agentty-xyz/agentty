+++
title = "Message"
description = "Durable session history, transcript visibility, and conversation replay."
weight = 1
+++

A Message is a saved entry in a session's history. Messages preserve conversation
context across restarts and support the transcript shown to the user.

## Content and Visibility

History includes conversation content, workflow feedback, and activity summaries. Some
context is retained for the agent without appearing in normal chat. Workflow feedback
and activity summaries remain separate from the conversation used for replay.

## Durability and Submission

The [Command Gateway](@/docs/core-components/command-gateway.md) admits requests;
messages record history. Input waiting in memory is not yet durable. Saved history
survives restarts, but does not automatically resume interrupted
[execution](@/docs/core-components/execution.md).
