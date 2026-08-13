# Infrastructure Layer

## Overview

Implementations of external interfaces for persistence, filesystem, process, clock, and
system boundaries.

## Entry Points

- `db.rs` composes `ag-store` with Agentty's database location and injected clock
  policy. Repository contracts, SQLite queries, and migrations live in `ag-store`.
- `file_index.rs` owns gitignore-aware file traversal used by `@` mentions and explorer
  features.
- `clipboard_image.rs` owns clipboard image capture, PNG encoding, and prompt-image
  temp-file persistence behind `ClipboardImageClient`.

## Change Guidance

- Keep new external integrations behind trait boundaries.
- Keep agent provider transports, app-server runtime infrastructure, and channel
  contracts in `crates/ag-agent/`.
- Keep reusable git operations in `crates/ag-git/`; do not recreate a local `git`
  implementation in this directory.
- Route subprocess, filesystem, and time access through existing infrastructure
  boundaries instead of introducing direct calls in orchestration layers.
