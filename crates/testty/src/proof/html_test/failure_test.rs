use super::super::build_html;
use crate::assertion::{AssertionFailure, Expected};
use crate::frame::{CellColor, CellStyle, TerminalFrame};
use crate::locator::MatchedSpan;
use crate::proof::report::ProofReport;
use crate::region::Region;
use crate::test_support::report_with_structured_failure;

#[test]
fn html_renders_structured_failure_expected_and_region() {
    // Arrange — failure whose region was scoped to row 0 columns 0..20.
    let region = Region::new(0, 0, 20, 1);
    let failure = AssertionFailure {
        message: "text 'Goodbye' not found in region (col=0, row=0, 20x1)".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(region),
        matched_spans: Vec::new(),
        frame_excerpt: "Hello World         ".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — context column shows the Expected description and the
    // structured Region coordinates, and the failure-detail wrapper
    // exists.
    assert!(html.contains("class=\"failure-detail\""));
    assert!(html.contains("Expected:"));
    assert!(html.contains("text 'Goodbye' visible in region"));
    assert!(html.contains("Region:"));
    assert!(html.contains("col=0 row=0 width=20 height=1"));
}

#[test]
fn html_renders_matched_spans_when_present() {
    // Arrange — `match_not_visible` style failure with two recorded
    // matches the renderer should list under the spans heading.
    let span_a = MatchedSpan {
        text: "Hello".to_string(),
        rect: Region::new(0, 0, 5, 1),
        foreground: None,
        background: None,
        style: CellStyle::default(),
    };
    let span_b = MatchedSpan {
        text: "Hello".to_string(),
        rect: Region::new(6, 1, 5, 1),
        foreground: None,
        background: None,
        style: CellStyle::default(),
    };
    let failure = AssertionFailure {
        message: "Expected text 'Hello' to NOT be visible".to_string(),
        expected: Expected::NotVisible {
            needle: "Hello".to_string(),
        },
        region: None,
        matched_spans: vec![span_a, span_b],
        frame_excerpt: "Hello World".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — both span entries render under the spans list, and
    // the section title carries the count.
    assert!(html.contains("Matched spans (2)"));
    assert!(html.contains("'Hello' at col=0 row=0 width=5"));
    assert!(html.contains("'Hello' at col=6 row=1 width=5"));
}

#[test]
fn html_frame_excerpt_uses_region_offsets_for_row_gutter() {
    // Arrange — region scoped to row 5 starting at column 10 so the
    // gutter labels must reflect those offsets, not the local
    // excerpt origin.
    let region = Region::new(10, 5, 11, 2);
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(region),
        matched_spans: Vec::new(),
        frame_excerpt: "first line \nsecond line".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — first frame row uses the region's row index (5), the
    // second uses 6, and the column ruler is anchored to col 10.
    assert!(html.contains("class=\"row-gutter\">   5 "));
    assert!(html.contains("class=\"row-gutter\">   6 "));
    assert!(html.contains("class=\"col-ruler\""));
}

#[test]
fn html_legacy_assertions_have_no_failure_detail() {
    // Arrange — legacy entry pushed without a structured failure
    // should not produce the structured detail block.
    let frame = TerminalFrame::new(20, 3, b"Test");
    let mut report = ProofReport::new("legacy_no_detail");
    report.add_capture("check", "Check", &frame);
    report.add_assertion("check", false, "color match");

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — legacy `add_assertion` carries no structured failure
    // so the failure-detail wrapper must not appear.
    assert!(html.contains("class=\"assertion fail\""));
    assert!(!html.contains("class=\"failure-detail\""));
}

#[test]
fn html_renders_match_count_expectation() {
    // Arrange — `MatchCount` describes a count expectation that the
    // renderer should surface in the Expected line.
    let failure = AssertionFailure {
        message: "wrong count".to_string(),
        expected: Expected::MatchCount {
            needle: "TODO".to_string(),
            count: 3,
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(html.contains("text 'TODO' to appear exactly 3 time(s)"));
}

#[test]
fn html_renders_color_expectation_with_rgb() {
    // Arrange — color expectations should render the RGB triple so
    // the failure column links the structured `CellColor` to the
    // human-readable description.
    let failure = AssertionFailure {
        message: "wrong color".to_string(),
        expected: Expected::ForegroundColor {
            needle: "Save".to_string(),
            color: CellColor::new(255, 0, 0),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — wording attributes the expectation to the first
    // match so readers do not assume every occurrence of the needle
    // was checked.
    assert!(html.contains("first match of 'Save' with foreground color rgb(255, 0, 0)"));
}

#[test]
fn html_format_span_includes_actual_fg_bg_and_style() {
    // Arrange — color failure whose first matched span carries the
    // actual foreground/background and `bold` style. The detail
    // block must surface those actual attributes alongside the
    // structured expectation so readers can compare expected vs.
    // actual without re-reading the assertion-line summary.
    let foreground = CellColor::new(10, 20, 30);
    let background = CellColor::new(40, 50, 60);
    let span = MatchedSpan {
        text: "Save".to_string(),
        rect: Region::new(2, 1, 4, 1),
        foreground: Some(foreground),
        background: Some(background),
        style: CellStyle::from_raw(0b0000_0001),
    };
    let failure = AssertionFailure {
        message: "Text 'Save' at (2, 1) has foreground Some(...), expected ...".to_string(),
        expected: Expected::ForegroundColor {
            needle: "Save".to_string(),
            color: CellColor::new(255, 0, 0),
        },
        region: None,
        matched_spans: vec![span],
        frame_excerpt: "Save".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert!(
        html.contains(
            "'Save' at col=2 row=1 width=4 fg=rgb(10, 20, 30) bg=rgb(40, 50, 60) style=bold"
        ),
        "expected matched-span to include actual fg/bg/style in:\n{html}"
    );
}

#[test]
fn html_format_span_omits_unset_color_and_style_fields() {
    // Arrange — span without color or style flags should render the
    // base coordinates only, with no trailing fg/bg/style fragment.
    let span = MatchedSpan {
        text: "Save".to_string(),
        rect: Region::new(0, 0, 4, 1),
        foreground: None,
        background: None,
        style: CellStyle::default(),
    };
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::NotVisible {
            needle: "Save".to_string(),
        },
        region: None,
        matched_spans: vec![span],
        frame_excerpt: "Save".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — list entry stops after `width=` and never contains an
    // empty `fg=`, `bg=`, or `style=` fragment.
    assert!(html.contains("'Save' at col=0 row=0 width=4</li>"));
    assert!(!html.contains("fg="));
    assert!(!html.contains("bg="));
    assert!(!html.contains("style="));
}
