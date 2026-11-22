//! Embedded web client responses, host and origin checks, and the
//! unauthenticated socket cap.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::support::{TestResult, TestServer, start_relay};

/// Sends one HTTP/1.1 `GET` naming the listener as `Host` and returns the
/// lowercased raw response.
async fn get(addr: SocketAddr, path: &str) -> TestResult<String> {
    get_with_host(addr, path, &addr.to_string()).await
}

/// Sends one HTTP/1.1 `GET` with `host` and returns the lowercased raw
/// response.
async fn get_with_host(addr: SocketAddr, path: &str, host: &str) -> TestResult<String> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;

    Ok(response.to_ascii_lowercase())
}

#[tokio::test]
async fn web_client_assets_carry_restrictive_headers() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let addr = server.handle.local_addr();

    // Act
    let page = get(addr, "/").await?;
    let script = get(addr, "/app.js").await?;
    let device_key = get(addr, "/device_key.mjs").await?;
    let noise = get(addr, "/noise.mjs").await?;
    let relay = get(addr, "/relay.mjs").await?;
    let stylesheet = get(addr, "/app.css").await?;

    // Assert
    for (response, content_type) in [
        (&page, "text/html; charset=utf-8"),
        (&script, "text/javascript; charset=utf-8"),
        (&device_key, "text/javascript; charset=utf-8"),
        (&noise, "text/javascript; charset=utf-8"),
        (&relay, "text/javascript; charset=utf-8"),
        (&stylesheet, "text/css; charset=utf-8"),
    ] {
        assert!(response.starts_with("http/1.1 200 ok"));
        assert!(response.contains(&format!("content-type: {content_type}")));
        assert!(response.contains(
            "content-security-policy: default-src 'none'; script-src 'self'; style-src 'self'; \
             connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'none'; \
             frame-ancestors 'none'"
        ));
        assert!(response.contains("x-content-type-options: nosniff"));
        assert!(response.contains("referrer-policy: no-referrer"));
        assert!(response.contains("cache-control: no-store"));
    }
    assert!(page.contains("<script type=\"module\" src=\"app.js\""));
    assert!(page.contains("<meta name=\"agentty-transport\" content=\"local\""));
    assert!(script.contains("from \"./device_key.mjs\""));
    assert!(script.contains("from \"./relay.mjs\""));
    assert!(device_key.contains("from \"./noise.mjs\""));
    assert!(relay.contains("from \"./noise.mjs\""));

    Ok(())
}

/// Splits a raw response into the headers that protect the web client and
/// its body.
fn guarded_headers_and_body(response: &str) -> (Vec<&str>, &str) {
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
    let headers = head
        .lines()
        .filter(|line| {
            [
                "http/1.1",
                "content-type:",
                "content-security-policy:",
                "x-content-type-options:",
                "referrer-policy:",
                "cache-control:",
            ]
            .iter()
            .any(|name| line.starts_with(name))
        })
        .collect();

    (headers, body)
}

/// Returns the request paths of the scripts and stylesheets `body` names.
fn referenced_paths(body: &str) -> Vec<String> {
    body.split('"')
        .filter(|quoted| {
            [".css", ".js", ".mjs"]
                .iter()
                .any(|extension| quoted.ends_with(extension))
                && !quoted.contains(char::is_whitespace)
        })
        .map(|quoted| format!("/{}", quoted.trim_start_matches("./")))
        .collect()
}

#[tokio::test]
async fn relay_serves_every_web_client_file_the_listener_serves() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let relay = start_relay().await?;
    let mut paths = vec!["/".to_string()];
    let mut compared: Vec<(String, String, String)> = Vec::new();

    // Act: follow every file the page and its scripts name, on both.
    while let Some(path) = paths.pop() {
        if compared.iter().any(|(seen, ..)| *seen == path) {
            continue;
        }
        let local = get(server.handle.local_addr(), &path).await?;
        let relayed = get(relay.local_addr(), &path).await?;
        paths.extend(referenced_paths(&local));
        compared.push((path, local, relayed));
    }

    // Assert
    for (path, local, relayed) in &compared {
        let relayed = relayed.replace(
            "name=\"agentty-transport\" content=\"relay\"",
            "name=\"agentty-transport\" content=\"local\"",
        );
        let (local_headers, local_body) = guarded_headers_and_body(local);
        let (relay_headers, relay_body) = guarded_headers_and_body(&relayed);
        assert!(
            local.starts_with("http/1.1 200 ok"),
            "listener lacks {path}"
        );
        assert_eq!(local_headers.len(), 6, "listener headers for {path}");
        assert_eq!(relay_headers, local_headers, "headers for {path}");
        assert_eq!(relay_body, local_body, "body of {path}");
    }
    let mut files: Vec<&str> = compared.iter().map(|(path, ..)| path.as_str()).collect();
    files.sort_unstable();
    assert_eq!(
        files,
        [
            "/",
            "/app.css",
            "/app.js",
            "/device_key.mjs",
            "/noise.mjs",
            "/relay.mjs"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn requests_naming_a_foreign_host_are_forbidden() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let addr = server.handle.local_addr();
    let mut upgrade = format!("ws://{addr}/ws").into_client_request()?;
    upgrade
        .headers_mut()
        .insert("Host", HeaderValue::from_static("evil.example:4646"));

    // Act
    let page = get_with_host(addr, "/", "evil.example:4646").await?;
    let allowed = get_with_host(addr, "/", "Box.Tailnet.ts.net").await?;
    let socket = tokio_tungstenite::connect_async(upgrade).await;

    // Assert
    assert!(page.starts_with("http/1.1 403 forbidden"));
    assert!(allowed.starts_with("http/1.1 200 ok"));
    let status = match socket {
        Err(Error::Http(response)) => Some(response.status().as_u16()),
        _ => None,
    };
    assert_eq!(status, Some(403));

    Ok(())
}

#[tokio::test]
async fn upgrades_beyond_the_unauthenticated_cap_are_refused() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let mut waiting = Vec::new();
    for _ in 0..8 {
        waiting.push(server.connect().await?);
    }

    // Act
    let refused = server.connect().await;
    drop(waiting.pop());
    let mut reopened = None;
    for _ in 0..100 {
        if let Ok(client) = server.connect().await {
            reopened = Some(client);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // Assert
    let status = match refused {
        Err(error) => match error.downcast_ref::<Error>() {
            Some(Error::Http(response)) => Some(response.status().as_u16()),
            _ => None,
        },
        Ok(_) => None,
    };
    assert_eq!(status, Some(503));
    assert!(reopened.is_some(), "a closed socket should free its slot");

    Ok(())
}

#[tokio::test]
async fn unknown_paths_are_not_found() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;

    // Act
    let response = get(server.handle.local_addr(), "/admin").await?;

    // Assert
    assert!(response.starts_with("http/1.1 404 not found"));

    Ok(())
}

#[tokio::test]
async fn cross_site_websocket_upgrades_are_forbidden() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let mut request = format!("ws://{}/ws", server.handle.local_addr()).into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", HeaderValue::from_static("https://evil.example"));

    // Act
    let result = tokio_tungstenite::connect_async(request).await;

    // Assert
    let status = match result {
        Err(Error::Http(response)) => Some(response.status().as_u16()),
        _ => None,
    };
    assert_eq!(status, Some(403));

    Ok(())
}

#[tokio::test]
async fn same_origin_websocket_upgrades_are_accepted() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let addr = server.handle.local_addr();
    let mut request = format!("ws://{addr}/ws").into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", HeaderValue::from_str(&format!("http://{addr}"))?);

    // Act
    let result = tokio_tungstenite::connect_async(request).await;

    // Assert
    assert!(result.is_ok());

    Ok(())
}
