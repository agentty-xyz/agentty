use std::path::Path;
use std::process::Command;
use std::time::Duration;

use tempfile::tempdir;

use super::*;
use crate::MockAgentBackend;
use crate::app_server::{AppServerError, AppServerTurnResponse, MockAppServerClient};

#[path = "submission_test/app_server_test.rs"]
mod app_server;
#[path = "submission_test/cli_test.rs"]
mod cli;
#[path = "submission_test/repair_test.rs"]
mod repair;
#[path = "submission_test/support_test.rs"]
mod support;

use support::*;
