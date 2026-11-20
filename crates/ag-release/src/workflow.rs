//! `workflow`: generates `.github/workflows/release.yml`, the GitHub Actions
//! workflow that runs the other commands, or checks that the committed file
//! is current.
//!
//! The workflow is rendered from `src/template/release_workflow.yml.j2` with
//! Askama's `workflow` syntax, whose `[= … =]` and `[% … %]` delimiters
//! leave the workflow's `${{ … }}` expressions alone. Every configured value
//! it embeds is validated by [`WorkflowConfig`] and emitted as a quoted YAML
//! scalar or shell word unless it is plainly safe.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use askama::Template;
use serde_json::Value;

use crate::action_pin::ActionPins;
use crate::cli::{Host, WorkflowOptions};
use crate::config::{Installer, TABLE};
use crate::target::{Libc, Target};
use crate::workflow_config::{CustomJob, JobStage, WorkflowConfig};
use crate::workspace::{TOOL_PACKAGE, Workspace};

/// Path of the generated workflow, relative to the workspace root.
pub(crate) const WORKFLOW_PATH: &str = ".github/workflows/release.yml";

/// Linux targets whose runner can run the `distributions` containers, in
/// order of preference.
const DISTRIBUTION_TARGETS: [Target; 4] = [
    Target::X8664UnknownLinuxGnu,
    Target::X8664UnknownLinuxMusl,
    Target::Aarch64UnknownLinuxGnu,
    Target::Aarch64UnknownLinuxMusl,
];

/// Writes the generated workflow, or with `options.check` fails unless the
/// committed workflow matches it.
///
/// # Errors
/// Returns an error when the configuration is invalid for a workflow, the
/// committed workflow cannot be read or differs with `options.check`, or a
/// file or the output cannot be written.
pub(crate) fn run(
    host: &Host<'_>,
    options: &WorkflowOptions,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    let workspace = Workspace::load(host.process, host.cargo.clone())?;
    let path = workspace.root.join(WORKFLOW_PATH);
    let existing = if host.is_file(&path)? {
        Some(
            String::from_utf8(host.read_file(&path)?)
                .map_err(|error| format!("`{}` is not UTF-8: {error}", path.display()))?,
        )
    } else {
        None
    };
    let generated = ReleaseWorkflow::render(&workspace, existing.as_deref())?;
    if !options.check {
        host.write_file(&path, generated.as_bytes())?;

        return Host::report_written(stdout, &[path]);
    }

    match existing {
        Some(existing) if existing == generated => {
            writeln!(stdout, "`{WORKFLOW_PATH}` is up to date")
                .and_then(|()| stdout.flush())
                .map_err(|error| format!("failed to write the command output: {error}"))
        }
        Some(_) => Err(format!(
            "`{WORKFLOW_PATH}` differs from the workflow that `[{TABLE}]` generates; run \
             `ag-release workflow` and commit the result"
        )),
        None => Err(format!(
            "`{WORKFLOW_PATH}` is missing; run `ag-release workflow` and commit the result"
        )),
    }
}

/// Values of the generated release workflow.
#[derive(Template)]
#[template(path = "release_workflow.yml.j2", escape = "none", syntax = "workflow")]
struct ReleaseWorkflow<'config> {
    actions: ActionPins,
    /// Custom jobs that `host` requires.
    check_jobs: Vec<&'config CustomJob>,
    /// YAML scalar of the `distributions` job's runner, or `None` without
    /// configured images.
    distribution_runner: Option<String>,
    distributions: Vec<QuotedDistribution>,
    /// Whether a musl target is configured, whose build needs musl tools.
    musl: bool,
    /// YAML scalar of the Node.js version `smoke` sets up.
    node_version: Option<String>,
    npm: bool,
    /// Custom jobs that run after the GitHub release.
    publish_jobs: Vec<&'config CustomJob>,
    shell: bool,
    /// Shell words of the paths whose change runs `smoke`.
    smoke_paths: Vec<String>,
    /// YAML flow sequence of the distinct runner labels.
    smoke_runners: String,
    /// Git source of `ag-release`, or `None` to build it from the workspace.
    tool: Option<QuotedTool>,
}

impl<'config> ReleaseWorkflow<'config> {
    /// Renders the workflow of `workspace`, keeping the action pins of the
    /// `existing` workflow.
    ///
    /// # Errors
    /// Returns an error naming the key when the configuration cannot be
    /// expressed as a workflow.
    fn render(workspace: &'config Workspace, existing: Option<&str>) -> Result<String, String> {
        let config = &workspace.config;
        let workflow_config = &config.workflow;
        let shell = config.installers.contains(&Installer::Shell);
        let mut runners = BTreeSet::new();
        for (target, runner) in &config.targets {
            WorkflowConfig::check_text(runner, &format!("{TABLE}.targets.{}", target.triple()))?;
            runners.insert(runner.as_str());
        }
        let jobs = |stage| {
            workflow_config
                .jobs
                .iter()
                .filter(|job| job.stage == stage)
                .collect::<Vec<_>>()
        };
        let workflow = Self {
            actions: ActionPins::resolve(existing),
            check_jobs: jobs(JobStage::Check),
            distribution_runner: Self::distribution_runner(workspace, shell)?,
            distributions: workflow_config
                .distributions
                .iter()
                .map(|distribution| QuotedDistribution {
                    error: Self::quoted(&distribution.error),
                    image: Self::quoted(&distribution.image),
                })
                .collect(),
            musl: config
                .targets
                .keys()
                .any(|target| target.libc() == Libc::Musl),
            node_version: workflow_config.node_version.as_deref().map(Self::quoted),
            npm: config.installers.contains(&Installer::Npm),
            publish_jobs: jobs(JobStage::Publish),
            shell,
            smoke_paths: Self::smoke_paths(workspace)?,
            smoke_runners: format!(
                "[{}]",
                runners
                    .into_iter()
                    .map(Self::scalar)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            tool: workflow_config.tool.as_ref().map(|tool| QuotedTool {
                git: Self::quoted(&tool.git),
                rev: Self::quoted(&tool.rev),
            }),
        };

        Ok(format!("{workflow}\n"))
    }

    /// Returns the runner of the `distributions` job: that of the first
    /// configured Linux target, which runs the images natively.
    fn distribution_runner(workspace: &Workspace, shell: bool) -> Result<Option<String>, String> {
        let config = &workspace.config;
        if config.workflow.distributions.is_empty() {
            return Ok(None);
        }
        let key = format!("{TABLE}.workflow.distributions");
        if !shell {
            return Err(format!(
                "`{key}` tests the shell installer, but `{TABLE}.installers` does not list `shell`"
            ));
        }
        let runner = DISTRIBUTION_TARGETS
            .iter()
            .find_map(|target| config.targets.get(target))
            .ok_or_else(|| {
                format!(
                    "`{key}` needs a Linux target in `{TABLE}.targets`: {}",
                    DISTRIBUTION_TARGETS.map(Target::triple).join(" or ")
                )
            })?;

        Ok(Some(Self::scalar(runner)))
    }

    /// Returns the shell words of the paths whose change runs `smoke`: the
    /// `ag-release` package when the workspace builds it, this workflow, and
    /// the configured `smoke-paths`.
    fn smoke_paths(workspace: &Workspace) -> Result<Vec<String>, String> {
        let workflow_config = &workspace.config.workflow;
        let mut paths = Vec::new();
        if workflow_config.tool.is_none() {
            let directory = workspace.tool_directory.as_deref().ok_or_else(|| {
                format!(
                    "`{TABLE}.workflow.tool` is not set, but `{TOOL_PACKAGE}` is not a package \
                     of this workspace; set it to `{{ git = \"https://<host>/<repository>\", \
                     rev = \"<commit>\" }}`"
                )
            })?;
            let relative = directory
                .strip_prefix(&workspace.root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| {
                    format!(
                        "the `{TOOL_PACKAGE}` package directory `{}` must be a UTF-8 path inside \
                         the workspace root",
                        directory.display()
                    )
                })?;
            let relative = if relative.is_empty() { "." } else { relative };
            WorkflowConfig::check_text(relative, relative)?;
            paths.push(relative);
        }
        paths.push(WORKFLOW_PATH);
        paths.extend(workflow_config.smoke_paths.iter().map(String::as_str));

        Ok(paths.into_iter().map(Self::shell_word).collect())
    }

    /// Returns `text` as a plain YAML scalar when it is unmistakably a
    /// string, such as `ubuntu-22.04`, and as a double-quoted one otherwise.
    fn scalar(text: &str) -> String {
        let plain = text.starts_with(|character: char| character.is_ascii_alphabetic())
            && text
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
            && !text
                .chars()
                .all(|character| character.is_ascii_alphabetic());
        if plain {
            return text.to_string();
        }

        Self::quoted(text)
    }

    /// Returns `text` as a double-quoted YAML scalar, which JSON string
    /// syntax is a subset of.
    fn quoted(text: &str) -> String {
        Value::from(text).to_string()
    }

    /// Returns `text` as one shell word, single-quoted unless it is plainly
    /// safe.
    fn shell_word(text: &str) -> String {
        if text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/+-".contains(character))
        {
            return text.to_string();
        }

        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// One `distributions` matrix entry as YAML scalars.
struct QuotedDistribution {
    error: String,
    image: String,
}

/// The Git source of `ag-release` as YAML scalars.
struct QuotedTool {
    git: String,
    rev: String,
}

#[cfg(test)]
#[path = "workflow_test.rs"]
mod tests;
