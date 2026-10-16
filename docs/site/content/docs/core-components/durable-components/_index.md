+++
title = "Durable Components"
description = "Persisted project, conversation, and execution records."
weight = 4
sort_by = "weight"
+++

Durable components describe saved data that remains available across application
restarts. Workflows write these records through persistence repositories; workers retain
responsibility for execution ordering and recovery.

- [Message](@/docs/core-components/durable-components/message.md): typed, ordered
  session history used for transcript display and conversation replay.
- [Project](@/docs/core-components/durable-components/project.md): repository identity
  and shared configuration for related sessions.
- [Session](@/docs/core-components/durable-components/session.md): conversation,
  lifecycle, and workspace context for ongoing work.
- [Operation](@/docs/core-components/durable-components/operation.md): durable tracking
  for session workflow execution.
- [Run](@/docs/core-components/durable-components/run.md): isolated utility model
  execution and its outcome.
