//! A paired device on the real `ag-remote` server over this host: it lists
//! the projects the host lists, reads a named project's sessions, and
//! creates a session there and starts its first turn, whatever project the
//! terminal shows.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::MockAgentChannel;
use ag_remote::protocol::{
    ClientFrame, RemoteProjectEntry, RemoteRequest, RemoteResponse, RemoteResult,
    RemoteSessionMode, RequestedProjectId, ServerFrame,
};
use ag_remote::{
    DeviceStore, PROTOCOL_VERSION, RemoteHost, RemoteServerConfig, RemoteServerHandle,
};
use futures_util::{SinkExt, StreamExt};
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::sync::{Notify, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::app::App;
use crate::app::test_support::{
    TestSessionRunFactory, add_listed_project, insert_loadable_session,
};
use crate::domain::session::Status;
use crate::infra::remote_control::RepositoryDeviceStore;
use crate::test_support::new_test_app;

/// Client side of one remote-control WebSocket.
type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Longest wait for an expected frame before the test fails.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_paired_device_reaches_a_named_listed_project_through_the_real_server() {
    // Arrange
    let (mut app, _directory, [home_project_id, other_project_id, third_project_id]) =
        Box::pin(three_project_app()).await;
    let server = start_server(&app).await;
    let (mut client, capabilities) = pair(&server).await;
    let create = RemoteRequest::CreateSession {
        mode: RemoteSessionMode::Regular,
        project_id: Some(requested(other_project_id)),
    };

    // Act
    let projects = app
        .drive_session_request(call(&mut client, 1, RemoteRequest::ListProjects))
        .await;
    let before_switch = app
        .drive_session_request(call(&mut client, 2, list_sessions(Some(other_project_id))))
        .await;
    let created = app
        .drive_session_request(call(&mut client, 3, create))
        .await;
    app.switch_project(third_project_id)
        .await
        .expect("project switch");
    let after_switch = app
        .drive_session_request(call(&mut client, 4, list_sessions(Some(other_project_id))))
        .await;
    let active_after_switch = app
        .drive_session_request(call(&mut client, 5, list_sessions(None)))
        .await;
    let turn_started = hold_first_turn(&mut app, &created_session_id(&created).unwrap_or_default());
    let message = RemoteRequest::SendMessage {
        images: Vec::new(),
        message: "Start here".to_string(),
        session_id: created_session_id(&created)
            .unwrap_or_default()
            .as_str()
            .into(),
    };
    let sent = app
        .drive_session_request(call(&mut client, 6, message))
        .await;
    tokio::time::timeout(FRAME_TIMEOUT, turn_started.notified())
        .await
        .expect("the new session's first turn should start");
    server.shutdown().await;

    // Assert
    assert!(
        capabilities
            .iter()
            .any(|capability| capability == "projects"),
        "{capabilities:?}"
    );
    let projects = listed_projects(&projects).expect("a device should list the projects");
    for project in [
        (home_project_id.to_string(), "home".to_string(), true),
        (other_project_id.to_string(), "other".to_string(), false),
        (third_project_id.to_string(), "third".to_string(), false),
    ] {
        assert!(projects.contains(&project), "{project:?}: {projects:?}");
    }
    assert_eq!(
        listed_sessions(&before_switch),
        Some((other_project_id.to_string(), vec!["other1".to_string()]))
    );
    let session_id = created_session_id(&created)
        .expect("a device should create a session in the named project");
    let row = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("session read")
        .expect("created session");
    assert_eq!(row.project_id, Some(other_project_id));
    // The terminal switching projects changes nothing the named listing
    // returns, while a listing that names no project follows the terminal.
    let (listed_project, mut listed_ids) =
        listed_sessions(&after_switch).expect("the named project should list");
    listed_ids.sort_unstable();
    let mut expected_ids = vec!["other1".to_string(), session_id.clone()];
    expected_ids.sort_unstable();
    // A message to the new session starts its first turn, there, while the
    // terminal shows yet another project.
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    let started = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("session read")
        .expect("created session");
    assert_eq!(started.status, "InProgress");
    assert!(
        !app.sessions.is_in_active_page(&session_id),
        "the terminal's list stays on its own project"
    );
    assert_eq!(listed_project, other_project_id.to_string());
    assert_eq!(listed_ids, expected_ids);
    assert_eq!(
        listed_sessions(&active_after_switch),
        Some((third_project_id.to_string(), vec!["third1".to_string()]))
    );
    assert_eq!(app.active_project_id(), third_project_id);
}

/// Builds an app with the listed projects `home`, `other`, and `third`,
/// each holding one review session named after it with `1` appended, and
/// the terminal on `home`. Returns their ids in that order.
async fn three_project_app() -> (App, TempDir, [i64; 3]) {
    let (mut app, directory) = new_test_app().await;
    let mut project_ids = [0; 3];
    for (project_id, name) in project_ids.iter_mut().zip(["home", "other", "third"]) {
        *project_id = add_listed_project(&mut app, &directory, name).await;
        insert_loadable_session(&app, &format!("{name}1"), Status::Review, *project_id).await;
    }
    app.switch_project(project_ids[0])
        .await
        .expect("project switch");

    (app, directory, project_ids)
}

/// Scripts the agent of `session_id` to hold its first turn open, so the
/// turn neither reaches a model nor ends, and returns the signal that it
/// started.
fn hold_first_turn(app: &mut App, session_id: &str) -> Arc<Notify> {
    let started = Arc::new(Notify::new());
    let mut channel = MockAgentChannel::new();
    let started_for_agent = Arc::clone(&started);
    channel.expect_run_turn().once().returning(move |_, _, _| {
        started_for_agent.notify_one();

        Box::pin(std::future::pending())
    });
    TestSessionRunFactory::install(&mut app.services).register(session_id, Arc::new(channel));

    started
}

/// Returns the id a device sends for `project_id`.
fn requested(project_id: i64) -> RequestedProjectId {
    RequestedProjectId::from(project_id.to_string().as_str())
}

/// Returns the request that lists the sessions of `project_id`, or of the
/// terminal's project for `None`.
fn list_sessions(project_id: Option<i64>) -> RemoteRequest {
    RemoteRequest::ListSessions {
        project_id: project_id.map(requested),
    }
}

/// Starts the real remote-control server on a free loopback port over this
/// host's remote session service and paired-device store.
async fn start_server(app: &App) -> RemoteServerHandle {
    let (_revision_tx, revisions) = watch::channel(1);
    let host = RemoteHost {
        devices: Arc::new(RepositoryDeviceStore::new(
            app.services.db().remote_devices(),
        )) as Arc<dyn DeviceStore>,
        revisions,
        sessions: app.remote_session_service(),
    };

    ag_remote::start(
        RemoteServerConfig {
            allowed_hosts: Vec::new(),
            bind_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            relay: None,
        },
        host,
    )
    .await
    .expect("remote control should start")
}

/// Pairs a device with a fresh code, approving it as the terminal would,
/// and returns its open client and the capabilities the server offered.
async fn pair(server: &RemoteServerHandle) -> (Client, Vec<String>) {
    let code = server
        .issue_pairing_code()
        .expect("pairing code should issue")
        .code;
    let url = format!("ws://{}/ws", server.local_addr());
    let (mut client, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("device should connect");
    send(
        &mut client,
        &ClientFrame::Pair {
            code,
            device_name: "Phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await;
    let confirmation = match next_frame(&mut client).await {
        ServerFrame::AwaitingApproval { confirmation } => Some(confirmation),
        _ => None,
    }
    .expect("pairing should wait for approval");
    let request_id = server
        .status()
        .wait_for(|status| {
            status
                .pairing_requests
                .iter()
                .any(|request| request.confirmation == confirmation)
        })
        .await
        .expect("pairing request should be listed")
        .pairing_requests
        .iter()
        .find(|request| request.confirmation == confirmation)
        .map(|request| request.id)
        .expect("pairing request should be listed");
    assert!(server.approve_pairing(request_id));
    let capabilities = match next_frame(&mut client).await {
        ServerFrame::Paired { capabilities, .. } => Some(capabilities),
        _ => None,
    }
    .expect("pairing should succeed");

    (client, capabilities)
}

/// Sends `request` as request `id` and returns its result, skipping change
/// notices and any other frame.
async fn call(client: &mut Client, id: u64, request: RemoteRequest) -> RemoteResult {
    send(client, &ClientFrame::Request { id, request }).await;
    loop {
        if let ServerFrame::Response {
            id: response_id,
            result,
        } = next_frame(client).await
            && response_id == id
        {
            return result;
        }
    }
}

/// Sends one JSON frame.
async fn send(client: &mut Client, frame: &ClientFrame) {
    let text = serde_json::to_string(frame).expect("frame should encode");
    client
        .send(Message::Text(text.into()))
        .await
        .expect("frame should send");
}

/// Waits for the next server frame, skipping control messages.
async fn next_frame(client: &mut Client) -> ServerFrame {
    loop {
        let message = tokio::time::timeout(FRAME_TIMEOUT, client.next())
            .await
            .expect("a frame should arrive")
            .expect("socket should stay open")
            .expect("frame should read");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).expect("frame should decode");
        }
    }
}

/// Returns the id, name, and terminal mark of each project a project
/// listing names.
fn listed_projects(result: &RemoteResult) -> Option<Vec<(String, String, bool)>> {
    let RemoteResult::Ok(RemoteResponse::Projects { projects }) = result else {
        return None;
    };

    Some(
        projects
            .iter()
            .map(|project: &RemoteProjectEntry| {
                (project.id.clone(), project.name.clone(), project.is_active)
            })
            .collect(),
    )
}

/// Returns the id of the session a creation answered with.
fn created_session_id(result: &RemoteResult) -> Option<String> {
    let RemoteResult::Ok(RemoteResponse::Created { session_id }) = result else {
        return None;
    };

    Some(session_id.clone())
}

/// Returns the project id and session ids of a session listing.
fn listed_sessions(result: &RemoteResult) -> Option<(String, Vec<String>)> {
    let RemoteResult::Ok(RemoteResponse::Sessions { project, sessions }) = result else {
        return None;
    };

    Some((
        project.as_ref()?.id.clone()?,
        sessions.iter().map(|session| session.id.clone()).collect(),
    ))
}
