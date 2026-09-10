use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mockall::Sequence;
use serde_json::Value;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::*;
use crate::agent::app_server::codex::{
    MockCodexRuntimeTransport, lifecycle, policy, stream_parser, usage,
};
use crate::model::agent::{AgentModel, ReasoningLevel};

#[path = "client_test/compaction_test.rs"]
mod compaction;
#[path = "client_test/request_test.rs"]
mod request;
#[path = "client_test/runtime_test.rs"]
mod runtime;
#[path = "client_test/support_test.rs"]
mod support;

use support::*;
