use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::timing::{
    MockReader, Options, compare, load, number, parse_reports, statistics, write_report,
};

#[path = "../tests/support/timing.rs"]
mod support;

fn options() -> Options {
    Options {
        before: (0..3)
            .map(|index| PathBuf::from(format!("before/{index}/metadata.json")))
            .collect(),
        after: (0..3)
            .map(|index| PathBuf::from(format!("after/{index}/metadata.json")))
            .collect(),
    }
}

fn reader(files: BTreeMap<PathBuf, String>) -> MockReader {
    let mut reader = MockReader::new();
    reader.expect_read().returning(move |path| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| "unreadable fixture".into())
    });

    reader
}

#[test]
fn repeated_comparison_keeps_phase_metrics_and_ranges_separate() {
    // Arrange
    let options = options();
    let host = reader(support::files());
    // Act
    let report = compare(&options, &host).expect("valid comparison");
    let before = load(&options.before, &host).expect("samples");
    let even = statistics(&before[..2], "hook_seconds");
    let mut output = Vec::new();
    write_report(&report, &mut output).expect("writable output");
    let mut full = &mut [][..];
    // Assert
    assert_eq!(report["before_sha"], "before");
    assert_eq!(report["after_sha"], "after");
    assert_eq!(
        report["metrics"]["hook_seconds"]["before"],
        json!({"min":12.0,"median":13.0,"max":14.0})
    );
    assert_eq!(report["metrics"]["hook_seconds"]["after"]["median"], 7.0);
    assert_eq!(
        report["metrics"]["execution_seconds"]["before"]["median"],
        2.5
    );
    assert_eq!(
        report["metrics"]["summed_test_process_seconds"]["before"]["median"],
        3.0
    );
    assert!(report["metrics"]["dirty_units"]["reduction_percent"].is_null());
    assert_eq!(even["median"], 12.5);
    assert_eq!(
        serde_json::from_slice::<Value>(&output).expect("JSON"),
        report
    );
    assert!(write_report(&report, &mut full).is_err());
}

#[test]
fn comparison_rejects_incomplete_mixed_or_duplicate_evidence() {
    for (pointer, value) in [
        ("/schema", json!(2)),
        ("/prepared", json!(false)),
        ("/outcome", json!("failure")),
        ("/hook_seconds", json!(-1)),
        ("/hook_seconds", Value::Null),
        ("/environment/rustc", Value::Null),
        ("/environment/runner_arch", json!("")),
        ("/environment/exact_cache_hit", json!(true)),
        ("/identity/sha", json!("other")),
        ("/identity/suite", json!("other")),
        ("/identity/run_id", json!("before-1")),
        ("/identity/job", json!("")),
        ("/reports/junit.xml", json!("wrong hash")),
    ] {
        // Arrange
        let mut files = support::files();
        let path = PathBuf::from("before/0/metadata.json");
        let mut metadata: Value = serde_json::from_str(&files[&path]).expect("metadata");
        *metadata.pointer_mut(pointer).expect("field") = value;
        files.insert(path, metadata.to_string());
        // Act / Assert
        assert!(
            compare(&options(), &reader(files)).is_err(),
            "accepted {pointer}"
        );
    }
    // Arrange
    let mut options = options();
    options.after.pop();
    // Act / Assert
    assert!(compare(&options, &MockReader::new()).is_err());
}

#[test]
fn missing_corrupt_and_unreadable_reports_are_not_zero_timings() {
    for path in [
        "before/0/metadata.json",
        "before/0/junit.xml",
        "before/0/cargo-timing.html",
        "after/2/metadata.json",
    ] {
        // Arrange
        let mut files = support::files();
        files.remove(&PathBuf::from(path));
        // Act / Assert
        assert!(compare(&options(), &reader(files)).is_err());
    }
    // Arrange
    let mut files = support::files();
    files.insert(PathBuf::from("before/0/metadata.json"), "{".into());
    // Act / Assert
    assert!(compare(&options(), &reader(files)).is_err());
}

#[test]
fn report_parsing_rejects_failures_invalid_numbers_and_truncation() {
    // Arrange / Act / Assert
    let minutes = support::CARGO.replace("4.0s", "93.6s (1m 33.6s)");
    assert_eq!(
        parse_reports(support::JUNIT, &minutes).expect("minute rendering")["build_seconds"],
        93.6
    );
    for junit in [
        "<testsuites>",
        "<testsuite/>",
        &support::JUNIT.replace("failures=\"0\"", "failures=\"1\""),
        &support::JUNIT.replace("errors=\"0\"", "errors=\"1\""),
        &support::JUNIT.replace("time=\"2.5\"", "time=\"NaN\""),
        &support::JUNIT.replace("time=\"1\"", ""),
        &support::JUNIT.replace("tests=\"2\"", ""),
    ] {
        assert!(
            parse_reports(junit, support::CARGO).is_err(),
            "accepted {junit}"
        );
    }
    for cargo in [
        "",
        &support::CARGO.replace("4.0s", "invalids"),
        &support::CARGO.replace("4.0s", "4.0"),
        &support::CARGO.replace("12", "-1"),
    ] {
        assert!(parse_reports(support::JUNIT, cargo).is_err());
    }
    for invalid in ["", "NaN", "inf", "-0.1"] {
        assert!(number(invalid).is_err());
    }
}
