use serde_json::{Value, json};

use crate::context::deferred_read_result;
use crate::read::output::{ReadError, ReadOutput, fit_tool_result};
use crate::tool::ReadArguments;

/// Builds a read output of `content` from `start_line` of a file with
/// `total_lines`, shown whole.
fn output(content: &str, start_line: u64, total_lines: Option<u64>) -> ReadOutput {
    let lines = u64::try_from(content.lines().count()).expect("line count");

    ReadOutput {
        comparison_base: None,
        content: content.to_string(),
        end_line: Some(start_line + lines - 1),
        line_bytes: None,
        next_offset: None,
        path: "src/lib.rs".to_string(),
        start_line,
        total_lines,
        truncated: false,
    }
}

/// Accepts results whose shown `content` or `result` holds at most `lines`
/// lines or items and at most `bytes` bytes of text or per item.
fn at_most(lines: usize, bytes: usize) -> impl Fn(&str) -> bool + Sync {
    move |result: &str| {
        let result: Value = serde_json::from_str(result).expect("result JSON");
        let shown = result.get("content").or_else(|| result.get("result"));
        match shown {
            Some(Value::String(text)) => text.lines().count() <= lines && text.len() <= bytes,
            Some(Value::Array(items)) => {
                items.len() <= lines
                    && items
                        .iter()
                        .all(|item| item.as_str().is_some_and(|item| item.len() <= bytes))
            }
            _ => false,
        }
    }
}

#[test]
fn pages_a_read_by_whole_lines_and_names_the_exact_continuation() {
    // Arrange
    let cases = [
        (
            output("a\nb\nc\nd", 1, Some(4)),
            "a\nb",
            "Showing lines 1-2 of 4. Use offset=3 limit=2 to continue.",
        ),
        (
            output("j\nk\nl", 10, None),
            "j\nk",
            "Showing lines 10-11. More lines follow. Use offset=12 limit=2 to continue.",
        ),
    ];

    for (output, content, continuation) in cases {
        // Act
        let (result, shown) = output
            .to_tool_result_within(&at_most(2, usize::MAX))
            .expect("two lines fit");

        // Assert
        let result: Value = serde_json::from_str(&result).expect("result JSON");
        assert_eq!(shown.content(), content);
        assert!(shown.truncated());
        assert_eq!(shown.next_offset(), Some(output.start_line + 2));
        assert_eq!(shown.total_lines(), output.total_lines);
        assert_eq!(result["continuation"], continuation);
        assert_eq!(result["content"], content);
    }
}

#[test]
fn keeps_a_read_that_fits_whole_without_a_continuation() {
    // Arrange
    let output = output("a\nb", 1, Some(2));

    // Act
    let (result, shown) = output
        .to_tool_result_within(&at_most(2, usize::MAX))
        .expect("whole read fits");

    // Assert
    assert_eq!(shown, output);
    assert_eq!(
        result,
        r#"{"path":"src/lib.rs","start_line":1,"end_line":2,"total_lines":2,"truncated":false,"content":"a\nb"}"#
    );
}

#[test]
fn shows_the_first_bytes_of_a_first_line_that_does_not_fit_whole() {
    // Arrange
    let output = output("abcdefgh\nij", 1, Some(2));

    // Act
    let (result, shown) = output
        .to_tool_result_within(&at_most(1, 3))
        .expect("three bytes fit");

    // Assert
    let result: Value = serde_json::from_str(&result).expect("result JSON");
    assert_eq!(shown.content(), "abc");
    assert_eq!(shown.end_line(), Some(1));
    assert_eq!(shown.next_offset(), Some(2));
    assert_eq!(result["line_bytes"], 8);
    assert_eq!(
        result["continuation"],
        "Showing only the first 3 of 8 bytes of line 1 of 2, which does not fit whole; read \
         cannot return the rest of that line. Use offset=2 to continue after it."
    );
}

#[test]
fn rejects_a_read_when_not_even_its_first_byte_fits() {
    // Arrange
    let output = output("abc", 7, None);

    // Act
    let error = output
        .to_tool_result_within(&|_| false)
        .expect_err("nothing fits");

    // Assert
    assert!(matches!(
        error,
        ReadError::LineTooLong { line: 7, path } if path == "src/lib.rs"
    ));
}

#[test]
fn refits_recorded_read_results_by_whole_lines_or_items() {
    // Arrange
    let page = output("a\nb\nc\nd\ne", 1, Some(5))
        .to_tool_result_within(&at_most(3, usize::MAX))
        .expect("page")
        .0;
    let partial = output("abcdefgh", 1, Some(1))
        .to_tool_result_within(&at_most(1, 6))
        .expect("partial line")
        .0;
    let search =
        json!({"action": "search", "result": ["a:1:x", "b:2:y", "c:3:z"], "truncated": false});
    let diff = json!({
        "action": "diff",
        "comparison_base": "abc123",
        "result": "l1\nl2\nl3\n",
        "truncated": false,
    });
    let arguments: ReadArguments =
        serde_json::from_value(json!({"path": "src/lib.rs"})).expect("read arguments");
    let cases = [
        (
            page,
            usize::MAX,
            Some(json!({
                "content": "a\nb",
                "continuation": "Showing lines 1-2 of 5. Use offset=3 limit=2 to continue.",
            })),
        ),
        (partial, 3, Some(json!({"content": "abc", "line_bytes": 8}))),
        (
            search.to_string(),
            usize::MAX,
            Some(json!({"result": ["a:1:x", "b:2:y"], "truncated": true})),
        ),
        (
            diff.to_string(),
            usize::MAX,
            Some(json!({"comparison_base": "abc123", "result": "l1\nl2\n", "truncated": true})),
        ),
        (
            json!({"error": "rejected", "path": "x", "status": "rejected"}).to_string(),
            usize::MAX,
            None,
        ),
        (deferred_read_result(&arguments), usize::MAX, None),
    ];

    for (content, bytes, expected) in cases {
        // Act
        let refitted = fit_tool_result(&content, &at_most(2, bytes));

        // Assert
        match expected {
            Some(Value::Object(fields)) => {
                let refitted: Value =
                    serde_json::from_str(&refitted.expect("refitted result")).expect("JSON");
                for (key, value) in fields {
                    assert_eq!(refitted[&key], value, "{key} of {content}");
                }
            }
            _ => assert_eq!(refitted, None, "{content}"),
        }
    }
}

#[test]
fn refitting_fails_when_no_form_fits() {
    // Arrange
    let search = json!({"action": "search", "result": ["a:1:x"], "truncated": false}).to_string();

    // Act
    let refitted = fit_tool_result(&search, &|_| false);

    // Assert
    assert_eq!(refitted, None);
}

#[test]
fn pages_reads_ending_in_an_empty_or_unterminated_line_exactly() {
    // Arrange
    let mut empty_last_line = output("a\n", 1, Some(2));
    empty_last_line.end_line = Some(2);
    let cases = [
        (
            empty_last_line,
            1,
            "a",
            "Showing lines 1-1 of 2. Use offset=2 limit=1 to continue.",
        ),
        (
            output("ab\ncd", 1, Some(2)),
            2,
            "ab",
            "Showing lines 1-1 of 2. Use offset=2 limit=1 to continue.",
        ),
    ];

    for (output, bytes, content, continuation) in cases {
        // Act
        let (result, shown) = output
            .to_tool_result_within(&at_most(usize::MAX, bytes))
            .expect("one line fits");

        // Assert
        let result: Value = serde_json::from_str(&result).expect("result JSON");
        assert_eq!(shown.content(), content);
        assert_eq!(shown.end_line(), Some(1));
        assert_eq!(shown.next_offset(), Some(2));
        assert_eq!(shown.total_lines(), Some(2));
        assert_eq!(result["continuation"], continuation);
    }
}

#[test]
fn a_first_line_cut_counts_the_metadata_until_not_one_byte_fits() {
    // Arrange
    // Long enough that the whole result outweighs any cut of it.
    let output = output(&format!("{}\nij", "abcdefgh".repeat(100)), 1, Some(2));
    let one_byte = output
        .to_tool_result_within(&at_most(1, 1))
        .expect("one byte fits")
        .0
        .len();

    // Act
    let (result, shown) = output
        .to_tool_result_within(&|result: &str| result.len() <= one_byte)
        .expect("exactly one byte fits beside the metadata");
    let error = output
        .to_tool_result_within(&|result: &str| result.len() < one_byte)
        .expect_err("the metadata alone leaves no room");

    // Assert
    assert_eq!(result.len(), one_byte);
    assert_eq!(shown.content(), "a");
    assert_eq!(shown.next_offset(), Some(2));
    assert_eq!(shown.total_lines(), Some(2));
    assert!(matches!(error, ReadError::LineTooLong { line: 1, .. }));
}

#[test]
fn inspection_results_keep_the_first_bytes_when_no_whole_line_or_item_fits() {
    // Arrange
    let diff = |result: &str| {
        json!({"action": "diff", "comparison_base": "abc123", "result": result, "truncated": false})
            .to_string()
    };
    let search =
        json!({"action": "search", "result": ["src/lib.rs:7:abcdefgh", "b"], "truncated": false})
            .to_string();
    let cases = [
        (diff("l1\nl2"), 1, usize::MAX, json!("l1\n")),
        (diff("abcdefgh\nij"), 1, 3, json!("abc")),
        (search, 1, 13, json!(["src/lib.rs:7:"])),
    ];

    for (content, lines, bytes, shown) in cases {
        // Act
        let refitted = fit_tool_result(&content, &at_most(lines, bytes));

        // Assert
        let refitted: Value =
            serde_json::from_str(&refitted.expect("refitted result")).expect("JSON");
        assert_eq!(refitted["result"], shown, "{content}");
        assert_eq!(refitted["truncated"], true, "{content}");
    }
    assert_eq!(fit_tool_result(&diff("abc"), &|_| false), None);
}
