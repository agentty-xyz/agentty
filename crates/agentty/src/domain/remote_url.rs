//! Validation of the addresses remote control and the platform connect to.
//!
//! Each address names a host without credentials and carries no fragment:
//! the pairing link appends its own fragment, and credentials would end up
//! in the link, its QR code, or request logs. Each address is printable
//! ASCII only, so one that a platform sends cannot carry terminal control
//! sequences onto the screen.

/// Accepts a relay base URL: `ws://` or `wss://`, a host, and no query.
/// A trailing slash is dropped.
///
/// # Errors
/// Returns a user-facing message naming the expected form.
pub fn parse_relay_url(value: &str) -> Result<String, String> {
    parse_base_url(value, &["wss://", "ws://"])
        .ok_or_else(|| "expected a relay URL such as wss://relay.example".to_string())
}

/// Accepts a web page Agentty opens or links to, such as the page a pairing
/// link opens or a sign-in page: `http://` or `https://` and a host. Unlike
/// the other addresses, it may carry a query and keeps a trailing slash,
/// since the page may be a directory.
///
/// # Errors
/// Returns a user-facing message naming the expected form.
pub fn parse_page_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    let (page, query) = value.split_once('?').unwrap_or((value, ""));
    let is_valid = parse_base_url(page, &["https://", "http://"]).is_some()
        && !query.contains(['#', '?'])
        && query.chars().all(|character| character.is_ascii_graphic());
    if !is_valid {
        return Err("expected a page URL such as https://platform.example/".to_string());
    }

    Ok(value.to_string())
}

/// Accepts a platform base URL without a query: `https://` and a host, or
/// `http://` for a loopback host only, so an access token never crosses a
/// network in plain text. A trailing slash is dropped.
///
/// # Errors
/// Returns a user-facing message naming the expected form.
pub fn parse_platform_url(value: &str) -> Result<String, String> {
    let error = || {
        "expected a platform URL such as https://platform.agentty.xyz; plain http:// is accepted \
         only for localhost"
            .to_string()
    };
    if let Some(url) = parse_base_url(value, &["https://"]) {
        return Ok(url);
    }
    let url = parse_base_url(value, &["http://"]).ok_or_else(error)?;
    // Strip the scheme exactly once, so `http://http://localhost` is judged
    // by the host a client would dial.
    let authority = url
        .get("http://".len()..)
        .unwrap_or_default()
        .split('/')
        .next()
        .unwrap_or_default();
    let host = authority
        .strip_prefix('[')
        .and_then(|bracketed| bracketed.split(']').next())
        .unwrap_or_else(|| authority.split(':').next().unwrap_or_default());
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(error());
    }

    Ok(url)
}

/// Returns `value` without surrounding whitespace and a trailing slash when
/// it starts with one of `schemes` and names a host without credentials,
/// whitespace, query, or fragment.
fn parse_base_url(value: &str, schemes: &[&str]) -> Option<String> {
    let url = value.trim().trim_end_matches('/');
    let rest = schemes.iter().find_map(|scheme| url.strip_prefix(scheme))?;
    let authority = rest.split('/').next().unwrap_or_default();
    let is_valid = !authority.is_empty()
        && !authority.contains('@')
        && !rest.contains(['?', '#'])
        && rest.chars().all(|character| character.is_ascii_graphic());

    is_valid.then(|| url.to_string())
}

#[cfg(test)]
#[path = "remote_url_test.rs"]
mod tests;
