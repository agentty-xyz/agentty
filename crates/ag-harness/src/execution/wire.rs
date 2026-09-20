//! Private, bounded launcher protocol. Never persist or log environment values.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Launch {
    pub(super) arguments: Vec<String>,
    pub(super) directory: PathBuf,
    pub(super) environment: BTreeMap<String, String>,
    pub(super) executable: PathBuf,
    pub(super) external_reads: Vec<PathBuf>,
    pub(super) git_metadata: Vec<PathBuf>,
    pub(super) host_information: bool,
    pub(super) launcher: PathBuf,
    pub(super) linux_bubblewrap: Option<PathBuf>,
    pub(super) workspace: PathBuf,
    pub(super) workspace_writes: Vec<PathBuf>,
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
