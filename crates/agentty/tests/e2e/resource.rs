//! Agentty process-tree resource stats in the list tab header.

use std::os::unix::fs::PermissionsExt;

use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::FeatureTest;

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Compact stats for the deterministic process table.
const PROCESS_STATS: &str = "Processes: 2  CPU: 15.0%  RAM: 3.0 MiB";

/// Shows Agentty and descendant process totals beside the list tabs while
/// excluding unrelated host processes.
#[tokio::test]
async fn test_process_resources() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("process_resources")
        .with_terminal_size(120, 24)
        .setup(|env| {
            Box::pin(async move {
                // `ps` runs as a direct child of Agentty, so `$PPID` is the
                // tracked root. The last row is an unrelated host process.
                let ps_path = env.stub_bin.join("ps");
                std::fs::write(
                    &ps_path,
                    r#"#!/bin/sh
printf '%s 1 12.5 2048 S\n2147483640 %s 2.5 1024 S\n2147483639 1 90.0 8192 S\n' "$PPID" "$PPID"
"#,
                )?;
                std::fs::set_permissions(&ps_path, std::fs::Permissions::from_mode(0o750))?;

                Ok(())
            })
        })
        .zola(
            "Process resources",
            "See Agentty's process count, CPU, and memory beside the tabs.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text(PROCESS_STATS, 15000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text(PROCESS_STATS, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("sessions", "Agentty process stats beside the tabs")
            },
            |frame, _report| {
                Box::pin(async move {
                    let header = Region::new(0, 0, frame.cols(), 3);
                    assertion::assert_text_in_region(frame, PROCESS_STATS, &header);
                    assertion::assert_text_in_region(frame, "Sessions", &header);
                })
            },
        )
        .await?;

    Ok(())
}

/// Downstream callers can construct and exhaustively match the totals.
#[test]
fn session_resources_public_shape_remains_exhaustive() {
    // Arrange
    let resources = agentty::domain::resource::SessionResources {
        cpu_percent: 12.5,
        process_count: 2,
        resident_memory_kib: 1024,
    };

    // Act
    let agentty::domain::resource::SessionResources {
        cpu_percent,
        process_count,
        resident_memory_kib,
    } = resources;

    // Assert
    assert_eq!(
        (cpu_percent, process_count, resident_memory_kib),
        (12.5, 2, 1024)
    );
}
