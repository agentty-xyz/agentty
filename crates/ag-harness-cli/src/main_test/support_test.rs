use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use clap::Parser;
use serde_json::{Value, json};
use wiremock::ResponseTemplate;

use crate::{Cli, Command, ResumeArgs, RunArgs};

pub(super) struct FixedModel(pub(super) Value);

#[async_trait]
impl ag_harness::Model for FixedModel {
    async fn complete(
        &self,
        _request: ag_harness::ModelRequest,
    ) -> Result<ag_harness::ModelCompletion, ag_harness::ModelError> {
        Ok(ag_harness::ModelCompletion::from_response(
            ag_harness::ModelResponse::Output(self.0.clone()),
        ))
    }
}

pub(super) struct FailOnceModel {
    pub(super) requests: AtomicUsize,
}

#[async_trait]
impl ag_harness::Model for FailOnceModel {
    async fn complete(
        &self,
        _request: ag_harness::ModelRequest,
    ) -> Result<ag_harness::ModelCompletion, ag_harness::ModelError> {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ag_harness::ModelError::InvalidResponse);
        }

        Ok(ag_harness::ModelCompletion::from_response(
            ag_harness::ModelResponse::Output(json!({"message": "recovered"})),
        ))
    }
}

pub(super) fn provider_response(message: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"message": message}).to_string()}
        }]
    }))
}

pub(super) fn run_arguments(command: Command) -> Option<RunArgs> {
    match command {
        Command::Run(args) => Some(args),
        Command::Resume(_) => None,
    }
}

pub(super) fn resume_arguments(command: Command) -> Option<ResumeArgs> {
    match command {
        Command::Resume(args) => Some(args),
        Command::Run(_) => None,
    }
}

pub(super) fn with_repository_controlled_git(mut cli: Cli) -> Result<Cli, io::Error> {
    let git_executable = std::env::current_exe()?;
    let repository_root = git_executable
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("test executable should have a parent"))?;
    cli.git_executable = Some(git_executable);
    match &mut cli.command {
        Command::Run(arguments) => arguments.read_dir = repository_root,
        Command::Resume(arguments) => arguments.read_dir = repository_root,
    }

    Ok(cli)
}

pub(super) fn parse_cli<I, T>(arguments: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
    arguments.splice(
        1..1,
        [
            OsString::from("--git-executable"),
            test_git_executable().into_os_string(),
        ],
    );

    Cli::try_parse_from(arguments)
}

pub(super) fn test_git_executable() -> PathBuf {
    let executable_name = format!("git{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::var_os("PATH");
    assert!(path.is_some(), "test PATH should be configured");
    let executables = path
        .iter()
        .flat_map(|path| std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join(&executable_name))
        .filter_map(|candidate| candidate.canonicalize().ok())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert!(
        !executables.is_empty(),
        "trusted Git executable should be available on PATH"
    );

    executables[0].clone()
}
