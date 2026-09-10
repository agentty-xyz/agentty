use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ag_protocol::{TurnPromptAttachment, TurnPromptTextSource};
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::*;
use crate::MockAgentBackend;
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentKind, ReasoningLevel};

#[path = "cli_test/execution_test.rs"]
mod execution;
#[path = "cli_test/repair_test.rs"]
mod repair;
#[path = "cli_test/support_test.rs"]
mod support;

use support::*;
