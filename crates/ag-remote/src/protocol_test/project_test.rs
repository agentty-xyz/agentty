//! Project frames: the `project_id` that requests about a project may
//! carry, compatible with older pages and servers in both directions, the
//! `list_projects` answer, the project ids that answers name, and the check
//! that a named project is listed.

use std::sync::Arc;

use ag_session::{
    HostProject, HostProjectStatus, ProjectScope, SessionBackend, SessionError, SessionService,
};
use serde::Deserialize;
use serde_json::json;

use crate::protocol::{
    RemoteProject, RemoteProjectEntry, RemoteRequest, RemoteResponse, RemoteSessionMode,
    RequestedProjectId,
};
use crate::source_support::{RecordingBackend, host_fixture};

/// `RemoteRequest` as a server before `project_id` declared it, with the
/// same serde attributes, for checking what such a server makes of a newer
/// page's frames.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
enum OlderRequest {
    CreateSession {
        #[serde(default)]
        mode: RemoteSessionMode,
    },
    GetHost,
    ListProjectFiles,
    ListSessions,
    SyncProject,
    #[serde(other)]
    Unsupported,
}

/// The requests about a project with `project_id`, in the order of
/// [`OLDER_FRAMES`].
fn project_requests(project_id: Option<&str>) -> Vec<RemoteRequest> {
    let project_id = || project_id.map(RequestedProjectId::from);

    vec![
        RemoteRequest::GetHost {
            project_id: project_id(),
        },
        RemoteRequest::ListProjectFiles {
            project_id: project_id(),
        },
        RemoteRequest::ListSessions {
            project_id: project_id(),
        },
        RemoteRequest::SyncProject {
            project_id: project_id(),
        },
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: project_id(),
        },
    ]
}

/// The requests about a project exactly as a page before `project_id`
/// sends them.
const OLDER_FRAMES: [&str; 5] = [
    r#"{"method":"get_host"}"#,
    r#"{"method":"list_project_files"}"#,
    r#"{"method":"list_sessions"}"#,
    r#"{"method":"sync_project"}"#,
    r#"{"method":"create_session","mode":"regular"}"#,
];

#[test]
fn an_older_page_s_requests_are_for_the_active_project_and_encode_unchanged() {
    // Arrange
    let without_project = project_requests(None);

    // Act
    let decoded: Vec<RemoteRequest> = OLDER_FRAMES
        .iter()
        .map(|frame| serde_json::from_str(frame).expect("an older frame should parse"))
        .collect();
    let encoded: Vec<String> = without_project
        .iter()
        .map(|request| serde_json::to_string(request).expect("request should encode"))
        .collect();
    let bare_creation: RemoteRequest = serde_json::from_str(r#"{"method":"create_session"}"#)
        .expect("a creation without a mode should parse");

    // Assert
    assert_eq!(decoded, without_project);
    assert_eq!(encoded, OLDER_FRAMES);
    assert_eq!(
        bare_creation,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: None,
        }
    );
}

#[test]
fn a_named_project_travels_as_a_string_and_round_trips() {
    // Arrange
    let named = project_requests(Some("2"));

    // Act
    let encoded = serde_json::to_value(&named).expect("requests should encode");
    let decoded: Vec<RemoteRequest> =
        serde_json::from_value(encoded.clone()).expect("requests should parse");
    let listing: RemoteRequest = serde_json::from_value(json!({"method": "list_projects"}))
        .expect("a project listing should parse");

    // Assert
    assert_eq!(
        encoded,
        json!([
            {"method": "get_host", "project_id": "2"},
            {"method": "list_project_files", "project_id": "2"},
            {"method": "list_sessions", "project_id": "2"},
            {"method": "sync_project", "project_id": "2"},
            {"method": "create_session", "mode": "regular", "project_id": "2"}
        ])
    );
    assert_eq!(decoded, named);
    assert_eq!(listing, RemoteRequest::ListProjects);
}

// A server that predates `project_id` drops the field it does not know and
// answers for its active project, so a page must send `project_id` only to
// a server that offers `projects`, and must check the project an answer
// names. Such a server answers `list_projects` as an unknown method.
#[test]
fn an_older_server_ignores_a_named_project_and_does_not_know_the_listing() {
    // Arrange
    let frames: Vec<serde_json::Value> = project_requests(Some("2"))
        .iter()
        .map(|request| serde_json::to_value(request).expect("request should encode"))
        .chain([json!({"method": "list_projects"})])
        .collect();

    // Act
    let parsed: Vec<OlderRequest> = frames
        .into_iter()
        .map(|frame| serde_json::from_value(frame).expect("an older server parses the frame"))
        .collect();

    // Assert
    assert_eq!(
        parsed,
        [
            OlderRequest::GetHost,
            OlderRequest::ListProjectFiles,
            OlderRequest::ListSessions,
            OlderRequest::SyncProject,
            OlderRequest::CreateSession {
                mode: RemoteSessionMode::Regular,
            },
            OlderRequest::Unsupported,
        ]
    );
}

#[test]
fn project_listings_name_each_project_and_leave_out_empty_fields() {
    // Arrange
    let projects = vec![
        HostProject {
            active_session_count: 2,
            id: 3,
            is_active: true,
            name: "agentty".to_string(),
        },
        HostProject {
            active_session_count: 0,
            id: 12,
            is_active: false,
            name: "website".to_string(),
        },
    ];
    let response = RemoteResponse::Projects {
        projects: projects.into_iter().map(RemoteProjectEntry::from).collect(),
    };

    // Act
    let encoded = serde_json::to_value(&response).expect("listing should encode");
    let decoded: RemoteResponse =
        serde_json::from_value(encoded.clone()).expect("listing should parse");

    // Assert
    assert_eq!(
        encoded,
        json!({"kind": "projects", "projects": [
            {"active_sessions": 2, "id": "3", "is_active": true, "name": "agentty"},
            {"id": "12", "name": "website"}
        ]})
    );
    assert_eq!(decoded, response);
}

#[test]
fn answers_name_their_project_only_when_the_host_does() {
    // Arrange
    let status = HostProjectStatus {
        ahead_behind: None,
        branch: Some("site".to_string()),
        project_name: "website".to_string(),
        sync: None,
    };

    // Act
    let named = RemoteProject::listed(status.clone(), Some(2));
    let unnamed = RemoteProject::listed(status, None);
    let host = RemoteResponse::from(host_fixture());

    // Assert
    assert_eq!(
        serde_json::to_value(&named).expect("project should encode"),
        json!({"branch": "site", "id": "2", "name": "website"})
    );
    assert_eq!(
        serde_json::to_value(&unnamed).expect("project should encode"),
        json!({"branch": "site", "name": "website"})
    );
    assert!(matches!(
        host,
        RemoteResponse::Host { project_id: Some(ref id), .. } if id == "1"
    ));
}

/// Checks `project_id` against a host that lists projects `1` and `2`,
/// returning the outcome and the host calls it made.
async fn check(
    backend: RecordingBackend,
    project_id: &str,
) -> (Result<ProjectScope, SessionError>, Vec<String>) {
    let backend = Arc::new(backend);
    let sessions = SessionService::new(Arc::clone(&backend) as Arc<dyn SessionBackend>);
    let outcome = RequestedProjectId::from(project_id).listed(&sessions).await;

    (outcome, backend.calls())
}

#[tokio::test]
async fn a_named_project_reaches_the_host_only_when_the_host_lists_it() {
    // Arrange
    let malformed = ["", "two", "02", "+2", " 2", "2.0", "99999999999999999999"];

    // Act
    let (listed, listed_calls) = check(RecordingBackend::default(), "2").await;
    let (unlisted, unlisted_calls) = check(RecordingBackend::default(), "9").await;
    let (failed, _) = check(RecordingBackend::failing(), "2").await;
    let mut refused = Vec::new();
    for project_id in malformed {
        refused.push(check(RecordingBackend::default(), project_id).await);
    }

    // Assert
    assert_eq!(listed, Ok(ProjectScope::Project(2)));
    assert_eq!(listed_calls, ["projects"]);
    assert_eq!(
        unlisted,
        Err(SessionError::Operation("Project not found".to_string()))
    );
    assert_eq!(unlisted_calls, ["projects"]);
    assert_eq!(
        failed,
        Err(SessionError::Operation("backend failed".to_string()))
    );
    for (outcome, calls) in refused {
        assert_eq!(
            outcome,
            Err(SessionError::Operation("Project not found".to_string()))
        );
        // A malformed id never reaches the host.
        assert_eq!(calls, Vec::<String>::new());
    }
}
