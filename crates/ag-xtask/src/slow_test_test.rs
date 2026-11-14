use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::slow_test::{Options, module, parse_allowlist, report};
use crate::timing::MockReader;

const JUNIT: &str = r#"<testsuites tests="7" failures="0" errors="0" time="12.5"><testsuite>
<testcase name="app::review::tests::quick" classname="agentty" time="0.004"/>
<testcase name="app::review::tests::medium" classname="agentty" time="0.2"/>
<testcase name="ui::render" classname="agentty" time="0.6"/>
<testcase name="app::state::tests::tie" classname="agentty" time="2"/>
<testcase name="top_level" classname="ag-git::client" time="2"/>
<testcase name="sandbox::flood" classname="ag-harness::sandbox" time="7"/>
<testcase name="app::review::tests::slow_batch" classname="agentty" time="12"/>
</testsuite></testsuites>"#;

const REPORT: &str = "\
7 tests: 23.804 s summed test time, 12.500 s suite time

Slowest tests (top 4 of 7):
   12.000 s  agentty  app::review::tests::slow_batch
    7.000 s  ag-harness::sandbox  sandbox::flood
    2.000 s  ag-git::client  top_level
    2.000 s  agentty  app::state::tests::tie

Binaries by summed time (top 4 of 3):
   14.804 s       5 tests  agentty
    7.000 s       1 tests  ag-harness::sandbox
    2.000 s       1 tests  ag-git::client

Modules at depth 2 by summed time (top 4 of 5):
   12.204 s       3 tests  agentty  app::review
    7.000 s       1 tests  ag-harness::sandbox  sandbox
    2.000 s       1 tests  ag-git::client  (root)
    2.000 s       1 tests  agentty  app::state

Duration bands:
    0.004 s       1 tests  <10ms
    0.000 s       0 tests  10-50ms
    0.000 s       0 tests  50-100ms
    0.200 s       1 tests  100-500ms
    0.600 s       1 tests  0.5-1s
    4.000 s       2 tests  1-5s
   19.000 s       2 tests  >=5s
";

const SLOW_BATCH: &str = "- binary: agentty
  test: app::review::tests::slow_batch
  reason: Replays every review batch.
";

const FLOOD: &str = "- binary: ag-harness::sandbox
  test: sandbox::flood
  reason: Waits for the sandbox deadline.
";

fn options(max_seconds: Option<f64>, allowlist: bool) -> Options {
    Options {
        allowlist: allowlist.then(|| PathBuf::from("allowlist.yaml")),
        max_seconds,
        module_depth: 2,
        report: PathBuf::from("junit.xml"),
        top: 4,
    }
}

fn reader(junit: &str, allowlist: Option<&str>) -> MockReader {
    let mut files = BTreeMap::from([(PathBuf::from("junit.xml"), junit.to_string())]);
    if let Some(allowlist) = allowlist {
        files.insert(PathBuf::from("allowlist.yaml"), allowlist.to_string());
    }
    let mut reader = MockReader::new();
    reader.expect_read().returning(move |path| {
        files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("{}: unreadable fixture", path.display()))
    });

    reader
}

fn run(options: &Options, reader: &MockReader) -> (Result<(), String>, String) {
    let mut output = Vec::new();
    let result = report(options, reader, &mut output);

    (result, String::from_utf8(output).expect("UTF-8 report"))
}

#[test]
fn report_ranks_tests_binaries_modules_and_duration_bands() {
    // Arrange
    let options = options(None, false);

    // Act
    let (result, output) = run(&options, &reader(JUNIT, None));

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(output, REPORT);
}

#[test]
fn check_fails_only_for_unlisted_tests_over_the_threshold() {
    // Arrange
    let listed = options(Some(5.0), true);
    let unlisted = options(Some(5.0), false);

    // Act
    let (listed_result, listed_output) = run(&listed, &reader(JUNIT, Some(SLOW_BATCH)));
    let (unlisted_result, unlisted_output) = run(&unlisted, &reader(JUNIT, None));

    // Assert
    let error = listed_result.expect_err("unlisted slow test");
    assert!(error.starts_with("1 tests exceed"), "{error}");
    assert!(error.contains("ag-harness::sandbox  sandbox::flood"));
    assert!(!error.contains("slow_batch"));
    assert!(listed_output.starts_with(REPORT));
    assert!(listed_output.ends_with("Threshold 5 s: 2 over, 1 allowlisted, 1 new\n"));
    assert!(
        unlisted_result
            .expect_err("no allowlist")
            .starts_with("2 tests exceed")
    );
    assert!(unlisted_output.ends_with("Threshold 5 s: 2 over, 0 allowlisted, 2 new\n"));
}

#[test]
fn stale_allowlist_entries_warn_without_failing() {
    // Arrange
    let options = options(Some(5.0), true);
    let allowlist = format!(
        "{SLOW_BATCH}{FLOOD}- binary: agentty
  test: app::review::tests::quick
  reason: Fixed by a concurrent change.
- binary: agentty
  test: app::removed
  reason: Deleted by a concurrent change.
"
    );

    // Act
    let (result, output) = run(&options, &reader(JUNIT, Some(&allowlist)));

    // Assert
    assert_eq!(result, Ok(()));
    assert!(output.ends_with(
        "Threshold 5 s: 2 over, 2 allowlisted, 0 new
warning: allowlisted test took 0.004 s, within 5 s; remove it once it stays fast: agentty  \
         app::review::tests::quick
warning: allowlisted test not in report; remove it if it is gone: agentty  app::removed
"
    ));
}

#[test]
fn unreadable_invalid_or_unwritable_inputs_fail() {
    for (junit, allowlist, expected) in [
        (
            "<testsuites>",
            Some(SLOW_BATCH.to_string()),
            "junit.xml: Invalid JUnit",
        ),
        (JUNIT, None, "allowlist.yaml: unreadable fixture"),
        (
            JUNIT,
            Some("binary: agentty".into()),
            "allowlist.yaml: Invalid allowlist",
        ),
        (
            JUNIT,
            Some(SLOW_BATCH.replace("reason", "note")),
            "Invalid allowlist",
        ),
        (
            JUNIT,
            Some(SLOW_BATCH.replace("Replays every review batch.", "' '")),
            "needs a binary, test, and reason",
        ),
        (
            JUNIT,
            Some(format!("{SLOW_BATCH}{SLOW_BATCH}")),
            "Duplicate allowlist entry `agentty  app::review::tests::slow_batch`",
        ),
    ] {
        // Arrange
        let reader = reader(junit, allowlist.as_deref());

        // Act
        let (result, _) = run(&options(Some(5.0), true), &reader);

        // Assert
        let error = result.expect_err("rejected input");
        assert!(error.contains(expected), "{error}");
    }
    // Arrange
    let mut missing = MockReader::new();
    missing
        .expect_read()
        .returning(|path| Err(format!("{}: missing", path.display())));
    let mut full = &mut [][..];

    // Act
    let unreadable = report(&options(None, false), &missing, &mut Vec::new());
    let unwritable = report(&options(None, false), &reader(JUNIT, None), &mut full);

    // Assert
    assert_eq!(unreadable, Err("junit.xml: missing".into()));
    assert!(unwritable.is_err());
}

#[test]
fn modules_keep_leading_path_segments_without_the_test_name() {
    for (name, depth, expected) in [
        ("app::review::tests::slow", 2, "app::review"),
        ("app::review::tests::slow", 3, "app::review::tests"),
        ("tests::slow", 2, "tests"),
        ("slow", 2, "(root)"),
        ("app::slow", 0, "(root)"),
    ] {
        // Arrange / Act
        let module = module(name, depth);

        // Assert
        assert_eq!(module, expected, "{name} at depth {depth}");
    }
}

#[test]
fn committed_allowlist_names_each_known_offender_once_with_a_reason() {
    // Arrange
    let allowlist = include_str!("../../../.config/slow-tests.yaml");

    // Act
    let entries = parse_allowlist(allowlist).expect("valid committed allowlist");

    // Assert
    assert!(!entries.is_empty());
}
