//! Embedded web client responses and WebSocket origin checks.

use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Error;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::support::{TestResult, TestServer};

/// Sends one HTTP/1.1 `GET` and returns the lowercased raw response.
async fn get(addr: SocketAddr, path: &str) -> TestResult<String> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
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
    let stylesheet = get(addr, "/app.css").await?;

    // Assert
    for (response, content_type) in [
        (&page, "text/html; charset=utf-8"),
        (&script, "text/javascript; charset=utf-8"),
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
    assert!(page.contains("<script src=\"/app.js\""));

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
