use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_protocol::{TurnPromptAttachment, TurnPromptTextSource};
use mockall::Sequence;
use tempfile::tempdir;

use super::*;
use crate::agent::app_server::codex::MockCodexRuntimeTransport;
use crate::model::agent::{AgentModel, ReasoningLevel};

#[path = "lifecycle_test/payload_test.rs"]
mod payload;
#[path = "lifecycle_test/runtime_test.rs"]
mod runtime;
#[path = "lifecycle_test/support_test.rs"]
mod support;

use support::*;
