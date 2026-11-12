use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::check_coverage::{FileGap, check, definition_names};

/// Two crate builds of one non-generic method, as `llvm-cov export` writes
/// them for a unit-test binary and the library linked into other tests.
const UNIT_TEST_COPY: &str = "_RNvMCsdOQmcqkM9GC_12ag_clipboardNtB2_9Clipboard3new";
const LIBRARY_COPY: &str = "_RNvMCs5ztIjVQvkMA_12ag_clipboardNtB2_9Clipboard3new";

/// Writer whose every write fails, standing in for a closed output stream.
struct ClosedOutput;

impl Write for ClosedOutput {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Joins report or output lines, each terminated by a newline.
fn text(lines: &[&str]) -> String {
    let mut text = lines.join("\n");
    text.push('\n');

    text
}

fn covered_file(path: &str) -> String {
    text(&[
        &format!("SF:/repo/{path}"),
        &format!("FN:1,{UNIT_TEST_COPY}"),
        &format!("FN:1,{LIBRARY_COPY}"),
        &format!("FNDA:3,{UNIT_TEST_COPY}"),
        &format!("FNDA:0,{LIBRARY_COPY}"),
        "FNF:1",
        "FNH:1",
        "DA:1,3",
        "DA:2,3",
        "BRF:0",
        "BRH:0",
        "LF:2",
        "LH:2",
        "end_of_record",
    ])
}

fn checked(report: &str, prefixes: &[&str]) -> (Result<(), String>, String) {
    let prefixes: Vec<PathBuf> = prefixes.iter().map(PathBuf::from).collect();
    let mut output = Vec::new();
    let result = check(report, Path::new("/repo"), &prefixes, &mut output);

    (
        result,
        String::from_utf8(output).expect("UTF-8 coverage report"),
    )
}

#[test]
fn definition_names_shorten_generic_arguments_and_key_every_instantiation_alike() {
    for (demangled, name, definition) in [
        ("ag_xtask::run", "ag_xtask::run", "ag_xtask::run"),
        (
            "<ag::Clipboard>::new_with_backend::<alloc::boxed::Box<dyn ag::Backend>, ag::make>",
            "<ag::Clipboard>::new_with_backend::<..>",
            "<>::new_with_backend::<>",
        ),
        (
            "<alloc::boxed::Box<dyn Fn() -> u8> as ag::Into<u8>>::into::{closure#0}",
            "<alloc::boxed::Box<..> as ag::Into<..>>::into::{closure#0}",
            "<>::into::{closure#0}",
        ),
        (
            "<<ag::A as ag::B>::C as ag::D<<ag::E as ag::F>::G>>::method",
            "<<ag::A as ag::B>::C as ag::D<..>>::method",
            "<>::method",
        ),
        ("ag::a>b", "ag::a>b", "ag::a>b"),
    ] {
        // Arrange
        let expected = (name.to_string(), definition.to_string());

        // Act
        let names = definition_names(demangled);

        // Assert
        assert_eq!(names, expected, "{demangled}");
    }
}

#[test]
fn line_ranges_compact_consecutive_lines() {
    for (lines, expected) in [
        (&[][..], ""),
        (&[7][..], "7"),
        (&[1, 2, 3, 5, 7, 8][..], "1-3,5,7-8"),
    ] {
        // Arrange
        let lines = lines.to_vec();

        // Act
        let ranges = FileGap::line_ranges(&lines);

        // Assert
        assert_eq!(ranges, expected);
    }
}

#[test]
fn complete_reports_pass_when_one_instantiation_covers_each_definition() {
    // Arrange
    let report = format!(
        "TN:\n{}\n{}",
        covered_file("crates/b/src/lib.rs"),
        covered_file("crates/a/src/lib.rs")
    );

    // Act
    let (result, output) = checked(&report, &[]);

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(
        output,
        "Coverage complete: 2 files have no uncovered lines or functions.\n"
    );
}

#[test]
fn path_prefixes_select_whole_components_and_reject_empty_selections() {
    // Arrange
    let uncovered = |source: &str| {
        text(&[
            source,
            "FNF:0",
            "FNH:0",
            "DA:1,0",
            "LF:1",
            "LH:0",
            "end_of_record",
        ])
    };
    let report = format!(
        "{}{}{}",
        covered_file("crates/a/src/lib.rs"),
        uncovered("SF:/repo/crates/ab/src/lib.rs"),
        uncovered("SF:/elsewhere/lib.rs")
    );

    for (prefixes, expected) in [
        (&["crates/a"][..], Ok(())),
        (
            &["crates/ab/src/lib.rs"][..],
            Err(
                "Coverage incomplete in 1 of 1 files: 1 uncovered lines and 0 uncovered functions"
                    .to_string(),
            ),
        ),
        (
            &["/elsewhere"][..],
            Err(
                "Coverage incomplete in 1 of 1 files: 1 uncovered lines and 0 uncovered functions"
                    .to_string(),
            ),
        ),
        (
            &["crates/c", "crates/d"][..],
            Err("No coverage records match the selected paths: crates/c, crates/d".to_string()),
        ),
    ] {
        // Act
        let (result, _) = checked(&report, prefixes);

        // Assert
        assert_eq!(result, expected, "{prefixes:?}");
    }
}

#[test]
fn gap_report_lists_zero_count_lines_and_uncovered_functions_only() {
    // Arrange
    let gaps = text(&[
        "SF:/repo/crates/a/src/lib.rs",
        "FN:3,crate::f::<u8>",
        "FN:3,crate::f::<u16>",
        "FN:8,crate::g",
        "FN:8,crate::g::{closure#0}",
        "FNDA:0,crate::f::<u8>",
        "FNDA:0,crate::f::<u16>",
        "FNDA:2,crate::g",
        "FNDA:0,crate::g::{closure#0}",
        "FNF:3",
        "FNH:1",
        "DA:9,0",
        "DA:3,0",
        "DA:4,0",
        "DA:5,0",
        "DA:8,2",
        "DA:9,0",
        "LF:10",
        "LH:4",
        "end_of_record",
    ]);
    // LLVM's `LH` below `LF` without a zero-count line only reflects lines
    // spread across separately built instantiations.
    let instantiated_lines = text(&[
        "SF:/repo/crates/b/src/lib.rs",
        &format!("FN:1,{UNIT_TEST_COPY}"),
        &format!("FN:1,{LIBRARY_COPY}"),
        "FN:4,crate::h",
        &format!("FNDA:3,{UNIT_TEST_COPY}"),
        &format!("FNDA:1,{LIBRARY_COPY}"),
        "FNDA:1,crate::h",
        "FNF:2",
        "FNH:2",
        "DA:1,4",
        "DA:4,1",
        "LF:5",
        "LH:4",
        "end_of_record",
    ]);
    let unnamed_function = text(&[
        "SF:/repo/crates/c/src/lib.rs",
        "FN:1,crate::k",
        "FNDA:1,crate::k",
        "FNF:2",
        "FNH:1",
        "DA:1,1",
        "LF:1",
        "LH:1",
        "end_of_record",
    ]);
    let report = format!(
        "{}{unnamed_function}{instantiated_lines}{gaps}",
        covered_file("crates/d/src/lib.rs")
    );

    // Act
    let (result, output) = checked(&report, &[]);

    // Assert
    assert_eq!(
        result,
        Err(
            "Coverage incomplete in 2 of 4 files: 4 uncovered lines and 3 uncovered functions"
                .to_string()
        )
    );
    assert_eq!(
        output,
        text(&[
            "crates/a/src/lib.rs",
            "  uncovered lines: 3-5,9",
            "  uncovered functions (start line, name):",
            "    3 crate::f::<..>",
            "    8 crate::g::{closure#0}",
            "crates/c/src/lib.rs",
            "  FNF-FNH is 1 but the function records show 0 uncovered functions",
        ])
    );
}

#[test]
fn never_run_function_records_fail_when_totals_show_no_gap() {
    for totals in [["FNF:0", "FNH:0"], ["FNF:2", "FNH:2"]] {
        // Arrange
        let report = text(&[
            "SF:/repo/a.rs",
            "FN:1,f",
            "FN:2,g",
            "FNDA:0,f",
            "FNDA:1,g",
            totals[0],
            totals[1],
            "DA:1,1",
            "DA:2,1",
            "LF:2",
            "LH:2",
            "end_of_record",
        ]);

        // Act
        let (result, output) = checked(&report, &[]);

        // Assert
        assert_eq!(
            result,
            Err(
                "Coverage incomplete in 1 of 1 files: 0 uncovered lines and 1 uncovered functions"
                    .to_string()
            ),
            "{totals:?}"
        );
        assert_eq!(
            output,
            text(&[
                "a.rs",
                "  uncovered functions (start line, name):",
                "    1 f",
                "  FNF-FNH is 0 but the function records show 1 uncovered functions",
            ]),
            "{totals:?}"
        );
    }
}

#[test]
fn incomplete_records_fail_naming_the_file_and_missing_data() {
    for (lines, expected) in [
        (
            &["SF:/repo/a.rs", "end_of_record"][..],
            "LCOV line 2: the record for a.rs lacks totals: `FNF`, `FNH`, `LF`, `LH`",
        ),
        (
            &[
                "SF:/repo/a.rs",
                "FN:1,f",
                "FNDA:0,f",
                "DA:1,1",
                "LF:1",
                "LH:1",
                "end_of_record",
            ][..],
            "LCOV line 7: the record for a.rs lacks totals: `FNF`, `FNH`",
        ),
        (
            &[
                "SF:/repo/a.rs",
                "FN:1,f",
                "FNDA:1,f",
                "FNF:1",
                "FNH:1",
                "DA:1,1",
                "LF:1",
                "end_of_record",
            ][..],
            "LCOV line 8: the record for a.rs lacks totals: `LH`",
        ),
        (
            &[
                "SF:/repo/a.rs",
                "FNF:0",
                "FNH:0",
                "LF:0",
                "LH:0",
                "end_of_record",
            ][..],
            "LCOV line 6: the record for a.rs has no `DA` line records",
        ),
        (
            &[
                "SF:/repo/a.rs",
                "FNF:1",
                "FNH:2",
                "DA:1,1",
                "LF:1",
                "LH:1",
                "end_of_record",
            ][..],
            "LCOV line 7: the record for a.rs has `FNH` 2 above `FNF` 1",
        ),
        (
            &[
                "SF:/repo/a.rs",
                "FNF:0",
                "FNH:0",
                "DA:1,1",
                "LF:1",
                "LH:2",
                "end_of_record",
            ][..],
            "LCOV line 7: the record for a.rs has `LH` 2 above `LF` 1",
        ),
    ] {
        // Arrange
        let report = text(lines);

        // Act
        let (result, output) = checked(&report, &[]);

        // Assert
        assert_eq!(result, Err(expected.to_string()), "{report:?}");
        assert_eq!(output, "");
    }
}

#[test]
fn unusable_reports_fail_with_their_location() {
    for (report, expected) in [
        ("", "LCOV report has no source file records"),
        ("TN:\n\n", "LCOV report has no source file records"),
        (
            "DA:1,0\n",
            "LCOV line 1: `DA` appears outside a source file record",
        ),
        (
            "end_of_record\n",
            "LCOV line 1: `end_of_record` outside a source file record",
        ),
        (
            "SF:/repo/a.rs\nSF:/repo/b.rs\n",
            "LCOV line 2: the record for a.rs has no `end_of_record`",
        ),
        (
            "SF:/repo/a.rs\nDA:1,1\n",
            "LCOV report ends inside the record for a.rs",
        ),
        (
            "SF:/repo/a.rs\nDA:1\n",
            "LCOV line 2: expected two comma-separated fields",
        ),
        (
            "SF:/repo/a.rs\nDA:1,x\n",
            "LCOV line 2: invalid count `x`: invalid digit found in string",
        ),
        (
            "SF:/repo/a.rs\nFN:x,f\n",
            "LCOV line 2: invalid count `x`: invalid digit found in string",
        ),
        (
            "SF:/repo/a.rs\nFNDA:-1,f\n",
            "LCOV line 2: invalid count `-1`: invalid digit found in string",
        ),
        (
            "SF:/repo/a.rs\nFNDA:1,missing\n",
            "LCOV line 2: `FNDA` names a function without `FN`",
        ),
    ] {
        // Act
        let (result, output) = checked(report, &[]);

        // Assert
        assert_eq!(result, Err(expected.to_string()), "{report:?}");
        assert_eq!(output, "");
    }
}

#[test]
fn output_failures_fail_the_check() {
    // Arrange
    let report = covered_file("crates/a/src/lib.rs");

    // Act
    let result = check(&report, Path::new("/repo"), &[], &mut ClosedOutput);

    // Assert
    assert_eq!(
        result,
        Err("Failed to write the coverage report: closed".to_string())
    );
}
