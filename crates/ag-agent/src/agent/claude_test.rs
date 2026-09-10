use std::ffi::OsStr;
use std::path::PathBuf;

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPromptAttachment};
use serde_json::Value;
use tempfile::tempdir;

use super::*;
use crate::agent::prompt as shared_prompt;
use crate::channel::AgentRequestKind;
use crate::model::agent::ReasoningLevel;

#[path = "claude_test/command_test.rs"]
mod command;
#[path = "claude_test/permission_test.rs"]
mod permission;
#[path = "claude_test/support_test.rs"]
mod support;

use support::*;
