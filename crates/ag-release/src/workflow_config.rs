//! `[workspace.metadata.release.workflow]`: settings of the release workflow
//! that `ag-release workflow` generates.
//!
//! Every value the generated workflow embeds is validated here, so that no
//! configured text can end a YAML scalar, start a GitHub Actions expression,
//! or pass an option where a command expects an operand.

use std::collections::BTreeMap;

use crate::config::ConfigTable;

/// Keys accepted in `[workspace.metadata.release.workflow]`.
const WORKFLOW_KEYS: [&str; 5] = [
    "distributions",
    "jobs",
    "node-version",
    "smoke-paths",
    "tool",
];

/// Keys accepted in `workspace.metadata.release.workflow.tool`.
const TOOL_KEYS: [&str; 2] = ["git", "rev"];

/// Keys accepted in `[workspace.metadata.release.workflow.jobs.<id>]`.
const JOB_KEYS: [&str; 6] = [
    "permissions",
    "plan",
    "prerelease",
    "secrets",
    "stage",
    "uses",
];

/// Jobs that the generated workflow defines itself.
pub(crate) const BUILT_IN_JOBS: [&str; 6] = [
    "plan",
    "smoke",
    "build",
    "assemble",
    "distributions",
    "host",
];

/// Access levels of most `GITHUB_TOKEN` permissions.
const READ_WRITE_NONE: &[&str] = &["read", "write", "none"];

/// `GITHUB_TOKEN` permissions and the access levels each accepts, as the
/// GitHub Actions workflow syntax documents them.
const PERMISSIONS: [(&str, &[&str]); 17] = [
    ("actions", READ_WRITE_NONE),
    ("artifact-metadata", READ_WRITE_NONE),
    ("attestations", READ_WRITE_NONE),
    ("checks", READ_WRITE_NONE),
    ("code-quality", READ_WRITE_NONE),
    ("contents", READ_WRITE_NONE),
    ("copilot-requests", &["write", "none"]),
    ("deployments", READ_WRITE_NONE),
    ("discussions", READ_WRITE_NONE),
    ("id-token", &["write", "none"]),
    ("issues", READ_WRITE_NONE),
    ("packages", READ_WRITE_NONE),
    ("pages", READ_WRITE_NONE),
    ("pull-requests", READ_WRITE_NONE),
    ("security-events", READ_WRITE_NONE),
    ("statuses", READ_WRITE_NONE),
    ("vulnerability-alerts", &["read", "none"]),
];

/// Length of a full Git commit hash in hexadecimal digits.
const COMMIT_LENGTH: usize = 40;

/// Validated `[workspace.metadata.release.workflow]` settings; the default
/// is the plain pipeline.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WorkflowConfig {
    /// Container images of the dry run's install test, in configured order.
    pub(crate) distributions: Vec<Distribution>,
    /// Reusable-workflow jobs, in configured order.
    pub(crate) jobs: Vec<CustomJob>,
    /// Node.js version that `smoke` sets up, or `None` for the runner's.
    pub(crate) node_version: Option<String>,
    /// Extra workspace-relative paths whose change runs `smoke`.
    pub(crate) smoke_paths: Vec<String>,
    /// Where jobs install `ag-release` from, or `None` when it is a package
    /// of the workspace.
    pub(crate) tool: Option<ToolSource>,
}

impl WorkflowConfig {
    /// Validates the `workflow` table of the release configuration.
    ///
    /// # Errors
    /// Returns an error naming the key when the table has unknown keys or
    /// holds an invalid value.
    pub(crate) fn parse(table: &ConfigTable<'_>) -> Result<Self, String> {
        table.reject_unknown(&WORKFLOW_KEYS)?;
        let node_version = table.optional_string("node-version")?;
        if let Some(node_version) = node_version {
            Self::check_text(node_version, &table.key("node-version"))?;
        }
        let smoke_paths = table.relative_paths("smoke-paths")?;
        for path in &smoke_paths {
            Self::check_text(path, &table.key("smoke-paths"))?;
        }

        Ok(Self {
            distributions: Self::parse_distributions(table)?,
            jobs: Self::parse_jobs(table)?,
            node_version: node_version.map(ToString::to_string),
            smoke_paths,
            tool: table
                .optional_table("tool")?
                .map(|tool| ToolSource::parse(&tool))
                .transpose()?,
        })
    }

    /// Checks that `text` can appear verbatim in the generated workflow: it
    /// has no control characters or line breaks, and no `${{`, which GitHub
    /// Actions would evaluate as an expression.
    ///
    /// # Errors
    /// Returns an error naming `key` when `text` is not safe to embed.
    pub(crate) fn check_text(text: &str, key: &str) -> Result<(), String> {
        if text.chars().any(|character| {
            character.is_control() || matches!(character, '\u{2028}' | '\u{2029}' | '\u{feff}')
        }) {
            return Err(format!(
                "`{key}` must not contain control characters or line breaks, found {text:?}"
            ));
        }
        if text.contains("${{") {
            return Err(format!(
                "`{key}` must not contain `${{{{`, which GitHub Actions evaluates as an \
                 expression, found `{text}`"
            ));
        }

        Ok(())
    }

    fn parse_distributions(table: &ConfigTable<'_>) -> Result<Vec<Distribution>, String> {
        let Some(images) = table.optional_table("distributions")? else {
            return Ok(Vec::new());
        };

        images
            .entries
            .iter()
            .map(|(image, error)| {
                let key = images.key(image);
                if image.is_empty()
                    || image.starts_with('-')
                    || image.chars().any(char::is_whitespace)
                {
                    return Err(format!(
                        "`{key}` must name a container image such as `ubuntu:22.04`"
                    ));
                }
                Self::check_text(image, &key)?;
                let error = error.as_str().ok_or_else(|| {
                    format!(
                        "`{key}` must be a string: empty when the install must succeed, else text \
                         that the installer's failure contains"
                    )
                })?;
                Self::check_text(error, &key)?;

                Ok(Distribution {
                    error: error.to_string(),
                    image: image.clone(),
                })
            })
            .collect()
    }

    fn parse_jobs(table: &ConfigTable<'_>) -> Result<Vec<CustomJob>, String> {
        let Some(jobs) = table.optional_table("jobs")? else {
            return Ok(Vec::new());
        };
        let mut parsed: Vec<CustomJob> = Vec::with_capacity(jobs.entries.len());
        for (id, value) in jobs.entries {
            let key = jobs.key(id);
            CustomJob::check_id(id, &key)?;
            if let Some(built_in) = BUILT_IN_JOBS
                .iter()
                .find(|built_in| built_in.eq_ignore_ascii_case(id))
            {
                return Err(format!(
                    "`{key}` collides with the built-in `{built_in}` job; built-in jobs: {}",
                    BUILT_IN_JOBS.join(", ")
                ));
            }
            if let Some(other) = parsed.iter().find(|job| job.id.eq_ignore_ascii_case(id)) {
                return Err(format!(
                    "`{key}` and `{}` differ only in case, but GitHub Actions expressions such as \
                     `needs.<id>` ignore case",
                    jobs.key(&other.id)
                ));
            }
            parsed.push(CustomJob::parse(id, &ConfigTable::new(value, key)?)?);
        }

        Ok(parsed)
    }
}

/// One container image of the dry run's install test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Distribution {
    /// Text the installer's failure must contain, or empty when the install
    /// must succeed.
    pub(crate) error: String,
    /// Container image reference, such as `ubuntu:22.04`.
    pub(crate) image: String,
}

/// A pinned Git revision that jobs install `ag-release` from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolSource {
    /// `https://` URL of the repository.
    pub(crate) git: String,
    /// Full 40-digit lowercase hexadecimal commit hash.
    pub(crate) rev: String,
}

impl ToolSource {
    fn parse(table: &ConfigTable<'_>) -> Result<Self, String> {
        table.reject_unknown(&TOOL_KEYS)?;
        let git = table.required_string("git")?;
        let git_key = table.key("git");
        if !git.strip_prefix("https://").is_some_and(|rest| {
            !rest.is_empty() && !rest.starts_with('/') && !rest.chars().any(char::is_whitespace)
        }) {
            return Err(format!(
                "`{git_key}` must be an `https://` repository URL, found `{git}`"
            ));
        }
        WorkflowConfig::check_text(git, &git_key)?;
        let rev = table.required_string("rev")?;
        if rev.len() != COMMIT_LENGTH
            || !rev
                .chars()
                .all(|character| character.is_ascii_digit() || matches!(character, 'a'..='f'))
        {
            return Err(format!(
                "`{}` must be a full 40-digit lowercase hexadecimal commit hash, found `{rev}`",
                table.key("rev")
            ));
        }

        Ok(Self {
            git: git.to_string(),
            rev: rev.to_string(),
        })
    }
}

/// The point of the pipeline where a custom job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobStage {
    /// Runs on tag pushes before the release; `host` requires it.
    Check,
    /// Runs on tag pushes after the GitHub release is published.
    Publish,
}

impl JobStage {
    fn parse(stage: &str, key: &str) -> Result<Self, String> {
        match stage {
            "check" => Ok(Self::Check),
            "publish" => Ok(Self::Publish),
            _ => Err(format!(
                "`{key}` must be `check` or `publish`, found `{stage}`"
            )),
        }
    }
}

/// A job that calls a reusable workflow at one stage of the pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CustomJob {
    /// GitHub Actions job id.
    pub(crate) id: String,
    /// `GITHUB_TOKEN` access level by permission name.
    pub(crate) permissions: BTreeMap<&'static str, &'static str>,
    /// Whether the job passes the plan JSON as the `plan` input.
    pub(crate) plan: bool,
    /// Whether a `publish` job also runs for prereleases.
    pub(crate) prerelease: bool,
    /// Names of the repository secrets passed to the reusable workflow
    /// under the same names, in configured order.
    pub(crate) secrets: Vec<String>,
    pub(crate) stage: JobStage,
    /// Reusable workflow reference, copied verbatim.
    pub(crate) uses: String,
}

impl CustomJob {
    fn check_id(id: &str, key: &str) -> Result<(), String> {
        let mut characters = id.chars();
        let valid = characters
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && characters.all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            });
        if !valid {
            return Err(format!(
                "`{key}` must be a job id that starts with a letter or `_` and contains only \
                 letters, digits, `-`, and `_`"
            ));
        }

        Ok(())
    }

    fn parse(id: &str, table: &ConfigTable<'_>) -> Result<Self, String> {
        table.reject_unknown(&JOB_KEYS)?;
        let stage = JobStage::parse(table.required_string("stage")?, &table.key("stage"))?;
        let uses = table.required_string("uses")?;
        Self::check_uses(uses, &table.key("uses"))?;
        if stage == JobStage::Check && table.entries.contains_key("prerelease") {
            return Err(format!(
                "`{}` applies only to `publish` jobs",
                table.key("prerelease")
            ));
        }

        Ok(Self {
            id: id.to_string(),
            permissions: Self::parse_permissions(table)?,
            plan: table.optional_bool("plan")?,
            prerelease: table.optional_bool("prerelease")?,
            secrets: Self::parse_secrets(table)?,
            stage,
            uses: uses.to_string(),
        })
    }

    /// Accepts secret names as GitHub allows them: letters, digits, and `_`,
    /// not starting with a digit or the reserved `GITHUB_` prefix.
    fn parse_secrets(table: &ConfigTable<'_>) -> Result<Vec<String>, String> {
        table
            .optional_strings("secrets")?
            .into_iter()
            .map(|name| {
                let valid = !name.starts_with(|character: char| character.is_ascii_digit())
                    && !name.to_ascii_uppercase().starts_with("GITHUB_")
                    && name
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '_');
                if !valid {
                    return Err(format!(
                        "`{}` entry `{name}` must be a secret name of letters, digits, and `_` \
                         that starts with neither a digit nor `GITHUB_`",
                        table.key("secrets")
                    ));
                }

                Ok(name.to_string())
            })
            .collect()
    }

    /// Accepts a local reference, `./<path>` or `$/<path>`, or a remote
    /// `<owner>/<repo>/<path>@<ref>`, spelled with characters that need no
    /// quoting in YAML.
    fn check_uses(uses: &str, key: &str) -> Result<(), String> {
        let plain = |text: &str| {
            text.chars()
                .all(|character| character.is_ascii_alphanumeric() || "._/-".contains(character))
        };
        let valid = match uses.strip_prefix("./").or_else(|| uses.strip_prefix("$/")) {
            Some(path) => !path.is_empty() && plain(path),
            None => uses.split_once('@').is_some_and(|(path, reference)| {
                path.starts_with(|character: char| character.is_ascii_alphanumeric())
                    && path.split('/').count() >= 3
                    && path.split('/').all(|segment| !segment.is_empty())
                    && plain(path)
                    && !reference.is_empty()
                    && plain(reference)
            }),
        };
        if !valid {
            return Err(format!(
                "`{key}` must be a reusable workflow such as `./.github/workflows/<file>.yml` or \
                 `<owner>/<repo>/.github/workflows/<file>.yml@<ref>`, found `{uses}`"
            ));
        }

        Ok(())
    }

    fn parse_permissions(
        table: &ConfigTable<'_>,
    ) -> Result<BTreeMap<&'static str, &'static str>, String> {
        let Some(permissions) = table.optional_table("permissions")? else {
            return Ok(BTreeMap::new());
        };

        permissions
            .entries
            .iter()
            .map(|(name, level)| {
                let key = permissions.key(name);
                let (name, levels) = PERMISSIONS
                    .iter()
                    .find(|(known, _)| known == name)
                    .ok_or_else(|| {
                        format!(
                            "`{key}` is not a `GITHUB_TOKEN` permission; permissions: {}",
                            PERMISSIONS.map(|(known, _)| known).join(", ")
                        )
                    })?;
                let level = levels
                    .iter()
                    .find(|known| level.as_str() == Some(**known))
                    .ok_or_else(|| format!("`{key}` must be one of: {}", levels.join(", ")))?;

                Ok((*name, *level))
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "workflow_config_test.rs"]
mod tests;
