//! The remote-control web client, served so a pairing link opens it.
//!
//! The files are the ones Agentty's own listener serves. The relay marks the
//! page so the client pairs with a link and encrypts end to end instead of
//! looking for a listener here. Whoever runs the relay controls this code;
//! that is part of trusting a relay.

use std::sync::LazyLock;

use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

/// Content security policy for every file of the web client.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
                                       connect-src 'self'; img-src 'self'; base-uri 'none'; \
                                       form-action 'none'; frame-ancestors 'none'";
/// Marker in the page that tells the client which transport to use.
const LOCAL_TRANSPORT: &str = r#"<meta name="agentty-transport" content="local">"#;
/// Marker that selects the relay transport.
const RELAY_TRANSPORT: &str = r#"<meta name="agentty-transport" content="relay">"#;
/// Content type of the client's scripts.
const SCRIPT: &str = "text/javascript; charset=utf-8";

/// The page, marked for the relay transport.
static PAGE: LazyLock<String> = LazyLock::new(|| {
    include_str!("../../ag-remote/assets/index.html").replacen(LOCAL_TRANSPORT, RELAY_TRANSPORT, 1)
});

/// Files the page loads, by request path, with their content types.
const FILES: [(&str, &str, &str); 10] = [
    (
        "/app.css",
        "text/css; charset=utf-8",
        include_str!("../../ag-remote/assets/app.css"),
    ),
    (
        "/app.js",
        SCRIPT,
        include_str!("../../ag-remote/assets/app.js"),
    ),
    (
        "/chat.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/chat.mjs"),
    ),
    (
        "/device_key.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/device_key.mjs"),
    ),
    (
        "/format.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/format.mjs"),
    ),
    (
        "/markdown.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/markdown.mjs"),
    ),
    (
        "/noise.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/noise.mjs"),
    ),
    (
        "/relay.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/relay.mjs"),
    ),
    (
        "/session_list.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/session_list.mjs"),
    ),
    (
        "/shell.mjs",
        SCRIPT,
        include_str!("../../ag-remote/assets/shell.mjs"),
    ),
];

/// Adds the web client's routes to `router`.
pub(crate) fn routes<State: Clone + Send + Sync + 'static>(router: Router<State>) -> Router<State> {
    FILES
        .into_iter()
        .fold(router, |router, (path, content_type, body)| {
            router.route(path, get(move || async move { file(content_type, body) }))
        })
        .route(
            "/",
            get(|| async { file("text/html; charset=utf-8", PAGE.as_str()) }),
        )
}

/// Builds one file response with restrictive headers.
fn file(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}
