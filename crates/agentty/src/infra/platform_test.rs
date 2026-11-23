//! Platform client tests against a fake platform server.

use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{any, body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::infra::platform::{
    AccessToken, DeviceAuthorization, DeviceCode, HttpPlatformClient, PlatformAccount,
    PlatformClient, PlatformError, RegisteredInstance, RemoteControlService,
};

/// Token the fake platform accepts.
const TOKEN: &str = "agp_test-token";
/// Instance identifier used in registration tests.
const INSTANCE_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Starts a fake platform that answers `method_name` requests to
/// `route` with `response`, and a client for it.
async fn fake_platform(
    method_name: &str,
    route: &str,
    response: ResponseTemplate,
) -> (MockServer, HttpPlatformClient) {
    let server = MockServer::start().await;
    Mock::given(method(method_name))
        .and(path(route))
        .respond_with(response)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client should build");

    (server, client)
}

/// Builds an error answer with the contract's body shape.
fn error_answer(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({ "error": code, "message": "Nope." }))
}

/// Returns the test token.
fn token() -> AccessToken {
    AccessToken::new(TOKEN.to_string())
}

/// Builds a valid device authorization answer.
fn device_authorization(interval: Option<u64>) -> Value {
    let mut answer = json!({
        "device_code": "device-code-secret",
        "user_code": "BCDF-GHJK",
        "verification_uri": "https://platform.example/device",
        "verification_uri_complete": "https://platform.example/device?user_code=BCDF-GHJK",
        "expires_in": 600,
    });
    if let Some(interval) = interval {
        answer["interval"] = json!(interval);
    }

    answer
}

#[tokio::test]
async fn device_authorization_names_this_agentty_and_keeps_the_interval() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/device/authorizations"))
        .and(body_json(json!({ "client_name": "Agentty on box" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(device_authorization(Some(7))))
        .expect(1)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&format!("{}/", server.uri())).expect("client");

    // Act
    let authorization = client
        .start_device_authorization("Agentty on box")
        .await
        .expect("authorization should start");

    // Assert
    assert_eq!(authorization.user_code, "BCDF-GHJK");
    assert_eq!(
        authorization.verification_uri,
        "https://platform.example/device"
    );
    assert_eq!(
        authorization.verification_uri_complete.as_deref(),
        Some("https://platform.example/device?user_code=BCDF-GHJK")
    );
    assert_eq!(authorization.expires_in, Duration::from_secs(600));
    assert_eq!(authorization.interval, Duration::from_secs(7));
    assert_eq!(
        authorization.device_code,
        DeviceCode::new("device-code-secret".to_string())
    );
}

#[tokio::test]
async fn device_authorization_bounds_the_interval_and_the_lifetime_the_platform_names() {
    // Arrange: interval, lifetime, then the interval and lifetime kept.
    let cases = [
        (None, 600, 5, 600),
        (Some(0), 600, 1, 600),
        (Some(u64::MAX), u64::MAX, 60, 3600),
    ];

    for (interval, expires_in, expected_interval, expected_expiry) in cases {
        let mut answer = device_authorization(interval);
        answer["expires_in"] = json!(expires_in);
        let (_server, client) = fake_platform(
            "POST",
            "/api/v1/device/authorizations",
            ResponseTemplate::new(200).set_body_json(answer),
        )
        .await;

        // Act
        let authorization = client
            .start_device_authorization("Agentty")
            .await
            .expect("authorization should start");

        // Assert
        assert_eq!(
            authorization.interval,
            Duration::from_secs(expected_interval)
        );
        assert_eq!(
            authorization.expires_in,
            Duration::from_secs(expected_expiry)
        );
    }
}

#[tokio::test]
async fn device_authorization_refuses_codes_and_pages_the_terminal_cannot_show_safely() {
    // Arrange
    let mut escape_code = device_authorization(Some(5));
    escape_code["user_code"] = json!("BCDF\u{1b}[2J");
    let mut file_page = device_authorization(Some(5));
    file_page["verification_uri"] = json!("file:///etc/passwd");
    let mut command_page = device_authorization(Some(5));
    command_page["verification_uri_complete"] = json!("javascript:alert(1)");
    let mut escape_page = device_authorization(Some(5));
    escape_page["verification_uri"] = json!("https://platform.example/device\u{1b}[2J");
    let mut expired = device_authorization(Some(5));
    expired["expires_in"] = json!(0);
    let mut no_device_code = device_authorization(Some(5));
    no_device_code["device_code"] = json!("");
    let cases = [
        escape_code,
        file_page,
        command_page,
        escape_page,
        expired,
        no_device_code,
        json!({ "user_code": "BCDF-GHJK" }),
    ];

    for answer in cases {
        let (_server, client) = fake_platform(
            "POST",
            "/api/v1/device/authorizations",
            ResponseTemplate::new(200).set_body_json(answer),
        )
        .await;

        // Act
        let result = client.start_device_authorization("Agentty").await;

        // Assert
        assert_eq!(result, Err(PlatformError::InvalidResponse));
    }
}

#[tokio::test]
async fn device_token_poll_reports_each_pending_and_final_outcome() {
    // Arrange
    let cases = [
        ("authorization_pending", PlatformError::AuthorizationPending),
        ("slow_down", PlatformError::SlowDown),
        ("access_denied", PlatformError::AccessDenied),
        ("expired_token", PlatformError::ExpiredToken),
        (
            "invalid_grant",
            PlatformError::Rejected {
                message: Some("Nope.".to_string()),
                status: 400,
            },
        ),
    ];

    for (code, expected) in cases {
        let (_server, client) =
            fake_platform("POST", "/api/v1/device/token", error_answer(400, code)).await;

        // Act
        let result = client
            .poll_device_token(&DeviceCode::new("device-code-secret".to_string()))
            .await;

        // Assert
        assert_eq!(result, Err(expected));
    }
}

#[tokio::test]
async fn device_token_poll_returns_the_token_and_login_once_approved() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/device/token"))
        .and(body_json(json!({ "device_code": "device-code-secret" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": TOKEN,
            "token_type": "bearer",
            "account": { "login": "octocat" },
            "unknown": "ignored",
        })))
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");

    // Act
    let sign_in = client
        .poll_device_token(&DeviceCode::new("device-code-secret".to_string()))
        .await
        .expect("sign-in should finish");

    // Assert
    assert_eq!(sign_in.access_token, token());
    assert_eq!(sign_in.login, "octocat");
}

#[tokio::test]
async fn device_token_poll_refuses_an_empty_token_or_login() {
    // Arrange
    let cases = [
        json!({ "access_token": "", "account": { "login": "octocat" } }),
        json!({ "access_token": TOKEN, "account": { "login": "\u{7}" } }),
    ];

    for answer in cases {
        let (_server, client) = fake_platform(
            "POST",
            "/api/v1/device/token",
            ResponseTemplate::new(200).set_body_json(answer),
        )
        .await;

        // Act
        let result = client
            .poll_device_token(&DeviceCode::new("code".to_string()))
            .await;

        // Assert
        assert_eq!(result, Err(PlatformError::InvalidResponse));
    }
}

#[tokio::test]
async fn account_presents_the_token_and_returns_the_offered_relay() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/account"))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account": { "login": "octo\u{1b}cat" },
            "services": {
                "remote_control": {
                    "relay_url": "wss://relay.platform.example/",
                    "pairing_url": "https://platform.example/sessions/",
                },
            },
        })))
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");

    // Act
    let account = client.account(&token()).await;

    // Assert
    assert_eq!(
        account,
        Ok(PlatformAccount {
            login: "octocat".to_string(),
            remote_control: Some(RemoteControlService {
                pairing_url: "https://platform.example/sessions/".to_string(),
                relay_url: "wss://relay.platform.example".to_string(),
            }),
        })
    );
}

#[tokio::test]
async fn account_without_a_relay_offers_no_remote_control() {
    // Arrange
    let answers = [
        json!({ "account": { "login": "octocat" } }),
        json!({ "account": { "login": "octocat" }, "services": {} }),
    ];

    for answer in answers {
        let (_server, client) = fake_platform(
            "GET",
            "/api/v1/account",
            ResponseTemplate::new(200).set_body_json(answer),
        )
        .await;

        // Act
        let account = client.account(&token()).await;

        // Assert
        assert_eq!(
            account,
            Ok(PlatformAccount {
                login: "octocat".to_string(),
                remote_control: None,
            })
        );
    }
}

#[tokio::test]
async fn account_refuses_relay_and_pairing_addresses_that_are_not_web_addresses() {
    // Arrange
    let services = [
        json!({ "relay_url": "https://relay.example", "pairing_url": "https://p.example/" }),
        json!({ "relay_url": "wss://relay.example", "pairing_url": "wss://p.example/" }),
        json!({ "relay_url": "wss://relay.example", "pairing_url": "https://p.example/#x" }),
    ];

    for remote_control in services {
        let (_server, client) = fake_platform(
            "GET",
            "/api/v1/account",
            ResponseTemplate::new(200).set_body_json(json!({
                "account": { "login": "octocat" },
                "services": { "remote_control": remote_control },
            })),
        )
        .await;

        // Act
        let account = client.account(&token()).await;

        // Assert
        assert_eq!(account, Err(PlatformError::InvalidResponse));
    }
}

#[tokio::test]
async fn account_errors_report_revocation_waitlist_and_rate_limits() {
    // Arrange
    let cases = [
        (
            error_answer(401, "invalid_token"),
            PlatformError::InvalidToken,
        ),
        (error_answer(403, "waitlisted"), PlatformError::Waitlisted),
        (
            error_answer(429, "rate_limited").insert_header("retry-after", "7"),
            PlatformError::RateLimited {
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
        (
            ResponseTemplate::new(429),
            PlatformError::RateLimited { retry_after: None },
        ),
        (
            ResponseTemplate::new(503).set_body_string("<html>down</html>"),
            PlatformError::Rejected {
                message: None,
                status: 503,
            },
        ),
        (
            ResponseTemplate::new(200).set_body_string("not json"),
            PlatformError::InvalidResponse,
        ),
        (
            ResponseTemplate::new(200).set_body_string("x".repeat(64 * 1024 + 1)),
            PlatformError::InvalidResponse,
        ),
    ];

    for (response, expected) in cases {
        let (_server, client) = fake_platform("GET", "/api/v1/account", response).await;

        // Act
        let result = client.account(&token()).await;

        // Assert
        assert_eq!(result, Err(expected));
    }
}

#[tokio::test]
async fn register_instance_sends_the_name_and_returns_the_registration() {
    // Arrange
    let server = MockServer::start().await;
    let route = format!("/api/v1/instances/{INSTANCE_ID}");
    Mock::given(method("PUT"))
        .and(path(route.as_str()))
        .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_json(json!({ "name": "Agentty on box" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "instance_id": INSTANCE_ID,
            "name": "Agentty on box",
        })))
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");

    // Act
    let registered = client
        .register_instance(&token(), INSTANCE_ID, "Agentty on box")
        .await;

    // Assert
    assert_eq!(
        registered,
        Ok(RegisteredInstance {
            instance_id: INSTANCE_ID.to_string(),
            name: "Agentty on box".to_string(),
        })
    );
}

#[tokio::test]
async fn register_instance_reports_limits_waitlist_and_mismatched_answers() {
    // Arrange
    let other_instance = "f".repeat(64);
    let cases = [
        (
            error_answer(403, "instance_limit"),
            PlatformError::InstanceLimit,
        ),
        (error_answer(403, "waitlisted"), PlatformError::Waitlisted),
        (
            error_answer(403, "forbidden"),
            PlatformError::Rejected {
                message: Some("Nope.".to_string()),
                status: 403,
            },
        ),
        (
            error_answer(422, "invalid_name"),
            PlatformError::Rejected {
                message: Some("Nope.".to_string()),
                status: 422,
            },
        ),
        (
            ResponseTemplate::new(200)
                .set_body_json(json!({ "instance_id": other_instance, "name": "Box" })),
            PlatformError::InvalidResponse,
        ),
        (
            ResponseTemplate::new(200)
                .set_body_json(json!({ "instance_id": INSTANCE_ID, "name": " " })),
            PlatformError::InvalidResponse,
        ),
    ];
    let route = format!("/api/v1/instances/{INSTANCE_ID}");

    for (response, expected) in cases {
        let (_server, client) = fake_platform("PUT", &route, response).await;

        // Act
        let result = client.register_instance(&token(), INSTANCE_ID, "Box").await;

        // Assert
        assert_eq!(result, Err(expected));
    }
}

#[tokio::test]
async fn sign_out_requests_remove_the_instance_and_revoke_the_token() {
    // Arrange
    let server = MockServer::start().await;
    let route = format!("/api/v1/instances/{INSTANCE_ID}");
    for route in [route.as_str(), "/api/v1/token"] {
        Mock::given(method("DELETE"))
            .and(path(route))
            .and(header("authorization", format!("Bearer {TOKEN}").as_str()))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
    }
    let client = HttpPlatformClient::new(&server.uri()).expect("client");

    // Act
    let removed = client.remove_instance(&token(), INSTANCE_ID).await;
    let revoked = client.revoke_token(&token()).await;

    // Assert
    assert_eq!(removed, Ok(()));
    assert_eq!(revoked, Ok(()));
}

#[tokio::test]
async fn malformed_instance_identifiers_never_reach_the_platform() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    let client = HttpPlatformClient::new(&server.uri()).expect("client");
    let uppercase = INSTANCE_ID.to_uppercase();

    // Act
    let removed = client.remove_instance(&token(), "../token").await;
    let registered = client.register_instance(&token(), &uppercase, "Box").await;

    // Assert
    assert_eq!(removed, Err(PlatformError::InvalidInstance));
    assert_eq!(registered, Err(PlatformError::InvalidInstance));
}

#[tokio::test]
async fn unreachable_platform_is_reported_as_such() {
    // Arrange: nothing listens on the platform address.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let address = unused.local_addr().expect("address");
    drop(unused);
    let client = HttpPlatformClient::new(&format!("http://{address}")).expect("client");

    // Act
    let result = client.revoke_token(&token()).await;

    // Assert
    assert_eq!(result, Err(PlatformError::Unreachable));
}

#[test]
fn secrets_stay_out_of_debug_output() {
    // Arrange
    let authorization = DeviceAuthorization {
        device_code: DeviceCode::new("device-code-secret".to_string()),
        expires_in: Duration::from_secs(600),
        interval: Duration::from_secs(5),
        user_code: "BCDF-GHJK".to_string(),
        verification_uri: "https://platform.example/device?user_code=BCDF-GHJK".to_string(),
        verification_uri_complete: Some(
            "https://platform.example/device?user_code=BCDF-GHJK".to_string(),
        ),
    };

    // Act
    let debug = format!("{authorization:?} {:?}", token());

    // Assert
    assert!(!debug.contains("device-code-secret"));
    assert!(!debug.contains("BCDF-GHJK"));
    assert!(!debug.contains(TOKEN));
    assert_eq!(token().expose(), TOKEN);
}

#[test]
fn rejected_requests_say_what_the_platform_said_when_it_said_anything() {
    // Arrange
    let with_message = PlatformError::Rejected {
        message: Some("Name too long.".to_string()),
        status: 422,
    };
    let without_message = PlatformError::Rejected {
        message: None,
        status: 503,
    };

    // Act
    let texts = [with_message.to_string(), without_message.to_string()];

    // Assert
    assert_eq!(
        texts,
        [
            "The platform refused the request (422): Name too long.",
            "The platform refused the request (503).",
        ]
    );
}
