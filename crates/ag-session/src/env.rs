//! Execution locations, independent of agent conversations and permissions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::SessionId;

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
}

/// Host-resolved local environment for execution in a shared session worktree.
///
/// Hosts validate the workspace before constructing this value. Resolution
/// neither provisions a checkout nor grants permissions. Paths describe the
/// current host, not durable identity or a security sandbox. Separate task
/// worktrees and remote execution are not supported yet.
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
