use serde_json::json;

use super::support::{ByteEstimator, FixedEstimator, read_call, reasoned_read_call, tool_result};
use crate::context::{
    ELIDED_SOURCE_LIMIT_FACTOR, HistoryLimit, HistoryProjection, HistoryTurn, ReplayForm,
    ReplayedHistory, SentHistory, TurnStop, project_history,
};
use crate::model::ModelMessage;

/// A completed turn reading one file of `bytes` bytes with reasoning beside
/// its call, then answering.
fn read_turn(input: &str, bytes: usize) -> HistoryTurn {
    HistoryTurn::from(vec![
        ModelMessage::User(input.to_string()),
        ModelMessage::AssistantToolCall(reasoned_read_call("r1", Some(&"r".repeat(200)))),
        tool_result("r1", "read", &"x".repeat(bytes)),
        ModelMessage::Assistant(json!({ "answer": input }).to_string()),
    ])
}

fn retained(turn: &HistoryTurn) -> u64 {
    turn.messages
        .iter()
        .map(|message| u64::try_from(message.retained_bytes()).expect("bytes fit u64"))
        .sum()
}

/// Positions `turns` from zero, oldest first.
fn positioned(turns: &[HistoryTurn]) -> Vec<(i64, HistoryTurn)> {
    (0..).zip(turns.iter().cloned()).collect()
}

fn byte_limit(bytes: u64) -> HistoryLimit<'static> {
    HistoryLimit::bytes(usize::try_from(bytes).expect("bytes fit usize"))
}

#[test]
fn projection_boundaries_stay_ordered_and_give_each_turn_its_form() {
    // Arrange
    let projection = HistoryProjection::new(2, 1, 4);

    // Act
    let forms = (0..6)
        .map(|position| projection.form(position))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        (
            projection.start(),
            projection.brief_before(),
            projection.elided_before()
        ),
        (2, 2, 4)
    );
    assert_eq!(
        forms,
        [
            None,
            None,
            Some(ReplayForm::Elided),
            Some(ReplayForm::Elided),
            Some(ReplayForm::Full),
            Some(ReplayForm::Full),
        ]
    );
    assert_eq!(HistoryProjection::new(3, 0, 1).elided_before(), 3);
    assert_eq!(projection.drop_before(5), HistoryProjection::new(5, 5, 5));
    assert_eq!(projection.drop_before(1), projection);
    assert_eq!(projection.elide_before(5), HistoryProjection::new(2, 2, 5));
}

#[test]
fn turns_replay_in_their_projected_forms_while_they_fit_together() {
    // Arrange
    let turns = [
        read_turn("oldest", 400),
        read_turn("older", 400),
        read_turn("newest", 400),
    ];
    let projection = HistoryProjection::new(0, 1, 2);
    let expected = vec![
        (0, turns[0].brief()),
        (1, turns[1].elided()),
        (2, turns[2].clone()),
    ];
    let total = expected.iter().map(|(_, turn)| retained(turn)).sum::<u64>();

    // Act
    let (kept, kept_projection) =
        project_history(&positioned(&turns), projection, &byte_limit(total));

    // Assert
    assert_eq!(kept, expected);
    assert_eq!(kept_projection, projection);
}

#[test]
fn a_new_turn_only_appends_until_the_history_outgrows_its_limit() {
    // Arrange
    let turns = (0..12)
        .map(|turn| read_turn(&format!("turn {turn}"), 300))
        .collect::<Vec<_>>();
    let limit = byte_limit(retained(&turns[0]) * 4);
    let mut projection = HistoryProjection::default();
    let mut previous: Vec<(i64, HistoryTurn)> = Vec::new();
    let mut reprojections = 0;

    for count in 1..=turns.len() {
        // Act
        let (kept, next) = project_history(&positioned(&turns[..count]), projection, &limit);

        // Assert
        if next == projection {
            assert_eq!(kept[..previous.len()], previous[..], "turn {count}");
            assert_eq!(kept.len(), previous.len() + 1);
        } else {
            reprojections += 1;
            let total = kept.iter().map(|(_, turn)| retained(turn)).sum::<u64>();
            assert!(total <= retained(&turns[0]) * 2, "turn {count}");
            // Each turn keeps its form or a shorter one.
            for (position, turn) in &kept {
                assert!(next.form(*position) <= projection.form(*position));
                assert_ne!(next.form(*position), None, "{turn:?}");
            }
        }
        projection = next;
        previous = kept;
    }
    assert!((2..=4).contains(&reprojections), "{reprojections}");
}

#[test]
fn outgrown_history_is_reprojected_newest_first_to_half_its_limit() {
    // Arrange
    let turns = [
        read_turn("oldest", 2_000),
        read_turn("middle", 2_000),
        read_turn("newest", 2_000),
    ];
    let full = retained(&turns[2]);
    // Half the limit holds the newest turn whole and the next one elided.
    let limit = (full + retained(&turns[1].elided())) * 2;

    // Act
    let (kept, projection) = project_history(
        &positioned(&turns),
        HistoryProjection::default(),
        &byte_limit(limit),
    );

    // Assert
    assert_eq!(kept, [(1, turns[1].elided()), (2, turns[2].clone())]);
    assert_eq!(projection, HistoryProjection::new(1, 1, 2));
}

#[test]
fn reprojection_never_replays_a_turn_more_completely_than_before() {
    // Arrange
    let turns = [
        read_turn("brief", 600),
        read_turn("elided", 600),
        read_turn("full", 6_000),
    ];
    let projection = HistoryProjection::new(0, 1, 2);
    // The newest turn alone outgrows the limit; once it is shortened, half
    // the limit would hold the older turns whole.
    let limit = retained(&turns[2]) - 1;

    // Act
    let (kept, next) = project_history(&positioned(&turns), projection, &byte_limit(limit));

    // Assert
    assert_eq!(
        kept,
        [
            (0, turns[0].brief()),
            (1, turns[1].elided()),
            (2, turns[2].elided())
        ]
    );
    assert_eq!(next, HistoryProjection::new(0, 1, 3));
}

#[test]
fn reprojection_stops_at_the_first_turn_that_fits_in_no_form() {
    // Arrange
    let turns = [
        HistoryTurn::from(vec![ModelMessage::User("small".into())]),
        HistoryTurn::from(vec![
            ModelMessage::User("large".into()),
            ModelMessage::Assistant("large answer".into()),
        ]),
        HistoryTurn::from(vec![ModelMessage::User("recent".into())]),
    ];

    // Act
    // Each message weighs 7, so the turns weigh 7, 14, and 7 and only the
    // newest fits half of 18.
    let (partial, partial_projection) = project_history(
        &positioned(&turns),
        HistoryProjection::default(),
        &HistoryLimit::weight(18, &FixedEstimator),
    );
    let (empty, empty_projection) = project_history(
        &positioned(&turns),
        HistoryProjection::default(),
        &HistoryLimit::weight(0, &FixedEstimator),
    );

    // Assert
    // The older single-message turn would fit, but projection never skips
    // the two-message turn between it and the retained suffix.
    assert_eq!(partial, [(2, turns[2].clone())]);
    assert_eq!(partial_projection, HistoryProjection::new(2, 2, 2));
    assert_eq!(empty, []);
    assert_eq!(empty_projection, HistoryProjection::new(3, 3, 3));
}

#[test]
fn reprojection_keeps_only_the_input_and_note_of_a_stopped_turn_that_does_not_fit() {
    // Arrange
    let stopped = HistoryTurn {
        messages: vec![
            ModelMessage::User("stopped".to_string()),
            ModelMessage::AssistantToolCall(read_call("call")),
            tool_result("call", "read", "{}"),
        ],
        stop: Some(TurnStop {
            error_type: "cancelled".to_string(),
            ..TurnStop::default()
        }),
    };
    let turns = [
        HistoryTurn::from(vec![ModelMessage::User("older".to_string())]),
        stopped.clone(),
    ];

    // Act
    // Both turns weigh 35, over 20. The newest turn may fill the whole limit,
    // which holds only the brief form (14) of its full and elided stopped
    // replays (28); nothing older fits in what half of the limit leaves.
    let (kept, projection) = project_history(
        &positioned(&turns),
        HistoryProjection::default(),
        &HistoryLimit::weight(20, &FixedEstimator),
    );

    // Assert
    assert_eq!(kept, [(1, stopped.brief())]);
    assert_eq!(projection, HistoryProjection::new(1, 2, 2));
}

#[test]
fn the_weight_limit_weighs_each_turn_as_it_replays() {
    // Arrange
    let turn = read_turn("work", 2_000);
    let weigh = |turn: &HistoryTurn| {
        turn.replay()
            .iter()
            .map(|message| u64::try_from(message.retained_bytes()).expect("bytes fit u64"))
            .sum::<u64>()
    };
    let elided_weight = weigh(&turn.elided());

    // Act
    let (kept, _) = project_history(
        &positioned(std::slice::from_ref(&turn)),
        HistoryProjection::default(),
        &HistoryLimit::weight(elided_weight * 2, &ByteEstimator),
    );

    // Assert
    assert_eq!(kept, [(0, turn.elided())]);
}

#[test]
fn a_turn_beyond_the_load_bound_replays_brief_instead_of_elided() {
    // Arrange
    let turn = read_turn("work", 4_000);
    let max_bytes = usize::try_from(retained(&turn))
        .expect("bytes fit usize")
        .div_ceil(ELIDED_SOURCE_LIMIT_FACTOR)
        - 1;
    let projection = HistoryProjection::new(0, 0, 1);

    // Act
    let (kept, next) = project_history(
        &positioned(std::slice::from_ref(&turn)),
        projection,
        &HistoryLimit::bytes(max_bytes),
    );

    // Assert
    assert_eq!(kept, [(0, turn.brief())]);
    assert_eq!(next, projection);
}

#[test]
fn the_next_turn_keeps_what_this_turn_left_out_or_sent_elided() {
    // Arrange
    let turns = positioned(&[
        read_turn("dropped", 300),
        read_turn("elided", 300),
        read_turn("already elided", 300).elided(),
        read_turn("whole", 300),
    ]);
    let replayed = ReplayedHistory {
        projection: HistoryProjection::new(0, 0, 2),
        turns: turns.clone(),
    };
    let sent = SentHistory {
        dropped_turns: 1,
        turns: vec![
            turns[1].1.elided().replay(),
            turns[2].1.replay(),
            turns[3].1.replay(),
        ],
    };

    // Act
    let after = replayed.after_turn(&sent);
    let unreduced = replayed.after_turn(&SentHistory {
        dropped_turns: 0,
        turns: turns.iter().map(|(_, turn)| turn.replay()).collect(),
    });

    // Assert
    assert_eq!(after, HistoryProjection::new(1, 1, 3));
    assert_eq!(unreduced, replayed.projection);
}
