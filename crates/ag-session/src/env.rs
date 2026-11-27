//! Execution locations, independent of agent conversations and permissions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{SessionId, session_branch};

/// Most task worktrees one session may keep at once, including ones still
/// being created or removed, so parallel writers cannot exhaust disk space.
pub const TASK_WORKTREE_LIMIT: usize = 2;

/// Stable identity and lifecycle owner of an execution environment.
///
/// The existing session worktree is shared by all tasks in that session.
/// Its identity uses the full persisted session ID, never the shortened branch
/// name or a filesystem path. Session setup and cleanup remain authoritative;
/// finishing a task does not release the environment.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum EnvId {
    /// A local worktree owned by one session and shared by its tasks.
    SessionWorktree(SessionId),
    /// A separate local worktree owned by one session for one task's writes.
    TaskWorktree(TaskWorktreeId),
}

impl EnvId {
    /// Returns the session that owns, provisions, and cleans up this Env.
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::SessionWorktree(session_id) => session_id,
            Self::TaskWorktree(worktree) => &worktree.session_id,
        }
    }
}

/// Identity of a task worktree inside its owning session.
///
/// The worktree branches from the session branch at a base commit the host
/// records when it reserves the Env. Its branch and directory names derive
/// from this identity, so cleanup needs no stored path.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct TaskWorktreeId {
    /// Session that owns the worktree and every cleanup decision.
    pub session_id: SessionId,
    /// Identifier unique within the owning session.
    pub worktree_id: String,
}

impl TaskWorktreeId {
    /// Returns the worktree's own branch, distinct from the session branch.
    ///
    /// A `-` suffix rather than a `/` path keeps the name from colliding
    /// with the session branch ref.
    pub fn branch(&self) -> String {
        format!(
            "{}-{}",
            session_branch(&self.session_id),
            Self::short(&self.worktree_id)
        )
    }

    /// Returns the worktree directory name, a sibling of the session's
    /// shortened worktree directory.
    pub fn directory_name(&self) -> String {
        format!(
            "{}-{}",
            Self::short(&self.session_id),
            Self::short(&self.worktree_id)
        )
    }

    /// Shortens an identifier the way session branches and folders do.
    fn short(id: &str) -> &str {
        id.char_indices()
            .nth(8)
            .map_or(id, |(boundary, _)| &id[..boundary])
    }
}

/// Host-resolved local environment for execution in a shared session worktree.
///
/// Hosts validate the workspace before constructing this value. Resolution
/// neither provisions a checkout nor grants permissions. Paths describe the
/// current host, not durable identity or a security sandbox. Task worktrees
/// are not resolved for execution yet, and remote execution is not supported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedEnv {
    /// Validated session branch in the shared repository.
    pub branch: String,
    /// Working directory passed to execution, preserving the host's path.
    pub folder: PathBuf,
    /// Stable environment identity and cleanup owner.
    pub id: EnvId,
    /// Canonical main checkout protected by isolation checks, if non-bare.
    pub main_checkout: Option<PathBuf>,
}
