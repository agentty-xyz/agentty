//! Host-selected commit identities for repository comparisons.

use std::io;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::read::command::{LocalRepositoryCommandRunner, RepositoryCommandRunner};
use crate::repository::Repository;
use crate::schema_contract;

/// A full commit OID validated in one repository scope by the host.
///
/// Branch selection and review-baseline policy belong to the host. This value
/// never follows a moving reference after construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComparisonBase {
    identity: ComparisonIdentity,
}

impl ComparisonBase {
    /// Resolves an explicit host revision, peeling tags to a commit.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision does not resolve to a commit in the
    /// configured repository or the bounded Git command fails.
    pub async fn resolve(
        repository: &Repository,
        revision: &str,
    ) -> Result<Self, ComparisonBaseError> {
        let runner = LocalRepositoryCommandRunner::new(repository.git_executable().to_path_buf());

        Self::resolve_with_runner(repository, revision, &runner).await
    }

    /// Validates a full OID naming a commit directly, without peeling tags.
    ///
    /// # Errors
    ///
    /// Rejects abbreviated OIDs, revisions, missing objects, and non-commits.
    pub async fn validate(repository: &Repository, oid: &str) -> Result<Self, ComparisonBaseError> {
        let runner = LocalRepositoryCommandRunner::new(repository.git_executable().to_path_buf());

        Self::validate_with_runner(repository, oid, &runner).await
    }

    /// Returns the canonical full commit OID used for every comparison.
    pub fn oid(&self) -> &str {
        &self.identity.oid
    }

    pub(crate) fn identity(&self) -> &ComparisonIdentity {
        &self.identity
    }

    pub(crate) fn matches_repository(&self, repository: &Repository) -> bool {
        self.identity.repository_root == repository.root().as_os_str().as_encoded_bytes()
    }

    async fn resolve_with_runner(
        repository: &Repository,
        revision: &str,
        runner: &dyn RepositoryCommandRunner,
    ) -> Result<Self, ComparisonBaseError> {
        if revision.is_empty() || revision.len() > 4096 || revision.chars().any(char::is_control) {
            return Err(ComparisonBaseError::InvalidRevision);
        }
        let oid = Self::command(
            repository,
            runner,
            &[
                "rev-parse".into(),
                "--verify".into(),
                "--end-of-options".into(),
                format!("{revision}^{{commit}}"),
            ],
        )
        .await?;

        Self::validate_with_runner(repository, &oid, runner).await
    }

    async fn validate_with_runner(
        repository: &Repository,
        oid: &str,
        runner: &dyn RepositoryCommandRunner,
    ) -> Result<Self, ComparisonBaseError> {
        if !ComparisonIdentity::valid_oid(oid) {
            return Err(ComparisonBaseError::InvalidOid);
        }
        let oid = oid.to_ascii_lowercase();
        let kind = Self::command(
            repository,
            runner,
            &["cat-file".into(), "-t".into(), oid.clone()],
        )
        .await?;
        if kind != "commit" {
            return Err(ComparisonBaseError::NotCommit);
        }

        Ok(Self {
            identity: ComparisonIdentity {
                oid,
                repository_root: repository.root().as_os_str().as_encoded_bytes().to_vec(),
            },
        })
    }

    async fn command(
        repository: &Repository,
        runner: &dyn RepositoryCommandRunner,
        arguments: &[String],
    ) -> Result<String, ComparisonBaseError> {
        let output = runner.run(repository.root(), arguments).await?;
        if output.code != Some(0) || output.truncated {
            return Err(ComparisonBaseError::Rejected {
                detail: schema_contract::bounded_diagnostic(
                    String::from_utf8_lossy(&output.stderr).trim(),
                ),
            });
        }
        let text =
            String::from_utf8(output.stdout).map_err(|_| ComparisonBaseError::InvalidOutput)?;

        Ok(text.trim_end_matches(['\n', '\r']).to_string())
    }
}

/// Comparison metadata in history is not proof of live repository validation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ComparisonIdentity {
    oid: String,
    repository_root: Vec<u8>,
}

impl ComparisonIdentity {
    pub(crate) fn is_valid(&self) -> bool {
        Self::valid_oid(&self.oid) && !self.repository_root.is_empty()
    }

    fn valid_oid(oid: &str) -> bool {
        matches!(oid.len(), 40 | 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
}

/// Failure to validate the host's explicit comparison selection.
#[derive(Debug, Error)]
pub enum ComparisonBaseError {
    /// A revision is empty, oversized, or contains control characters.
    #[error("comparison revision must be nonempty bounded text without control characters")]
    InvalidRevision,
    /// Only full hexadecimal object identifiers are accepted by validation.
    #[error("comparison base must be a full hexadecimal commit OID")]
    InvalidOid,
    /// The selected object exists but is not a commit.
    #[error("comparison base object is not a commit")]
    NotCommit,
    /// Git returned an invalid textual response.
    #[error("comparison validation returned non-UTF-8 output")]
    InvalidOutput,
    /// The command could not complete.
    #[error("comparison validation failed: {0}")]
    Command(#[from] io::Error),
    /// Git rejected the selection or returned incomplete output.
    #[error("comparison validation was rejected: {detail}")]
    Rejected {
        /// Bounded Git diagnostic.
        detail: String,
    },
}

#[cfg(test)]
#[path = "comparison_fixture_test.rs"]
pub(crate) mod support;

#[cfg(test)]
#[path = "comparison_test.rs"]
mod tests;
