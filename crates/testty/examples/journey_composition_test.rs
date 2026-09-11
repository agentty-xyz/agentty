#[path = "support_test.rs"]
mod support;

use std::io;

use testty::journey::{Journey, StartupWait};

use super::{main, print_journey, print_startup_preset, run};

#[test]
fn reports_presets_composed_scenarios_and_raw_steps() {
    // Arrange
    let mut output = Vec::new();

    // Act
    run(&mut output).expect("showcase output");
    let text = String::from_utf8(output).expect("UTF-8 output");

    // Assert
    for expected in [
        "default preset",
        "fast-native preset",
        "slow-node preset",
        "smoke_startup",
        "settings_navigation",
        "full_workflow",
        "manual_test",
    ] {
        assert!(text.contains(expected), "missing {expected}");
    }
}

#[test]
fn reports_output_failures_in_showcase_and_helpers() {
    // Arrange
    let journey = Journey::wait_for_startup_default();
    let mut output = io::Cursor::new([]);

    // Act
    let showcase = run(&mut output);
    let summary = print_journey(&journey, &mut output);
    let preset = print_startup_preset("default", &journey, StartupWait::Default, &mut output);

    // Assert
    for result in [showcase, summary, preset] {
        assert_eq!(
            result.expect_err("full output").kind(),
            io::ErrorKind::WriteZero
        );
    }
}

#[test]
fn entry_point_writes_the_showcase() {
    // Arrange / Act
    let result = main();

    // Assert
    assert!(result.is_ok());
}

#[test]
fn propagates_disconnections_at_every_output_write() {
    // Arrange / Act / Assert
    support::assert_output_failures(run);
}
