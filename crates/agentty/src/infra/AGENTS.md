# Infrastructure Layer

## Overview

Implementations of external interfaces (Database, System, provider transport).

## Entry Points

- `db.rs` owns SQLite persistence and query execution.
- `channel.rs` and `agent.rs` own provider transport and prompt execution.
- `app_server.rs` and `app_server/` own shared app-server runtime infrastructure.
- `file_index.rs` owns gitignore-aware file traversal used by `@` mentions and explorer
  features.
- `clipboard_image.rs` owns clipboard image capture, PNG encoding, and prompt-image
  temp-file persistence behind `ClipboardImageClient`.

## Change Guidance

- Keep new external integrations behind trait boundaries.
- Keep reusable git operations in `crates/ag-git/`; do not recreate a local `git`
  implementation in this directory.
- Route subprocess, filesystem, and time access through existing infrastructure
  boundaries instead of introducing direct calls in orchestration layers.
