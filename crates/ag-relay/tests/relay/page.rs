//! The web client the relay serves for pairing links.

use std::net::SocketAddr;

use ag_relay::RelayLimits;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::support::{TestResult, start};

/// Fetches `path` and returns the whole response.
async fn get(address: SocketAddr, path: &str) -> TestResult<String> {
    let mut connection = TcpStream::connect(address).await?;
    connection
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: relay\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await?;
    let mut response = String::new();
    connection.read_to_string(&mut response).await?;

    Ok(response)
}

#[tokio::test]
async fn relay_serves_the_web_client_marked_for_the_relay_transport() -> TestResult {
    // Arrange
    let relay = start(RelayLimits::default()).await?;
    let address = relay.local_addr();

    // Act
    let page = get(address, "/").await?;
    let script = get(address, "/app.js").await?;
    let device_key = get(address, "/device_key.mjs").await?;
    let graph = get(address, "/graph.mjs").await?;
    let noise = get(address, "/noise.mjs").await?;
    let transport = get(address, "/relay.mjs").await?;
    let stylesheet = get(address, "/app.css").await?;

    // Assert
    for (response, content_type) in [
        (&page, "text/html; charset=utf-8"),
        (&script, "text/javascript; charset=utf-8"),
        (&device_key, "text/javascript; charset=utf-8"),
        (&graph, "text/javascript; charset=utf-8"),
        (&noise, "text/javascript; charset=utf-8"),
        (&transport, "text/javascript; charset=utf-8"),
        (&stylesheet, "text/css; charset=utf-8"),
    ] {
        let response = response.to_ascii_lowercase();
        assert!(response.starts_with("http/1.1 200 ok"));
        assert!(response.contains(&format!("content-type: {content_type}")));
        assert!(response.contains(
            "content-security-policy: default-src 'none'; script-src 'self'; style-src 'self'; \
             font-src data:; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action \
             'none'; frame-ancestors 'none'"
        ));
        assert!(response.contains("x-content-type-options: nosniff"));
        assert!(response.contains("referrer-policy: no-referrer"));
        assert!(response.contains("cache-control: no-store"));
    }
    assert!(page.contains(r#"<meta name="agentty-transport" content="relay">"#));
    assert!(!page.contains(r#"content="local""#));
    assert!(page.contains(r#"<script type="module" src="app.js">"#));
    // The generated design tokens, the self-hosted font face as the `data:`
    // URI the policy's `font-src` allows, then the shared base styles, come
    // ahead of the client's own rules.
    let tokens = stylesheet.find("--color-bg: #070b08;");
    let fonts = stylesheet.find("src: url(\"data:font/woff2;base64,");
    let base = stylesheet.find(".skip-link {");
    let rules = stylesheet.find("--tone-ready:");
    assert!(
        tokens.is_some() && tokens < fonts && fonts < base && base < rules,
        "{stylesheet}"
    );
    assert!(script.contains(r#"from "./device_key.mjs""#));
    assert!(script.contains(r#"from "./relay.mjs""#));
    assert!(script.contains(r#"from "./graph.mjs""#));
    assert!(graph.contains(r#"from "./markdown.mjs""#));
    assert!(device_key.contains(r#"from "./noise.mjs""#));
    assert!(transport.contains(r#"from "./noise.mjs""#));
    assert!(noise.contains("Noise_IK_25519_AESGCM_SHA256"));

    Ok(())
}
