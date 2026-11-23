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

/// Content security policy for every file of the web client. Fonts may only
/// be the `data:` URIs the stylesheet embeds.
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
                                       font-src data:; connect-src 'self'; img-src 'self'; \
                                       base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
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

/// One script of the web client, served at its file name.
macro_rules! script {
    ($file:literal) => {
        (
            concat!("/", $file),
            SCRIPT,
            include_str!(concat!("../../ag-remote/assets/", $file)),
        )
    };
}

/// Files the page loads, by request path, with their content types.
const FILES: [(&str, &str, &str); 23] = [
    // The generated design tokens, font faces, and shared base styles come
    // first, as Agentty's listener serves them.
    (
        "/app.css",
        "text/css; charset=utf-8",
        concat!(
            include_str!("../../ag-remote/assets/tokens.css"),
            include_str!("../../ag-remote/assets/fonts.css"),
            include_str!("../../ag-remote/assets/base.css"),
            include_str!("../../ag-remote/assets/app.css")
        ),
    ),
    script!("app.js"),
    script!("chat.mjs"),
    script!("composer.mjs"),
    script!("connection.mjs"),
    script!("device_key.mjs"),
    script!("diff.mjs"),
    script!("format.mjs"),
    script!("graph.mjs"),
    script!("home.mjs"),
    script!("instance.mjs"),
    script!("markdown.mjs"),
    script!("mention.mjs"),
    script!("noise.mjs"),
    script!("pairings.mjs"),
    script!("project.mjs"),
    script!("relay.mjs"),
    script!("session_list.mjs"),
    script!("settings.mjs"),
    script!("shell.mjs"),
    script!("slash.mjs"),
    script!("suggest.mjs"),
    script!("theme.js"),
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
