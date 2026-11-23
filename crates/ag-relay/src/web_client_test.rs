use std::collections::BTreeSet;

use super::{
    WebClientError, WebClientLink, WebClientOptions, WebClientPage, WebClientSection, relay_origin,
    replace_elements, same_origin_path, web_client_files,
};

/// The relay's own content security policy.
const DEFAULT_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src \
                              data:; connect-src 'self'; img-src 'self'; base-uri 'none'; \
                              form-action 'none'; frame-ancestors 'none'";

/// Options that set everything, with labels that try to escape their text.
fn full_options() -> WebClientOptions {
    WebClientOptions {
        relay_url: Some("wss://Relay.Example:8443/base/".to_string()),
        directory_path: Some("/api/instances?scope=all".to_string()),
        brand_badge: Some("<b>cloud</b>".to_string()),
        sections: vec![
            WebClientSection {
                label: "Dashboard".to_string(),
                href: "/dashboard".to_string(),
                current: false,
                badge: None,
            },
            WebClientSection {
                label: "Sessions & \"more\"".to_string(),
                href: "/sessions/".to_string(),
                current: true,
                badge: Some("2'".to_string()),
            },
        ],
        account: Some(WebClientLink {
            label: " ada<script> ".to_string(),
            href: "/account".to_string(),
        }),
    }
}

#[test]
fn default_options_render_the_relays_own_page() {
    // Arrange
    let options = WebClientOptions::default();

    // Act
    let page = WebClientPage::new(&options);

    // Assert
    let page = page.expect("default options render");
    assert_eq!(page, WebClientPage::default());
    assert!(
        page.html()
            .contains(r#"<meta name="agentty-transport" content="relay">"#)
    );
    assert!(!page.html().contains(r#"content="local""#));
    assert!(!page.html().contains("agentty-relay"));
    assert!(!page.html().contains("agentty-directory"));
    assert_eq!(
        page.html()
            .matches(r#"<span class="badge">remote</span>"#)
            .count(),
        2
    );
    assert_eq!(
        page.html()
            .matches(r#"<nav class="sections" aria-label="Sections" hidden>"#)
            .count(),
        2
    );
    assert!(page.html().contains(r#"<a class="account" hidden></a>"#));
    assert_eq!(page.content_security_policy(), DEFAULT_POLICY);
    assert_eq!(page.connect_sources(), ["'self'"]);
    assert_eq!(
        page.headers(),
        [
            ("content-security-policy", DEFAULT_POLICY),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ]
    );
}

#[test]
fn options_render_the_relay_directory_navigation_and_account_escaped() {
    // Arrange
    let options = full_options();

    // Act
    let page = WebClientPage::new(&options).expect("options render");

    // Assert
    let html = page.html();
    assert!(html.contains(concat!(
        r#"<meta name="agentty-transport" content="relay">"#,
        r#"<meta name="agentty-relay" content="wss://Relay.Example:8443/base">"#,
        r#"<meta name="agentty-directory" content="/api/instances?scope=all">"#
    )));
    assert_eq!(
        html.matches(r#"<span class="badge">&lt;b&gt;cloud&lt;/b&gt;</span>"#)
            .count(),
        2
    );
    assert!(!html.contains(">remote<"));
    let navigation = concat!(
        r#"<nav class="sections" aria-label="Sections"><ul>"#,
        r#"<li><a href="/dashboard">Dashboard</a></li>"#,
        r#"<li><a href="/sessions/" aria-current="page">Sessions &amp; &quot;more&quot;"#,
        r#"<span class="badge">2&#39;</span></a></li>"#,
        "</ul></nav>"
    );
    assert_eq!(html.matches(navigation).count(), 2);
    assert!(!html.contains(r#"aria-label="Sections" hidden"#));
    assert!(html.contains(concat!(
        r#"<a class="account" href="/account">"#,
        r#"<span class="account-glyph" aria-hidden="true">a</span>"#,
        r#"<span class="account-label">ada&lt;script&gt;</span></a>"#
    )));
    assert!(!html.contains("<script>"));
    assert_eq!(
        page.connect_sources(),
        ["'self'", "wss://relay.example:8443"]
    );
    assert_eq!(
        page.content_security_policy(),
        DEFAULT_POLICY.replace(
            "connect-src 'self';",
            "connect-src 'self' wss://relay.example:8443;"
        )
    );
}

#[test]
fn unusable_options_are_refused_when_the_page_is_built() {
    // Arrange
    let with = |change: fn(&mut WebClientOptions)| {
        let mut options = full_options();
        change(&mut options);

        WebClientPage::new(&options).map(|_| ())
    };

    // Act
    let results = [
        with(|options| options.relay_url = Some("https://relay.example".to_string())),
        with(|options| options.directory_path = Some("//other.example/list".to_string())),
        with(|options| options.sections[0].href = "https://other.example/".to_string()),
        with(|options| options.sections[1].label = "  ".to_string()),
        with(|options| options.sections[1].badge = Some(String::new())),
        with(|options| options.brand_badge = Some(" ".to_string())),
        with(|options| {
            options.account = Some(WebClientLink {
                label: "ada".to_string(),
                href: "javascript:alert(1)".to_string(),
            });
        }),
        with(|options| {
            options.account = Some(WebClientLink {
                label: String::new(),
                href: "/account".to_string(),
            });
        }),
    ];

    // Assert
    assert_eq!(
        results,
        [
            Err(WebClientError::RelayUrl),
            Err(WebClientError::Path("//other.example/list".to_string())),
            Err(WebClientError::Path("https://other.example/".to_string())),
            Err(WebClientError::EmptyLabel),
            Err(WebClientError::EmptyLabel),
            Err(WebClientError::EmptyLabel),
            Err(WebClientError::Path("javascript:alert(1)".to_string())),
            Err(WebClientError::EmptyLabel),
        ]
    );
}

#[test]
fn relay_origin_accepts_websocket_addresses_only() {
    // Arrange
    let addresses = [
        "ws://127.0.0.1:8787",
        "wss://[::1]:8443/relay/",
        "wss://RELAY.example",
        "http://relay.example",
        "wss://",
        "wss://user@relay.example",
        "wss://relay example",
        "wss://relay.example/?token=1",
        "wss://relay.example/#x",
        "wss://relay.example/a\\b",
        "wss://relay.example/a b",
        "relay.example",
        "wss://:8443",
        "wss://relay.example:",
        "wss://relay.example:84a3",
        "wss://[::1",
        "wss://[::1]8443",
        "wss://[]:8443",
        "wss://[relay.example]",
        "wss://relay_host.example",
    ];

    // Act
    let origins = addresses.map(|address| relay_origin(address).ok());

    // Assert
    let mut expected = vec![
        Some("ws://127.0.0.1:8787".to_string()),
        Some("wss://[::1]:8443".to_string()),
        Some("wss://relay.example".to_string()),
    ];
    expected.resize(addresses.len(), None);
    assert_eq!(origins.to_vec(), expected);
}

#[test]
fn same_origin_path_accepts_paths_on_the_pages_origin_only() {
    // Arrange
    let paths = [
        "/",
        "/sessions/?view=all#top",
        "//other.example/",
        "/\\other.example/",
        "https://other.example/",
        "relative",
        "",
        "/a b",
        "/a\u{7}",
        "/a\u{a0}b",
    ];

    // Act
    let accepted = paths.map(|path| same_origin_path(path).is_ok());

    // Assert
    assert_eq!(
        accepted,
        [
            true, true, false, false, false, false, false, false, false, false
        ]
    );
}

#[test]
fn replace_elements_replaces_each_element_and_keeps_an_unclosed_one() {
    // Arrange
    let html = "a<x>1</x>b<x>2\n</x>c<x>3";

    // Act
    let replaced = replace_elements(html, ("<x>", "</x>"), "Y");

    // Assert
    assert_eq!(replaced, "aYbYc<x>3");
}

#[test]
fn web_client_files_name_each_file_once_relative_to_the_page() {
    // Arrange
    let files = web_client_files();

    // Act
    let paths: BTreeSet<&str> = files.iter().map(|file| file.path).collect();

    // Assert
    assert_eq!(paths.len(), files.len());
    assert!(paths.contains("app.css"));
    assert!(paths.contains("app.js"));
    assert!(paths.contains("theme.js"));
    assert!(files.iter().all(|file| !file.path.starts_with('/')
        && !file.body.is_empty()
        && file.content_type.ends_with("; charset=utf-8")));
}
