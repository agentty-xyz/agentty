//! Tests of what a session tells a platform's Sync.

use super::{
    FALLBACK_SUMMARY, finish_summary, is_sync_repository, last_active_text, milestone_line,
    session_label, sync_repository, work_brief, work_title,
};

#[test]
fn the_title_is_the_first_line_with_text_cut_to_two_hundred_characters() {
    // Arrange
    let long_line = "x".repeat(250);
    let cases = [
        (
            "Fix the relay reconnect",
            Some("Fix the relay reconnect".to_string()),
        ),
        (
            "\n  \n  Fix the relay reconnect  \n\nIt drops after a suspend.",
            Some("Fix the relay reconnect".to_string()),
        ),
        (long_line.as_str(), Some("x".repeat(200))),
        ("   \n\t\n", None),
        ("", None),
    ];

    for (message, expected) in cases {
        // Act
        let title = work_title(message);

        // Assert
        assert_eq!(title, expected, "message {message:?}");
    }
}

#[test]
fn the_title_does_not_end_in_whitespace_left_by_the_cut() {
    // Arrange
    let message = format!("{} tail", "y".repeat(199));

    // Act
    let title = work_title(&message);

    // Assert
    assert_eq!(title, Some("y".repeat(199)));
}

#[test]
fn the_brief_is_the_message_cut_to_four_thousand_characters() {
    // Arrange
    let long_message = "é".repeat(4100);

    // Act
    let short = work_brief("\n Fix it\n\nNow. \n");
    let long = work_brief(&long_message);

    // Assert
    assert_eq!(short, "Fix it\n\nNow.");
    assert_eq!(long.chars().count(), 4000);
}

#[test]
fn the_session_label_is_the_instance_name_and_the_start_of_the_identifier() {
    // Arrange
    let long_name = "n".repeat(120);
    let cases = [
        (
            "Agentty on studio",
            "3f9c2a7e-0000-4000-8000-000000000000",
            "Agentty on studio 3f9c2a".to_string(),
        ),
        ("  ", "3f9c2a7e", "3f9c2a".to_string()),
        (long_name.as_str(), "abc", format!("{} abc", "n".repeat(93))),
    ];

    for (instance_name, session_id, expected) in cases {
        // Act
        let label = session_label(instance_name, session_id);

        // Assert
        assert_eq!(label, expected);
        assert!(label.chars().count() <= 100);
    }
}

#[test]
fn the_repository_is_a_credential_free_host_and_path_that_fits_the_contract() {
    // Arrange: the remote, then the repository a request names.
    let long_path = format!("https://example.com/{}", "p".repeat(200));
    let cases = [
        (
            "https://user:token@github.com/agentty-xyz/agentty.git",
            Some("github.com/agentty-xyz/agentty"),
        ),
        (
            "git@github.com:agentty-xyz/agentty.git",
            Some("github.com/agentty-xyz/agentty"),
        ),
        (
            "ssh://git@gitlab.example.com:2222/group/sub/project.git",
            Some("gitlab.example.com/group/sub/project"),
        ),
        ("https://example.com/caf\u{e9}/project", None),
        ("https://example.com/team/project%20x", None),
        ("/home/user/project", None),
        (long_path.as_str(), None),
    ];

    for (remote, expected) in cases {
        // Act
        let repository = sync_repository(remote);

        // Assert
        assert_eq!(repository.as_deref(), expected, "remote {remote:?}");
    }
}

#[test]
fn only_a_host_and_a_path_without_empty_segments_fits_the_contract() {
    // Arrange
    let cases = [
        ("a/b", true),
        ("host:8443/team/project", true),
        ("ab", false),
        ("/team/project", false),
        ("host", false),
        ("host/", false),
        ("host/team//project", false),
        ("host/team project", false),
    ];

    for (repository, expected) in cases {
        // Act
        let fits = is_sync_repository(repository);

        // Assert
        assert_eq!(fits, expected, "repository {repository:?}");
    }
}

#[test]
fn the_summary_is_the_session_title_or_a_fixed_fallback() {
    // Arrange
    let long_title = "t".repeat(2100);

    // Act
    let titled = finish_summary(Some("  Fix the relay reconnect "));
    let long = finish_summary(Some(&long_title));
    let blank = finish_summary(Some("   "));
    let untitled = finish_summary(None);

    // Assert
    assert_eq!(titled, "Fix the relay reconnect");
    assert_eq!(long.chars().count(), 2000);
    assert_eq!(blank, FALLBACK_SUMMARY);
    assert_eq!(untitled, FALLBACK_SUMMARY);
}

#[test]
fn last_activity_reads_in_the_largest_whole_unit() {
    // Arrange: milliseconds since the activity, then the text.
    let minute = 60_000;
    let cases = [
        (0, "just now"),
        (59_999, "just now"),
        (minute, "1 minute ago"),
        (59 * minute, "59 minutes ago"),
        (60 * minute, "1 hour ago"),
        (23 * 60 * minute, "23 hours ago"),
        (24 * 60 * minute, "1 day ago"),
        (72 * 60 * minute, "3 days ago"),
    ];
    let now = 1_767_261_600_000;

    for (elapsed, expected) in cases {
        // Act
        let text = last_active_text(now, now - elapsed);

        // Assert
        assert_eq!(text, expected);
    }
    assert_eq!(last_active_text(now, now + minute), "just now");
}

#[test]
fn a_milestone_reads_as_one_notice_line() {
    // Arrange
    let (number, title) = (3, "Ship milestones");

    // Act
    let line = milestone_line(number, title);

    // Assert
    assert_eq!(line, "Milestone M3: Ship milestones");
}
