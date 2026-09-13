use std::io;

use mockall::Sequence;

use super::{ComparisonBase, ComparisonBaseError, ComparisonIdentity};
use crate::comparison::support::{COMPARISON_OID, ComparisonRepository};
use crate::read::command::{MockRepositoryCommandRunner, RepositoryCommandOutput};
use crate::repository::Repository;

fn output(text: impl Into<Vec<u8>>) -> RepositoryCommandOutput {
    RepositoryCommandOutput {
        code: Some(0),
        stderr: Vec::new(),
        stdout: text.into(),
        truncated: false,
    }
}

#[tokio::test]
async fn resolution_peels_once_and_validates_the_returned_commit() {
    // Arrange
    let repository = Repository::fixture("repo");
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|_, arguments| {
            arguments
                == [
                    "rev-parse",
                    "--verify",
                    "--end-of-options",
                    "release^{commit}",
                ]
        })
        .returning(|_, _| Ok(output(format!("{COMPARISON_OID}\n"))));
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .withf(|_, arguments| arguments == ["cat-file", "-t", COMPARISON_OID])
        .returning(|_, _| Ok(output("commit\n")));

    // Act
    let base = ComparisonBase::resolve_with_runner(&repository, "release", &runner)
        .await
        .expect("commit");

    // Assert
    assert_eq!(base.oid(), COMPARISON_OID);
    assert!(base.matches_repository(&repository));
    assert!(!base.matches_repository(&Repository::fixture("other")));
}

#[tokio::test]
async fn malformed_host_values_never_run_git() {
    // Arrange
    let repository = Repository::fixture("repo");
    let runner = MockRepositoryCommandRunner::new();

    // Act
    let mut errors = Vec::new();
    for oid in ["", "HEAD", "abc", &"z".repeat(40), &"a".repeat(41)] {
        errors.push(ComparisonBase::validate_with_runner(&repository, oid, &runner).await);
    }
    let mut revisions = Vec::new();
    for revision in ["", "head\n", &"x".repeat(4097)] {
        revisions.push(ComparisonBase::resolve_with_runner(&repository, revision, &runner).await);
    }

    // Assert
    assert!(
        errors
            .iter()
            .all(|error| matches!(error, Err(ComparisonBaseError::InvalidOid)))
    );
    assert!(
        revisions
            .iter()
            .all(|error| matches!(error, Err(ComparisonBaseError::InvalidRevision)))
    );
}

#[tokio::test]
async fn validation_requires_commit_type_and_canonicalizes_full_oids() {
    // Arrange
    let repository = Repository::fixture("repo");
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    for kind in ["blob", "tree", "tag", "commit", "commit"] {
        runner
            .expect_run()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, _| Ok(output(kind)));
    }

    // Act
    let mut rejected = Vec::new();
    for _ in 0..3 {
        rejected
            .push(ComparisonBase::validate_with_runner(&repository, COMPARISON_OID, &runner).await);
    }
    let sha1 = ComparisonBase::validate_with_runner(&repository, &"A".repeat(40), &runner)
        .await
        .expect("sha1");
    let sha256 = ComparisonBase::validate_with_runner(&repository, &"B".repeat(64), &runner)
        .await
        .expect("sha256");

    // Assert
    assert!(
        rejected
            .iter()
            .all(|error| matches!(error, Err(ComparisonBaseError::NotCommit)))
    );
    assert_eq!(sha1.oid(), "a".repeat(40));
    assert_eq!(sha256.oid(), "b".repeat(64));
}

#[tokio::test]
async fn validation_rejects_failed_truncated_and_invalid_command_output() {
    // Arrange
    let repository = Repository::fixture("repo");
    let mut runner = MockRepositoryCommandRunner::new();
    let mut sequence = Sequence::new();
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Err(io::Error::other("unavailable")));
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            let mut output = output("");
            output.code = Some(128);
            output.stderr = b"missing object".to_vec();
            Ok(output)
        });
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            let mut output = output("commit");
            output.truncated = true;
            Ok(output)
        });
    runner
        .expect_run()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| Ok(output([255])));

    // Act
    let command = ComparisonBase::validate_with_runner(&repository, COMPARISON_OID, &runner).await;
    let missing = ComparisonBase::validate_with_runner(&repository, COMPARISON_OID, &runner).await;
    let truncated =
        ComparisonBase::validate_with_runner(&repository, COMPARISON_OID, &runner).await;
    let invalid = ComparisonBase::validate_with_runner(&repository, COMPARISON_OID, &runner).await;

    // Assert
    assert!(matches!(command, Err(ComparisonBaseError::Command(_))));
    assert!(
        matches!(missing, Err(ComparisonBaseError::Rejected { detail }) if detail == "missing object")
    );
    assert!(matches!(
        truncated,
        Err(ComparisonBaseError::Rejected { .. })
    ));
    assert!(matches!(invalid, Err(ComparisonBaseError::InvalidOutput)));
}

#[test]
fn historical_identity_validation_needs_no_live_repository() {
    // Arrange
    let valid = ComparisonBase::fixture("removed-repository")
        .identity()
        .clone();
    let invalid_oid = ComparisonIdentity {
        oid: "HEAD".into(),
        ..valid.clone()
    };
    let invalid_root = ComparisonIdentity {
        repository_root: Vec::new(),
        ..valid.clone()
    };

    // Act / Assert
    assert!(valid.is_valid());
    assert!(!invalid_oid.is_valid());
    assert!(!invalid_root.is_valid());
}

#[tokio::test]
async fn real_git_resolves_tags_and_detached_head_without_main_and_rejects_non_commits() {
    // Arrange
    let fixture = ComparisonRepository::new().await;

    // Act
    let base = ComparisonBase::resolve(&fixture.repository, "v1")
        .await
        .expect("peeled tag");
    let validated = ComparisonBase::validate(&fixture.repository, &fixture.base)
        .await
        .expect("commit OID");
    let mut invalid = Vec::new();
    for oid in [&fixture.tag, &fixture.blob, &fixture.tree, COMPARISON_OID] {
        invalid.push(ComparisonBase::validate(&fixture.repository, oid).await);
    }
    let absent = ComparisonBase::resolve(&fixture.repository, "main").await;
    fixture.move_branch().await;
    let next = ComparisonBase::resolve(&fixture.repository, "release")
        .await
        .expect("next invocation");
    tokio::fs::write(fixture.directory.path().join(".git/HEAD"), &fixture.base)
        .await
        .expect("detached HEAD");
    let detached = ComparisonBase::resolve(&fixture.repository, "HEAD")
        .await
        .expect("detached commit");

    // Assert
    assert_eq!(base, validated);
    assert_eq!(base.oid(), fixture.base);
    assert_eq!(next.oid(), fixture.next);
    assert_eq!(detached, base);
    assert!(invalid.iter().all(Result::is_err));
    assert!(absent.is_err());
}
