//! Deterministic context-benchmark fixture and scorer checks.

use std::collections::{BTreeMap, BTreeSet};

use ag_harness::OutputSchema;
use serde_json::{Value, json};

use crate::fixture::{
    FixtureFile, HugeResultFixture, LARGE_CODE, PART_CODES, ReadLoopFixture, SessionReadsFixture,
    WriteFixture, answer_schema,
};

const DEFAULT_RESULT_LINES: usize = 200;
const MAX_READ_BYTES: usize = 50 * 1024;

fn public_functions(file: &FixtureFile) -> Vec<String> {
    file.contents
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub fn "))
        .filter_map(|rest| rest.split('(').next())
        .map(str::to_string)
        .collect()
}

fn read_loop_answer(fixture: &ReadLoopFixture) -> Value {
    let files = fixture
        .files()
        .iter()
        .map(|file| json!({ "path": file.path, "public_functions": public_functions(file) }))
        .collect::<Vec<_>>();
    let most = fixture
        .files()
        .iter()
        .max_by_key(|file| public_functions(file).len())
        .map(|file| file.path.clone());

    json!({ "files": files, "most_public_functions": most })
}

#[test]
fn read_loop_ground_truth_matches_an_independent_parse() {
    // Arrange
    let fixture = ReadLoopFixture::generate();
    let answer = read_loop_answer(&fixture);

    // Act
    let score = fixture.score(&answer);

    // Assert
    assert!(score.is_perfect(), "{:?}", score.failures);
    assert_eq!(score.total, fixture.files().len() + 1);
    assert!(OutputSchema::new(ReadLoopFixture::schema()).is_ok());
}

#[test]
fn read_loop_files_are_varied_and_fit_one_default_read() {
    // Arrange
    let fixture = ReadLoopFixture::generate();
    let prompt = fixture.prompt();

    // Act
    let mut counts = fixture
        .files()
        .iter()
        .map(|file| public_functions(file).len())
        .collect::<Vec<_>>();
    counts.sort_unstable();
    let distinct_lines = fixture
        .files()
        .iter()
        .flat_map(|file| file.contents.lines())
        .filter(|line| line.trim().len() > 8)
        .collect::<BTreeSet<_>>()
        .len();
    let total_lines = fixture
        .files()
        .iter()
        .flat_map(|file| file.contents.lines())
        .filter(|line| line.trim().len() > 8)
        .count();

    // Assert
    assert_eq!(fixture.files().len(), 16);
    assert!(counts[counts.len() - 1] > counts[counts.len() - 2]);
    assert!(
        fixture
            .files()
            .iter()
            .all(|file| file.contents.lines().count() <= DEFAULT_RESULT_LINES)
    );
    assert!(distinct_lines * 10 >= total_lines * 8);
    assert!(
        fixture
            .files()
            .iter()
            .all(|file| prompt.contains(&file.path))
    );
}

#[test]
fn read_loop_score_counts_each_wrong_file() {
    // Arrange
    let fixture = ReadLoopFixture::generate();
    let mut answer = read_loop_answer(&fixture);
    answer["files"][0]["public_functions"]
        .as_array_mut()
        .expect("public functions")
        .pop();
    answer["most_public_functions"] = json!("./src/missing.rs");

    // Act
    let score = fixture.score(&answer);

    // Assert
    assert_eq!(score.passed, score.total - 2);
    assert_eq!(score.failures.len(), 2);
    assert!(score.correctness() < 1.0);
}

/// Answers the session fixture's reading turns, two files each, then its
/// recall turn with every file, from an independent parse.
fn session_answers(fixture: &SessionReadsFixture) -> Vec<Value> {
    let entry = |file: &FixtureFile| json!({ "path": file.path, "public_functions": public_functions(file) });
    let largest = |files: &[FixtureFile]| {
        files
            .iter()
            .max_by_key(|file| public_functions(file).len())
            .map(|file| file.path.clone())
    };
    let mut answers = fixture
        .files()
        .chunks(2)
        .map(|files| {
            json!({
                "files": files.iter().map(entry).collect::<Vec<_>>(),
                "most_public_functions": largest(files),
            })
        })
        .collect::<Vec<_>>();
    answers.push(json!({
        "files": fixture.files().iter().map(entry).collect::<Vec<_>>(),
        "most_public_functions": largest(fixture.files()),
    }));

    answers
}

#[test]
fn session_reads_ground_truth_matches_an_independent_parse() {
    // Arrange
    let fixture = SessionReadsFixture::generate();
    let answers = session_answers(&fixture);

    // Act
    let score = fixture.score(&answers);

    // Assert
    assert!(score.is_perfect(), "{:?}", score.failures);
    assert_eq!(score.total, 8 * 3 + fixture.files().len() + 1);
    assert_eq!(fixture.prompts().len(), 9);
    assert!(OutputSchema::new(SessionReadsFixture::schema()).is_ok());
    for (prompt, files) in fixture.prompts().iter().zip(fixture.files().chunks(2)) {
        assert!(files.iter().all(|file| prompt.contains(&file.path)));
    }
}

#[test]
fn session_reads_history_outgrows_a_small_byte_budget_within_a_few_turns() {
    // Arrange
    let fixture = SessionReadsFixture::generate();

    // Act
    let per_turn = fixture
        .files()
        .chunks(2)
        .map(|files| files.iter().map(|file| file.contents.len()).sum::<usize>())
        .collect::<Vec<_>>();

    // Assert
    assert!(
        per_turn.iter().all(|bytes| (3_000..12_000).contains(bytes)),
        "{per_turn:?}"
    );
    assert!(
        per_turn.iter().sum::<usize>() > 2 * 24 * 1024,
        "{per_turn:?}"
    );
}

#[test]
fn session_reads_score_fails_missing_and_wrong_turns() {
    // Arrange
    let fixture = SessionReadsFixture::generate();
    let mut answers = session_answers(&fixture);
    // The first file of the first pair declares fewer public functions.
    answers[0]["most_public_functions"] = json!(fixture.files()[0].path);
    answers.truncate(7);

    // Act
    let score = fixture.score(&answers);

    // Assert
    let missing_turn = 3;
    let missing_recall = fixture.files().len() + 1;
    assert_eq!(score.failures.len(), 1 + missing_turn + missing_recall);
    assert_eq!(score.total - score.passed, score.failures.len());
}

#[test]
fn write_fixture_accepts_only_the_exact_diff() {
    // Arrange
    let fixture = WriteFixture::generate();
    let original = fixture
        .files()
        .iter()
        .map(|file| (file.path.clone(), file.contents.clone()))
        .collect::<BTreeMap<_, _>>();
    let expected = fixture
        .expected
        .iter()
        .map(|file| (file.path.clone(), file.contents.clone()))
        .collect::<BTreeMap<_, _>>();
    let status = " M src/courier.rs\n M src/ledger.rs\n";

    // Act
    let perfect = fixture.score(&expected, status);
    let unchanged = fixture.score(&original, "");
    let extra = fixture.score(&expected, &format!("{status}?? notes.txt\n"));

    // Assert
    assert!(perfect.is_perfect(), "{:?}", perfect.failures);
    assert_eq!((unchanged.passed, unchanged.total), (0, 4));
    assert_eq!((extra.passed, extra.total), (3, 4));
}

#[test]
fn write_fixture_edits_are_unambiguous_and_span_a_long_file() {
    // Arrange
    let fixture = WriteFixture::generate();
    let prompt = fixture.prompt();

    // Act
    let positions = fixture
        .edits
        .iter()
        .map(|edit| {
            let source = fixture
                .files()
                .iter()
                .find(|file| file.path == edit.path)
                .expect("edited file");
            let matches = source
                .contents
                .lines()
                .enumerate()
                .filter(|(_, line)| line.trim() == edit.old_line)
                .map(|(index, _)| index + 1)
                .collect::<Vec<_>>();
            (matches, prompt.contains(&edit.new_line))
        })
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(fixture.edits.len(), 3);
    assert!(
        positions
            .iter()
            .all(|(matches, named)| matches.len() == 1 && *named)
    );
    assert!(
        positions
            .iter()
            .any(|(matches, _)| matches[0] > DEFAULT_RESULT_LINES)
    );
}

#[test]
fn sequential_write_fixture_edits_each_long_file_once_in_order() {
    // Arrange
    let fixture = WriteFixture::sequential();
    let prompt = fixture.prompt();
    let expected = fixture
        .expected
        .iter()
        .map(|file| (file.path.clone(), file.contents.clone()))
        .collect::<BTreeMap<_, _>>();
    let status = fixture
        .files()
        .iter()
        .map(|file| format!(" M {}", file.path))
        .collect::<Vec<_>>()
        .join("\n");

    // Act
    let perfect = fixture.score(&expected, &status);
    let edited_paths = fixture
        .edits
        .iter()
        .map(|edit| edit.path.as_str())
        .collect::<Vec<_>>();
    let unambiguous = fixture.edits.iter().all(|edit| {
        let source = fixture
            .files()
            .iter()
            .find(|file| file.path == edit.path)
            .expect("edited file");
        let matches = source
            .contents
            .lines()
            .filter(|line| line.trim() == edit.old_line)
            .count();

        matches == 1 && prompt.contains(&edit.new_line)
    });

    // Assert
    assert!(perfect.is_perfect(), "{:?}", perfect.failures);
    assert!(unambiguous);
    assert_eq!(
        edited_paths,
        fixture
            .files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(fixture.files().len(), 8);
    // Together the files outgrow the default 15,616-unit capacity even at
    // four and a half bytes per unit, so later writes supersede earlier
    // reads.
    let bytes: usize = fixture.files().iter().map(|file| file.contents.len()).sum();
    assert!(bytes * 2 > 9 * 15_616, "{bytes} bytes");
    assert!(prompt.contains("one file at a time"));
}

#[test]
fn huge_result_fixture_names_every_code_once() {
    // Arrange
    let fixture = HugeResultFixture::generate();
    let complete = json!({ "answer": "AMBER-17, basalt-29, cobalt-31, dune-43, ember-59, fjord-61, ivory-97" });
    let partial = json!({ "answer": PART_CODES.join(", ") });

    // Act
    let complete = HugeResultFixture::score(&complete);
    let partial = HugeResultFixture::score(&partial);

    // Assert
    let large = fixture.files().last().expect("large file");
    assert!(complete.is_perfect(), "{:?}", complete.failures);
    assert_eq!((partial.passed, partial.total), (6, 7));
    assert_eq!(large.contents.trim_end().lines().count(), 1);
    assert!(large.contents.len() < MAX_READ_BYTES);
    assert!(
        large
            .contents
            .starts_with(&format!("The code word of the large file is {LARGE_CODE}."))
    );
    assert!(
        fixture
            .files()
            .iter()
            .all(|file| file.contents.matches("code word").count() == 1)
    );
    assert!(HugeResultFixture::prompt().contains("notes/part6.txt"));
    assert!(OutputSchema::new(answer_schema()).is_ok());
}

#[test]
fn fixtures_are_deterministic() {
    // Arrange
    let contents = |files: &[FixtureFile]| {
        files
            .iter()
            .map(|file| (file.path.clone(), file.contents.clone()))
            .collect::<Vec<_>>()
    };

    // Act
    let first = (
        contents(ReadLoopFixture::generate().files()),
        contents(WriteFixture::generate().files()),
        contents(WriteFixture::sequential().files()),
        contents(HugeResultFixture::generate().files()),
    );
    let second = (
        contents(ReadLoopFixture::generate().files()),
        contents(WriteFixture::generate().files()),
        contents(WriteFixture::sequential().files()),
        contents(HugeResultFixture::generate().files()),
    );

    // Assert
    assert_eq!(first, second);
}
