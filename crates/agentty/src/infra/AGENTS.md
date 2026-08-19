# Infrastructure Layer

Concrete implementations for filesystem, process, persistence composition, clocks,
clipboard storage, and other host boundaries.

- Put new external access behind a typed trait boundary that orchestration can inject.
- Keep workflow policy in `app`; infra implements capabilities rather than deciding
  application flow.
- Reuse agent transports from `ag-agent`, clipboard reads from `ag-clipboard`, Git
  operations from `ag-git`, and persistence from `ag-store` instead of recreating them
  here.
