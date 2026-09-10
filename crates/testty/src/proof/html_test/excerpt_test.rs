use super::super::{build_html, highlight_limit};
use crate::assertion::{AssertionFailure, Expected};
use crate::frame::CellColor;
use crate::region::Region;
use crate::test_support::report_with_structured_failure;

#[test]
fn html_highlights_needle_occurrences_in_frame_excerpt() {
    // Arrange — `not visible` failure whose excerpt contains the
    // needle twice; both occurrences should be wrapped in
    // `needle-hit` spans so the report shows where the text was
    // found.
    let failure = AssertionFailure {
        message: "found two".to_string(),
        expected: Expected::NotVisible {
            needle: "Hello".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: "Hello world Hello again".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert
    assert_eq!(html.matches("class=\"needle-hit\">Hello").count(), 2);
}

#[test]
fn html_highlights_overlapping_needle_matches() {
    // Arrange — `TerminalFrame::find_text` reports `ana` twice in
    // `banana` because it advances one character past each match.
    // The HTML renderer must mirror that overlap semantics so the
    // excerpt does not silently drop the second match.
    let failure = AssertionFailure {
        message: "found two".to_string(),
        expected: Expected::NotVisible {
            needle: "ana".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: "banana".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — the two overlapping matches collapse into one span
    // covering the union of touched bytes, but every cell that any
    // match touched is highlighted (no second `ana` is dropped).
    assert!(
        html.contains("<span class=\"needle-hit\">anana</span>"),
        "expected merged overlapping highlight in:\n{html}"
    );
}

#[test]
fn html_column_ruler_uses_terminal_cell_width_for_wide_glyphs() {
    // Arrange — the excerpt contains a wide CJK glyph that occupies
    // two terminal cells, and the surrounding region is 5 cells
    // wide. The ruler must size itself by display width so labeled
    // columns line up with the underlying terminal cells: padding
    // the line to 5 display cells with `chars().count()` would
    // overshoot by one cell (2 chars + 3 spaces = 5 chars but 6
    // display cells), and sizing the ruler from `chars().count()`
    // would undershoot to two labels.
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(Region::new(0, 0, 5, 1)),
        matched_spans: Vec::new(),
        frame_excerpt: "中a".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — the ones-row ruler labels exactly 5 cells (`01234`)
    // because the line is padded to the region's 5 display columns
    // and the ruler is sized from the resulting display width, not
    // from the raw character count.
    assert!(
        html.contains("<span class=\"col-ruler\">     01234</span>"),
        "expected width-5 ones-row ruler in:\n{html}"
    );
}

#[test]
fn html_column_ruler_renders_hundreds_row_past_column_99() {
    // Arrange — region anchored at column 100 with width 21 so the
    // ruler must label cells 100..=120. Without a hundreds row,
    // column 120 would render as `2` (tens) over `0` (ones) and be
    // indistinguishable from absolute column 20.
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(Region::new(100, 0, 21, 1)),
        matched_spans: Vec::new(),
        frame_excerpt: "x".repeat(21),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — three ruler rows are emitted. The hundreds row marks
    // columns 100, 110, 120 with `1`; the tens row marks 100→`0`,
    // 110→`1`, 120→`2`; the ones row repeats `0..=9` and ends with
    // `0` at column 120. Stacking the three rows at column 120
    // recovers `120` instead of the ambiguous `20`.
    assert!(
        html.contains("<span class=\"col-ruler\">     1         1         1</span>"),
        "expected hundreds row marking cols 100/110/120 in:\n{html}"
    );
    assert!(
        html.contains("<span class=\"col-ruler\">     0         1         2</span>"),
        "expected tens row marking cols 100/110/120 in:\n{html}"
    );
    assert!(
        html.contains("<span class=\"col-ruler\">     012345678901234567890</span>"),
        "expected ones row spanning 21 cells in:\n{html}"
    );
}

#[test]
fn html_column_ruler_omits_hundreds_row_under_column_100() {
    // Arrange — narrow ruler entirely under column 100 must keep the
    // existing two-row layout so the hundreds row stays opt-in for
    // excerpts that actually need it.
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(Region::new(0, 0, 3, 1)),
        matched_spans: Vec::new(),
        frame_excerpt: "abc".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — exactly the two original ruler rows are emitted.
    assert_eq!(html.matches("class=\"col-ruler\"").count(), 2);
}

#[test]
fn html_renders_adjacent_needle_matches_as_separate_spans() {
    // Arrange — adjacent matches share an edge but do not overlap.
    // For `needle = "ab"` in `abab`, the matcher reports two
    // distinct hits at byte ranges 0..2 and 2..4. The renderer must
    // keep them as two separate `needle-hit` spans so the report
    // shows the matcher saw two occurrences instead of one long
    // run.
    let failure = AssertionFailure {
        message: "found two".to_string(),
        expected: Expected::NotVisible {
            needle: "ab".to_string(),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: "abab".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — two distinct spans, not one merged `abab` span.
    assert_eq!(
        html.matches("<span class=\"needle-hit\">ab</span>").count(),
        2,
        "expected two adjacent highlights in:\n{html}"
    );
    assert!(
        !html.contains("<span class=\"needle-hit\">abab</span>"),
        "adjacent matches must not collapse into one span in:\n{html}"
    );
}

#[test]
fn html_first_match_only_expectation_highlights_only_first_occurrence() {
    // Arrange — `ForegroundColor` validates only the first match of
    // `needle`. The excerpt may show the needle multiple times, but
    // highlighting every occurrence would falsely imply the matcher
    // checked them all. The renderer must emphasize only the first
    // hit across the whole excerpt.
    let failure = AssertionFailure {
        message: "wrong fg".to_string(),
        expected: Expected::ForegroundColor {
            needle: "Save".to_string(),
            color: CellColor::new(255, 0, 0),
        },
        region: None,
        matched_spans: Vec::new(),
        frame_excerpt: "Save first\nSave second".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — exactly one needle highlight even though the excerpt
    // contains two textual occurrences of `Save`.
    assert_eq!(
        html.matches("<span class=\"needle-hit\">Save</span>")
            .count(),
        1,
        "expected only the first occurrence to be highlighted in:\n{html}"
    );
}

#[test]
fn html_highlight_limit_classifies_expected_variants() {
    // Arrange / Act / Assert — first-match-only matchers cap at 1,
    // every-match matchers stay unbounded so the renderer cannot
    // silently suppress hits when new variants are added.
    assert_eq!(
        highlight_limit(&Expected::TextInRegion {
            needle: "x".to_string(),
        }),
        None
    );
    assert_eq!(
        highlight_limit(&Expected::NotVisible {
            needle: "x".to_string(),
        }),
        None
    );
    assert_eq!(
        highlight_limit(&Expected::MatchCount {
            needle: "x".to_string(),
            count: 2,
        }),
        None
    );
    assert_eq!(
        highlight_limit(&Expected::ForegroundColor {
            needle: "x".to_string(),
            color: CellColor::new(0, 0, 0),
        }),
        Some(1)
    );
    assert_eq!(
        highlight_limit(&Expected::BackgroundColor {
            needle: "x".to_string(),
            color: CellColor::new(0, 0, 0),
        }),
        Some(1)
    );
    assert_eq!(
        highlight_limit(&Expected::Highlighted {
            needle: "x".to_string(),
        }),
        Some(1)
    );
    assert_eq!(
        highlight_limit(&Expected::NotHighlighted {
            needle: "x".to_string(),
        }),
        Some(1)
    );
}

#[test]
fn html_renders_all_blank_region_with_full_dimensions() {
    // Arrange — region scoped to a 2-row × 8-col area where every
    // cell is blank. `text_in_region` collapses that to an empty
    // string by trimming trailing spaces and dropping trailing
    // empty lines, but the renderer must still surface the
    // region's geometry so location-sensitive debugging keeps
    // working when the failure is "the region is empty".
    let region = Region::new(5, 3, 8, 2);
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(region),
        matched_spans: Vec::new(),
        frame_excerpt: String::new(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — both region rows render with their absolute row
    // labels (3 and 4), the column ruler is anchored to the
    // region's start column (5), and the `<pre>` is not the empty
    // shell that the previous renderer emitted for blank excerpts.
    assert!(html.contains("class=\"row-gutter\">   3 "));
    assert!(html.contains("class=\"row-gutter\">   4 "));
    assert!(html.contains("class=\"col-ruler\""));
    assert!(!html.contains("<pre class=\"frame-excerpt\"></pre>"));
}

#[test]
fn html_pads_partially_blank_region_to_full_width() {
    // Arrange — region width is 12 cells but the first row only
    // contains `hi` before `text_in_region` trims trailing spaces.
    // The ruler must still span the full region width so column
    // labels reach the right edge of the region.
    let region = Region::new(0, 0, 12, 1);
    let failure = AssertionFailure {
        message: "missing".to_string(),
        expected: Expected::TextInRegion {
            needle: "Goodbye".to_string(),
        },
        region: Some(region),
        matched_spans: Vec::new(),
        frame_excerpt: "hi".to_string(),
    };
    let report = report_with_structured_failure(&failure);

    // Act
    let html = build_html(&report).expect("should build HTML");

    // Assert — the ones-row ruler labels all 12 columns (`0..=9`,
    // then `0`, `1`), proving the renderer padded the line out to
    // the region's display width before sizing the ruler.
    assert!(
        html.contains("<span class=\"col-ruler\">     012345678901</span>"),
        "expected width-12 ones-row ruler in:\n{html}"
    );
}
