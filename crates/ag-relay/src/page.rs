//! The remote-control web client, served so a pairing link opens it.
//!
//! The files are the ones Agentty's own listener serves. The relay marks the
//! page so the client pairs with a link and encrypts end to end instead of
//! looking for a listener here. Whoever runs the relay controls this code;
//! that is part of trusting a relay.

use std::sync::LazyLock;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::web_client::{self, WebClientPage};

/// The relay's own page.
static PAGE: LazyLock<WebClientPage> = LazyLock::new(WebClientPage::default);

/// Adds the web client's routes to `router`.
pub(crate) fn routes<State: Clone + Send + Sync + 'static>(router: Router<State>) -> Router<State> {
    web_client::web_client_files()
        .iter()
        .fold(router, |router, file| {
            router.route(
                &format!("/{}", file.path),
                get(move || async move { respond(file.content_type, file.body) }),
            )
        })
        .route(
            "/",
            get(|| async { respond("text/html; charset=utf-8", PAGE.html()) }),
        )
}

/// Builds one response with the page's restrictive headers.
fn respond(content_type: &'static str, body: &'static str) -> Response {
    let mut response = body.into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.extend(PAGE.headers().map(|(name, value)| {
        (
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        )
    }));

    response
}
