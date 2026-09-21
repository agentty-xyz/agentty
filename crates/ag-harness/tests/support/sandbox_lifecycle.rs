use std::time::Duration;

use ag_harness::{CommandOutcome, CommandTermination};

use super::fixture::{CONFORMANCE_EXECUTORS, Workspace};
#[cfg(target_os = "macos")]
use super::fixture::{Descendant, NativeFixture, wait_file};
#[tokio::test]
async fn main_exit_waits_for_attached_descendants_and_combines_capture_budget() {
    for selected in CONFORMANCE_EXECUTORS {
        // Arrange
        let workspace = Workspace::new();
        let options = workspace.executor_options(selected, Duration::from_secs(5), 17);

        // Act
        let output = workspace
            .harness()
            .run_once_with_options(
                "(/bin/sleep 0.1; printf finished > output/child) & printf 12345678901234567890; \
                 printf abcdefghijklmnopqrstuvwxyz >&2; exit 7",
                options,
            )
            .await
            .expect("turn");
        let result: CommandOutcome = serde_json::from_value(output.into_output()).expect("outcome");

        // Assert
        assert_eq!(result.exit_code, Some(7), "{selected:?}: {result:?}");
        assert_eq!(
            result.termination,
            CommandTermination::Completed,
            "{selected:?}: {result:?}"
        );
        assert!(result.truncated, "{selected:?}: {result:?}");
        assert_eq!(
            result.stdout.len() + result.stderr.len(),
            17,
            "{selected:?}: {result:?}"
        );
        // The read-only native Linux policy leaves no observable marker; the
        // wire-level launcher test proves completion waits for descendants
        // there.
        if selected.observes_markers() {
            assert_eq!(
                std::fs::read_to_string(workspace.path().join("output/child"))
                    .expect("descendant completed"),
                "finished",
                "{selected:?}"
            );
        }
    }
}

#[tokio::test]
async fn timeout_and_caller_drop_settle_through_retained_control() {
    for selected in CONFORMANCE_EXECUTORS {
        // Arrange
        let workspace = Workspace::new();
        let harness = workspace.harness();
        let turn = harness.run_once_controlled(
            "printf ready > output/ready; /bin/sleep 30",
            workspace.executor_options(selected, Duration::from_secs(10), 128),
        );
        let control = turn.control();
        let mut turn = Box::pin(turn);

        // Act
        tokio::select! {
            result = &mut turn => assert!(result.is_err(), "command must wait"),
            () = async {
                if selected.observes_markers() {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        while !workspace.path().join("output/ready").exists() { tokio::time::sleep(Duration::from_millis(5)).await; }
                    }).await.expect("started command");
                } else {
                    // The read-only native Linux policy leaves no observable
                    // marker; a bounded delay lets the command start before
                    // the caller drops.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            } => {}
        }
        drop(turn);
        tokio::time::timeout(Duration::from_secs(5), control.commands_settled())
            .await
            .expect("bounded cleanup")
            .expect("cleanup");
        control
            .effects_settled()
            .await
            .expect("filesystem settlement");
        control.settled().await.expect("persistence settlement");
        let timeout = harness
            .run_once_with_options(
                "/bin/sleep 30",
                workspace.executor_options(selected, Duration::from_millis(500), 128),
            )
            .await
            .expect("timeout turn");
        let timeout: CommandOutcome =
            serde_json::from_value(timeout.into_output()).expect("timeout result");

        // Assert
        assert_eq!(
            timeout.termination,
            CommandTermination::Deadline,
            "{selected:?}: {timeout:?}"
        );
        assert!(!timeout.cleanup_failed, "{selected:?}: {timeout:?}");
        control.retry_commands().await.expect("stale cleanup");
    }
}

/// The Linux equivalent drives the launcher wire directly in
/// `sandbox_launcher.rs`, because the production Linux policy rejects the
/// write grants this synchronization requires.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn native_detached_fork_and_posix_spawn_keep_access_confinement_and_report_scope() {
    // Arrange
    let fixture = NativeFixture::build();
    for mode in ["fork", "spawn"] {
        let workspace = Workspace::new();
        let outside = tempfile::tempdir().expect("outside workspace");
        let forbidden = outside.path().join("forbidden");
        let command = format!(
            "'{}' {mode} output/pid output/release output/result '{}'",
            fixture.executable().display(),
            forbidden.display()
        );
        let harness = workspace.harness();
        let turn = harness.run_once_controlled(command, fixture.options(&workspace));
        let control = turn.control();
        let mut turn = Box::pin(turn);

        // Act
        let pid_path = workspace.path().join("output/pid");
        tokio::select! {
            biased;
            () = wait_file(&pid_path) => {}
            result = &mut turn => assert!(result.is_err(), "descendant must start"),
        }
        let descendant = Descendant(
            std::fs::read_to_string(workspace.path().join("output/pid"))
                .expect("pid")
                .parse()
                .expect("numeric pid"),
        );
        let early = tokio::time::timeout(Duration::from_millis(200), &mut turn).await;
        std::fs::write(workspace.path().join("output/release"), "release")
            .expect("release descendant");
        let result = match early {
            Ok(result) => result.expect("best-effort completion"),
            Err(_) => turn.await.expect("namespace completion"),
        };
        let result: CommandOutcome = serde_json::from_value(result.into_output()).expect("outcome");
        wait_file(&workspace.path().join("output/result")).await;
        control.commands_settled().await.expect("cleanup");
        drop(descendant);

        // Assert
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert!(!forbidden.exists());
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("output/result")).expect("result"),
            "confined"
        );
        assert_eq!(
            result.cleanup_scope,
            ag_harness::CommandCleanupScope::ProcessGroupBestEffort
        );
    }
}

#[tokio::test]
async fn output_flood_cannot_prevent_deadline_cleanup() {
    for selected in CONFORMANCE_EXECUTORS {
        // Arrange
        let workspace = Workspace::new();
        let options = workspace.executor_options(selected, Duration::from_secs(1), 37);

        // Act
        let result = workspace
            .harness()
            .run_once_with_options(
                "while :; do printf 'output-flood'; printf 'stderr-flood' >&2; done",
                options,
            )
            .await
            .expect("turn");
        let result: CommandOutcome = serde_json::from_value(result.into_output()).expect("outcome");

        // Assert
        assert_eq!(
            result.termination,
            CommandTermination::Deadline,
            "{selected:?}: {result:?}"
        );
        assert!(!result.cleanup_failed, "{selected:?}: {result:?}");
        assert!(result.truncated, "{selected:?}: {result:?}");
        assert_eq!(
            result.stdout.len() + result.stderr.len(),
            37,
            "{selected:?}: {result:?}"
        );
    }
}
