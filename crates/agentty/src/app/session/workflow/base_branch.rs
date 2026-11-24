//! Guard against a Harness turn moving its session's base branch or
//! retargeting the worktree's `HEAD`.
//!
//! Harness commands may write Git references in the shared repository.
//! Moving the base under the session would hide the session's changes from
//! the review diff that Agentty commits, merges, and pushes from, so every
//! Harness turn that is followed by commit or publish effects records the
//! base before it runs and fails when it moved. The sandbox keeps every
//! branch but the session's read-only, but commands can still point `HEAD`,
//! or the session branch itself, at another branch, which Agentty's own
//! commit would then move. So `HEAD` must resolve to the session branch, or
//! be detached as during a rebase, both before such a turn runs and after.

use std::path::Path;

use ag_contracts::{AgentError, TurnResult};
use ag_git::GitClient;

use super::session_branch;
use crate::app::session::SessionError;
use crate::domain::agent::AgentKind;
use crate::infra::db::AppRepositories;

/// What `GitClient::head_reference` reports for a detached `HEAD`.
const DETACHED_HEAD: &str = "HEAD";

/// The session and repository whose base branch a turn must leave in place.
pub(super) struct BaseBranchScope<'a> {
    pub(super) db: &'a AppRepositories,
    pub(super) folder: &'a Path,
    pub(super) git_client: &'a dyn GitClient,
    pub(super) session_id: &'a str,
}

/// The base branch commit captured before a Harness turn, resolved the way
/// the review diff resolves the session's recorded base.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct BaseBranchSnapshot {
    commit: String,
    reference: String,
}

impl BaseBranchSnapshot {
    /// Captures the base before a turn run by `agent`: `None` for agents
    /// other than Harness and for a session without a recorded base.
    ///
    /// # Errors
    /// Returns an error when the recorded base cannot be read or no longer
    /// resolves, for example because an earlier turn deleted it, so the turn
    /// never runs on a base Agentty cannot check, or when `HEAD` resolves to
    /// a branch other than the session's, for example because an earlier
    /// turn retargeted it.
    pub(super) async fn capture(
        scope: &BaseBranchScope<'_>,
        agent: AgentKind,
    ) -> Result<Option<Self>, SessionError> {
        if agent != AgentKind::Harness {
            return Ok(None);
        }
        let reference = scope
            .db
            .sessions()
            .get_session_base_branch(scope.session_id)
            .await?;
        let Some(reference) = reference else {
            return Ok(None);
        };
        let commit = scope
            .git_client
            .ref_hash(scope.folder.to_path_buf(), reference.clone())
            .await
            .map_err(|error| {
                SessionError::Workflow(format!(
                    "The base branch `{reference}` does not resolve ({error}), so Agentty does \
                     not run Harness commands that it would commit or publish on top of it. \
                     Restore the branch in the repository, then send the prompt again."
                ))
            })?;
        Self::check_head(scope)
            .await
            .map_err(SessionError::Workflow)?;

        Ok(Some(Self { commit, reference }))
    }

    /// Fails a successful turn that moved or deleted the base `snapshot`
    /// recorded, or left the worktree's `HEAD` resolving to another branch,
    /// so Agentty commits, reviews, and publishes nothing on top of it; a
    /// turn without a snapshot passes unchanged.
    ///
    /// # Errors
    /// Returns the turn's own error, or a backend error naming the moved
    /// base or retargeted `HEAD` and how to restore it.
    pub(super) async fn verify(
        snapshot: Option<&Self>,
        scope: &BaseBranchScope<'_>,
        turn_result: Result<TurnResult, AgentError>,
    ) -> Result<TurnResult, AgentError> {
        let Some(snapshot) = snapshot else {
            return turn_result;
        };
        let result = turn_result?;
        let current = scope
            .git_client
            .ref_hash(scope.folder.to_path_buf(), snapshot.reference.clone())
            .await
            .ok();
        if current.as_deref() != Some(snapshot.commit.as_str()) {
            return Err(Self::moved_base(snapshot));
        }
        Self::check_head(scope).await.map_err(AgentError::Backend)?;

        Ok(result)
    }

    /// Checks that the worktree's `HEAD`, following every symbolic
    /// reference, names the session branch or is detached.
    ///
    /// # Errors
    /// Returns a message naming the branch `HEAD` resolves to instead, or
    /// why it does not resolve, and how to point it back.
    async fn check_head(scope: &BaseBranchScope<'_>) -> Result<(), String> {
        let expected = format!("refs/heads/{}", session_branch(scope.session_id));
        let head = scope
            .git_client
            .head_reference(scope.folder.to_path_buf())
            .await
            .map_err(|error| error.to_string());
        if matches!(&head, Ok(head) if *head == expected || head == DETACHED_HEAD) {
            return Ok(());
        }
        let found = match head {
            Ok(head) => format!("resolves to `{head}`"),
            Err(error) => format!("does not resolve ({error})"),
        };

        Err(format!(
            "The session worktree's `HEAD` {found} instead of `{expected}`, so Agentty does not \
             commit or publish the session or run Harness commands on it. Point it back, for \
             example with `git symbolic-ref HEAD {expected}` in the worktree, and make sure the \
             session branch is not a symbolic reference, then send the prompt again."
        ))
    }

    /// Returns the error of a turn that moved or deleted the base `snapshot`
    /// recorded.
    fn moved_base(snapshot: &Self) -> AgentError {
        AgentError::Backend(format!(
            "The base branch `{reference}` moved during this turn, so Agentty stopped before \
             committing or publishing the session. If the session's commands moved it, restore it \
             to `{commit}` in the repository, for example with `git branch --force {reference} \
             {commit}` for a local branch; if you moved it yourself, send the prompt again.",
            reference = snapshot.reference,
            commit = snapshot.commit,
        ))
    }
}

#[cfg(test)]
#[path = "base_branch_test.rs"]
mod tests;
