use std::path::PathBuf;
use std::sync::Arc;

use ag_protocol::TurnPromptAttachment;
use tokio::sync::mpsc;

use super::*;
use crate::app_server::{AppServerTurnResponse, MockAppServerClient};
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentModel, ReasoningLevel};

#[path = "app_server_test/repair_test.rs"]
mod repair;
#[path = "app_server_test/stream_test.rs"]
mod stream;
#[path = "app_server_test/support_test.rs"]
mod support;
#[path = "app_server_test/turn_test.rs"]
mod turn;

use support::*;
