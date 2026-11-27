//! Tests of the platform's Sync routes against a fake platform server.

use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::infra::platform::{
    AccessToken, ClaimId, HttpPlatformClient, PlatformClient, PlatformError, SyncError, SyncHolder,
    SyncStart, SyncTask, SyncTaskRef, SyncWork, SyncWorkInProgress,
};

/// Token the fake platform accepts.
const TOKEN: &str = "agp_test-token";
/// Claim the fake platform issues.
const CLAIM: &str = "clm_secret";

/// Returns the test token.
fn token() -> AccessToken {
    AccessToken::new(TOKEN.to_string())
}

/// Returns the work of a first message.
fn work() -> SyncWork {
    SyncWork {
        brief: "Fix the relay reconnect\n\nIt drops after a suspend.".to_string(),
        not_duplicates: Vec::new(),
        repository: "github.com/agentty-xyz/agentty".to_string(),
        session: "Agentty on studio 3f9c2a".to_string(),
        takeover: None,
        title: "Fix the relay reconnect".to_string(),
    }
}

/// Returns the reference of task 12 of `agentty`.
fn task_ref() -> SyncTaskRef {
    SyncTaskRef {
        number: 12,
        project: "agentty".to_string(),
    }
}

/// Returns the task object of answers, with its page on `uri`.
fn task_json(uri: &str) -> Value {
    json!({
        "project": "agentty",
        "number": 12,
        "title": "Fix the relay\u{7} reconnect",
        "url": format!("{uri}/sync/projects/agentty/tasks/12"),
    })
}

/// Starts a fake platform answering `POST route` with `response`, and a
/// client for it.
async fn fake_sync(route: &str, response: ResponseTemplate) -> (MockServer, HttpPlatformClient) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(response)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client should build");

    (server, client)
}

#[tokio::test]
async fn a_start_sends_only_the_work_and_keeps_the_cleaned_answer() {
    // Arrange
    let server = MockServer::start().await;
    let uri = server.uri();
    let mut in_progress = vec![
        json!({ "number": 9, "title": "Speed up\u{1b} the diff view", "session": "laptop 81ac04", "last_activity_at_ms": 1_767_261_600_000_u64 }),
        json!({ "number": 10, "title": "Unlabelled work", "session": null, "last_activity_at_ms": 5 }),
        json!({ "number": 11, "title": "\u{1b}", "session": "x", "last_activity_at_ms": 5 }),
        json!({ "number": "twelve", "title": "Unreadable", "last_activity_at_ms": 5 }),
    ];
    for number in 20..40 {
        in_progress.push(json!({
            "number": number, "title": "More", "session": "s", "last_activity_at_ms": 1,
        }));
    }
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({
            "repository": "github.com/agentty-xyz/agentty",
            "title": "Fix the relay reconnect",
            "brief": "Fix the relay reconnect\n\nIt drops after a suspend.",
            "session": "Agentty on studio 3f9c2a",
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "task": task_json(&uri),
            "claim_id": CLAIM,
            "created": true,
            "in_progress": in_progress,
            "extra": "ignored",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&uri).expect("client");

    // Act
    let start = client
        .start_sync_work(&token(), &work())
        .await
        .expect("start should succeed");

    // Assert
    assert_eq!(start.claim_id, ClaimId::new(CLAIM.to_string()));
    assert!(start.created);
    assert_eq!(
        start.task,
        SyncTask {
            number: 12,
            project: "agentty".to_string(),
            title: "Fix the relay reconnect".to_string(),
            url: Some(format!("{uri}/sync/projects/agentty/tasks/12")),
        }
    );
    assert_eq!(start.in_progress.len(), 20);
    assert_eq!(
        start.in_progress[..2],
        [
            SyncWorkInProgress {
                last_activity_at_ms: 1_767_261_600_000,
                number: 9,
                session: Some("laptop 81ac04".to_string()),
                title: "Speed up the diff view".to_string(),
            },
            SyncWorkInProgress {
                last_activity_at_ms: 5,
                number: 10,
                session: None,
                title: "Unlabelled work".to_string(),
            },
        ]
    );
    assert!(!format!("{start:?}").contains(CLAIM));
}

#[tokio::test]
async fn a_repeated_start_names_the_task_it_takes_over_and_the_work_it_is_not() {
    // Arrange
    let server = MockServer::start().await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/work"))
        .and(body_json(json!({
            "repository": "github.com/agentty-xyz/agentty",
            "title": "Fix the relay reconnect",
            "brief": "Fix the relay reconnect\n\nIt drops after a suspend.",
            "session": "Agentty on studio 3f9c2a",
            "takeover": 12,
            "not_duplicates": [9, 15],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task": task_json(&uri),
            "claim_id": CLAIM,
            "in_progress": "not a list",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&uri).expect("client");
    let work = SyncWork {
        not_duplicates: vec![9, 15],
        takeover: Some(12),
        ..work()
    };

    // Act
    let start = client.start_sync_work(&token(), &work).await;

    // Assert
    let start = start.expect("start should succeed");
    assert!(!start.created);
    assert_eq!(start.in_progress, Vec::new());
}

#[tokio::test]
async fn a_task_page_is_kept_only_when_it_is_one_of_the_platform_pages() {
    // Arrange
    let server = MockServer::start().await;
    let uri = server.uri();
    let pages = [
        json!("https://evil.example/sync/projects/agentty/tasks/12"),
        json!(format!("{uri}.evil.example/tasks/12")),
        json!(format!("{uri}/tasks/12\u{1b}")),
        json!(format!("{uri}/{}", "a".repeat(2100))),
        json!(null),
    ];

    for page in pages {
        server.reset().await;
        let mut task = task_json(&uri);
        task["url"] = page;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync/work"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "task": task, "claim_id": CLAIM })),
            )
            .mount(&server)
            .await;
        let client = HttpPlatformClient::new(&uri).expect("client");

        // Act
        let start = client.start_sync_work(&token(), &work()).await;

        // Assert
        assert_eq!(start.expect("start should succeed").task.url, None);
    }
}

#[tokio::test]
async fn a_start_refuses_answers_it_cannot_trust() {
    // Arrange
    let uri = "http://127.0.0.1:1";
    let mut bad_project = task_json(uri);
    bad_project["project"] = json!("../admin");
    let mut no_number = task_json(uri);
    no_number["number"] = json!(0);
    let mut blank_title = task_json(uri);
    blank_title["title"] = json!(" \u{1b} ");
    let answers = [
        json!({ "task": bad_project, "claim_id": CLAIM }),
        json!({ "task": no_number, "claim_id": CLAIM }),
        json!({ "task": blank_title, "claim_id": CLAIM }),
        json!({ "task": task_json(uri), "claim_id": "" }),
        json!({ "task": task_json(uri), "claim_id": "clm_\n" }),
        json!({ "task": task_json(uri), "claim_id": "c".repeat(257) }),
        json!({ "claim_id": CLAIM }),
    ];

    for answer in answers {
        let (_server, client) = fake_sync(
            "/api/v1/sync/work",
            ResponseTemplate::new(200).set_body_json(answer),
        )
        .await;

        // Act
        let start = client.start_sync_work(&token(), &work()).await;

        // Assert
        assert_eq!(
            start,
            Err(SyncError::Failed(PlatformError::InvalidResponse))
        );
    }
}

#[tokio::test]
async fn a_start_outside_the_contract_is_never_sent() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");
    let works = [
        SyncWork {
            repository: "github.com/caf\u{e9}/agentty".to_string(),
            ..work()
        },
        SyncWork {
            repository: "github.com".to_string(),
            ..work()
        },
        SyncWork {
            title: String::new(),
            ..work()
        },
        SyncWork {
            title: "t".repeat(201),
            ..work()
        },
        SyncWork {
            brief: "b".repeat(4001),
            ..work()
        },
        SyncWork {
            session: "s".repeat(101),
            ..work()
        },
        SyncWork {
            session: String::new(),
            ..work()
        },
    ];

    for work in works {
        // Act
        let start = client.start_sync_work(&token(), &work).await;

        // Assert
        assert_eq!(start, Err(SyncError::Failed(PlatformError::InvalidRequest)));
    }
    server.verify().await;
}

#[tokio::test]
async fn a_conflict_names_the_task_and_its_holder() {
    // Arrange
    let server = MockServer::start().await;
    let uri = server.uri();
    let holders = [
        (
            json!({ "session": "laptop\u{7} 81ac04", "claimed_at_ms": 1, "last_activity_at_ms": 2 }),
            Some("laptop 81ac04".to_string()),
        ),
        (
            json!({ "session": null, "claimed_at_ms": 1, "last_activity_at_ms": 2 }),
            None,
        ),
    ];

    for (holder, expected_session) in holders {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync/work"))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({
                "error": "task_in_progress",
                "message": "Another session is working on task #12",
                "task": task_json(&uri),
                "holder": holder,
            })))
            .mount(&server)
            .await;
        let client = HttpPlatformClient::new(&uri).expect("client");

        // Act
        let start = client.start_sync_work(&token(), &work()).await;

        // Assert
        assert_eq!(
            start,
            Err(SyncError::TaskInProgress {
                holder: SyncHolder {
                    last_activity_at_ms: 2,
                    session: expected_session,
                },
                task: SyncTask {
                    number: 12,
                    project: "agentty".to_string(),
                    title: "Fix the relay reconnect".to_string(),
                    url: Some(format!("{uri}/sync/projects/agentty/tasks/12")),
                },
            })
        );
    }
}

#[tokio::test]
async fn sync_failures_map_to_what_the_session_does_next() {
    // Arrange: the status, the body, then the error.
    let incomplete_conflict = json!({ "error": "task_in_progress", "task": task_json("x") });
    let mut bad_task = task_json("x");
    bad_task["project"] = json!("a/b");
    let untrusted_conflict = json!({
        "error": "task_in_progress",
        "task": bad_task,
        "holder": { "session": "s", "claimed_at_ms": 1, "last_activity_at_ms": 2 },
    });
    let cases = [
        (409, json!({ "error": "claim_lost" }), SyncError::ClaimLost),
        (
            409,
            json!({ "error": "task_closed" }),
            SyncError::TaskClosed,
        ),
        (
            403,
            json!({ "error": "sync_disabled" }),
            SyncError::SyncDisabled,
        ),
        (
            403,
            json!({ "error": "waitlisted" }),
            SyncError::Failed(PlatformError::Waitlisted),
        ),
        (
            401,
            json!({ "error": "invalid_token" }),
            SyncError::Failed(PlatformError::InvalidToken),
        ),
        (
            400,
            json!({ "error": "invalid_request", "message": "Bad body." }),
            SyncError::Failed(PlatformError::Rejected {
                message: Some("Bad body.".to_string()),
                status: 400,
            }),
        ),
        (
            503,
            json!({ "error": "sync_unavailable" }),
            SyncError::Failed(PlatformError::Rejected {
                message: None,
                status: 503,
            }),
        ),
        (
            409,
            incomplete_conflict,
            SyncError::Failed(PlatformError::InvalidResponse),
        ),
        (
            409,
            untrusted_conflict,
            SyncError::Failed(PlatformError::InvalidResponse),
        ),
    ];

    for (status, body, expected) in cases {
        let (_server, client) = fake_sync(
            "/api/v1/sync/work",
            ResponseTemplate::new(status).set_body_json(body),
        )
        .await;

        // Act
        let start = client.start_sync_work(&token(), &work()).await;

        // Assert
        assert_eq!(start, Err(expected));
    }
}

#[tokio::test]
async fn activity_finish_and_release_send_the_claim_to_the_task_route() {
    // Arrange
    let server = MockServer::start().await;
    let uri = server.uri();
    let routes = [
        (
            "activity",
            json!({ "claim_id": CLAIM }),
            ResponseTemplate::new(204),
        ),
        (
            "finish",
            json!({
                "claim_id": CLAIM,
                "summary": "Fix the relay reconnect",
                "result_url": "https://github.com/agentty-xyz/agentty/pull/7",
            }),
            ResponseTemplate::new(200).set_body_json(json!({ "task": task_json(&uri) })),
        ),
        (
            "release",
            json!({ "claim_id": CLAIM }),
            ResponseTemplate::new(200),
        ),
    ];
    for (route, body, response) in routes {
        Mock::given(method("POST"))
            .and(path(format!(
                "/api/v1/sync/projects/agentty/tasks/12/{route}"
            )))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .and(body_json(body))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = HttpPlatformClient::new(&uri).expect("client");
    let claim = ClaimId::new(CLAIM.to_string());

    // Act
    let activity = client
        .report_sync_activity(&token(), &task_ref(), &claim)
        .await;
    let finish = client
        .finish_sync_task(
            &token(),
            &task_ref(),
            &claim,
            "Fix the relay reconnect",
            Some("https://github.com/agentty-xyz/agentty/pull/7".to_string()),
        )
        .await;
    let release = client
        .release_sync_task(&token(), &task_ref(), &claim)
        .await;

    // Assert
    assert_eq!(activity, Ok(()));
    assert_eq!(finish, Ok(()));
    assert_eq!(release, Ok(()));
    server.verify().await;
}

#[tokio::test]
async fn a_finish_without_a_review_request_sends_only_the_summary() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync/projects/agentty/tasks/12/finish"))
        .and(body_json(json!({ "claim_id": CLAIM, "summary": "Done." })))
        .respond_with(ResponseTemplate::new(409).set_body_json(json!({ "error": "task_closed" })))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");

    // Act
    let finish = client
        .finish_sync_task(
            &token(),
            &task_ref(),
            &ClaimId::new(CLAIM.to_string()),
            "Done.",
            None,
        )
        .await;

    // Assert
    assert_eq!(finish, Err(SyncError::TaskClosed));
}

#[tokio::test]
async fn task_requests_outside_the_contract_are_never_sent() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");
    let claim = ClaimId::new(CLAIM.to_string());
    let unsafe_projects = ["", ".", "..", "a/b", "a?b", &"p".repeat(101)];

    // Act, Assert
    for project in unsafe_projects {
        let task = SyncTaskRef {
            number: 12,
            project: project.to_string(),
        };
        let activity = client.report_sync_activity(&token(), &task, &claim).await;
        let release = client.release_sync_task(&token(), &task, &claim).await;
        let finish = client
            .finish_sync_task(&token(), &task, &claim, "Done.", None)
            .await;
        for result in [activity, release, finish] {
            assert_eq!(
                result,
                Err(SyncError::Failed(PlatformError::InvalidRequest))
            );
        }
    }
    for summary in [String::new(), "s".repeat(2001)] {
        let finish = client
            .finish_sync_task(&token(), &task_ref(), &claim, &summary, None)
            .await;
        assert_eq!(
            finish,
            Err(SyncError::Failed(PlatformError::InvalidRequest))
        );
    }
    server.verify().await;
}

#[tokio::test]
async fn an_unreachable_platform_fails_a_sync_request_plainly() {
    // Arrange: nothing listens on the platform address.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let address = unused.local_addr().expect("address");
    drop(unused);
    let client = HttpPlatformClient::new(&format!("http://{address}")).expect("client");

    // Act
    let start = client.start_sync_work(&token(), &work()).await;

    // Assert
    assert_eq!(start, Err(SyncError::Failed(PlatformError::Unreachable)));
}

#[test]
fn sync_errors_and_claims_read_plainly_without_the_claim() {
    // Arrange
    let claim = ClaimId::new(CLAIM.to_string());
    let conflict = SyncError::TaskInProgress {
        holder: SyncHolder {
            last_activity_at_ms: 0,
            session: None,
        },
        task: SyncTask {
            number: 12,
            project: "agentty".to_string(),
            title: "Fix".to_string(),
            url: None,
        },
    };

    // Act
    let texts = [
        conflict.to_string(),
        SyncError::ClaimLost.to_string(),
        SyncError::TaskClosed.to_string(),
        SyncError::SyncDisabled.to_string(),
        SyncError::Failed(PlatformError::Unreachable).to_string(),
        PlatformError::InvalidRequest.to_string(),
    ];

    // Assert
    assert_eq!(format!("{claim:?}"), "ClaimId(<redacted>)");
    assert_eq!(claim.expose(), CLAIM);
    assert_eq!(
        texts,
        [
            "Another session holds task #12.",
            "Another session took the task over.",
            "The task was finished or canceled elsewhere.",
            "The platform does not offer Sync to this account.",
            "Could not reach the platform.",
            "The request does not fit what the platform accepts, so it was not sent.",
        ]
    );
}

#[test]
fn a_held_task_names_its_project_and_number_for_later_requests() {
    // Arrange
    let start = SyncStart {
        claim_id: ClaimId::new(CLAIM.to_string()),
        created: true,
        in_progress: Vec::new(),
        task: SyncTask {
            number: 12,
            project: "agentty".to_string(),
            title: "Fix".to_string(),
            url: None,
        },
    };

    // Act
    let reference = start.task.reference();

    // Assert
    assert_eq!(reference, task_ref());
}
