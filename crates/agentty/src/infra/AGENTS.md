# Infrastructure Layer

## Overview

Implementations of external interfaces (Database, Git, System).

## Directory Index

- [agent.rs](agent.rs) - Concrete agent backend implementations (Gemini, Claude, Codex).
- [db.rs](db.rs) - SQLite database implementation.
- [file_index.rs](file_index.rs) - Gitignore-aware file listing and fuzzy filtering for `@` mentions.
- [git.rs](git.rs) - Git operations.
- [lock.rs](lock.rs) - File locking mechanisms.
- [mod.rs](mod.rs) - Module definition.
- [version.rs](version.rs) - Version checking infrastructure.
