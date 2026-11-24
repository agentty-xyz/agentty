//! Private, bounded launcher protocol. Never persist or log environment values.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::contract::ReadScope;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    pub(super) arguments: Vec<String>,
    pub(super) directory: PathBuf,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) executable: PathBuf,
    /// Readable roots outside the workspace: each granted path as given and
    /// resolved, and the repository's Git directories outside it.
    pub(super) external_reads: Vec<PathBuf>,
    pub(super) git_metadata: Vec<PathBuf>,
    pub(super) git_writes: Option<GitWrites>,
    /// Resolved paths hidden from commands, the built-in ones included.
    /// Coding grants default to their least access when absent.
    #[serde(default)]
    pub(super) hidden_paths: Vec<PathBuf>,
    pub(super) host_information: bool,
    pub(super) launcher: PathBuf,
    pub(super) linux_bubblewrap: Option<PathBuf>,
    pub(super) network: bool,
    /// Whether everything outside [`Self::hidden_paths`] is readable.
    #[serde(default)]
    pub(super) read_scope: ReadScope,
    /// Whether the system directories were granted, so Linux adds a private
    /// `/proc` and a minimal `/dev`.
    pub(super) system: bool,
    /// Resolved Unix-domain sockets commands may connect to; only macOS
    /// allows them.
    #[serde(default)]
    pub(super) unix_sockets: Vec<PathBuf>,
    pub(super) workspace: PathBuf,
    /// Validated `(device, inode)` identity of each write grant, in grant
    /// order; Linux launch fails when a mounted grant no longer matches.
    pub(super) write_nodes: Vec<(u64, u64)>,
    /// Resolved absolute writable directories: the workspace write grants,
    /// the temporary directory, then the writable roots.
    pub(super) writes: Vec<PathBuf>,
}

/// The repository's Git directories that the Git-writes grant opens.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitWrites {
    /// The linked worktree's own administration directory, when it is not
    /// the common directory.
    pub(super) admin: Option<PathBuf>,
    /// The branch whose reference and reflog in the common directory are
    /// writable.
    pub(super) branch: String,
    /// The common Git directory holding objects, references, and
    /// configuration.
    pub(super) common: PathBuf,
}

#[derive(Deserialize, Serialize)]
pub(super) enum Notice {
    Configure,
    MainExit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Finished,
    Failed,
}
