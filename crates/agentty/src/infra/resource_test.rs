#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::time::Duration;

use super::{RealResourceClient, ResourceClient, parse_process_table, process_tree_resources};
use crate::domain::resource::SessionResources;

#[tokio::test]
async fn host_sample_includes_the_current_process_tree() {
    // Arrange
    let client = RealResourceClient;

    // Act
    let samples = tokio::time::timeout(Duration::from_secs(3), client.sample())
        .await
        .expect("accounting completes")
        .expect("host ps available");

    // Assert
    let resources = process_tree_resources(&samples, std::process::id()).expect("root");
    assert!(resources.process_count >= 1);
    assert!(resources.resident_memory_kib > 0);
}

#[cfg(unix)]
#[test]
fn failed_command_and_invalid_utf8_are_unavailable() {
    // Arrange
    let mut output = std::process::Output {
        status: std::process::ExitStatus::from_raw(256),
        stdout: b"1 0 0 1024".to_vec(),
        stderr: Vec::new(),
    };

    // Act / Assert
    assert!(RealResourceClient::parse_output(&output).is_none());
    output.status = std::process::ExitStatus::from_raw(0);
    output.stdout = vec![0xff];
    assert!(RealResourceClient::parse_output(&output).is_none());
}

#[test]
fn tree_totals_include_descendants_and_exclude_other_sessions() {
    // Arrange
    let samples = parse_process_table(
        "30 20 25.5 1024 S\n10 1 100.0 2048 Ss\n20 10 3.0 512 R\n40 1 90.0 8000 S\n",
    )
    .expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 10).expect("root present");

    // Assert
    assert_eq!(
        resources,
        SessionResources {
            cpu_percent: 128.5,
            process_count: 3,
            resident_memory_kib: 3584
        }
    );
    assert_eq!(process_tree_resources(&samples, 999), None);
    assert_eq!(process_tree_resources(&samples, 1), None);
}

#[test]
fn malformed_tables_are_unavailable() {
    // Arrange
    let malformed = [
        "1",
        "x 0 0 1 S",
        "1 x 0 1 S",
        "1 0 x 1 S",
        "1 0 0 x S",
        "1 0 NaN 1 S",
        "1 0 inf 1 S",
        "1 0 -1 1 S",
        "1 0 0 1",
        "1 0 0 1 S extra",
    ];

    // Act / Assert
    for output in malformed {
        assert!(parse_process_table(output).is_none(), "{output}");
    }
    assert!(parse_process_table("\n ").expect("empty table").is_empty());
}

#[test]
fn cycles_and_duplicate_rows_do_not_double_count() {
    // Arrange
    let samples =
        parse_process_table("1 2 1 100 S\n2 1 2 200 S\n2 1 2 200 S").expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 1).expect("root present");

    // Assert
    assert_eq!(resources.process_count, 2);
    assert_eq!(resources.resident_memory_kib, 300);
}

#[test]
fn exited_roots_are_unavailable_and_exited_children_are_excluded() {
    // Arrange
    let samples = parse_process_table("1 0 1 100 S\n2 1 50 200 Z+\n3 1 50 200 X\n4 1 50 200 x")
        .expect("valid table");

    // Act
    let resources = process_tree_resources(&samples, 1).expect("live root");

    // Assert
    assert_eq!(resources.process_count, 1);
    assert_eq!(resources.cpu_percent, 1.0);
    assert_eq!(resources.resident_memory_kib, 100);
    for pid in [2, 3, 4] {
        assert!(process_tree_resources(&samples, pid).is_none());
    }
}
