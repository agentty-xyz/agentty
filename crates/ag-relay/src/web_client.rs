//! The remote-control web client as files and a rendered page, for the
//! relay and for another web application that serves the client itself;
//! see [`WebClientPage`].

/// Content type of the client's scripts.
const SCRIPT: &str = "text/javascript; charset=utf-8";
/// Marker in the page that tells the client which transport to use.
const LOCAL_TRANSPORT: &str = r#"<meta name="agentty-transport" content="local">"#;
/// Marker that selects the relay transport.
const RELAY_TRANSPORT: &str = r#"<meta name="agentty-transport" content="relay">"#;
/// The brand badge as the page names it.
const BRAND_BADGE: &str = r#"<span class="badge">remote</span>"#;
/// Where the page's own sections go in the sidebar and on the pairing
/// card: each such element, up to its closing tag.
const SECTIONS: (&str, &str) = (
    r#"<nav class="sections" aria-label="Sections" hidden>"#,
    "</nav>",
);
/// Where the account link goes in the sidebar foot.
const ACCOUNT: (&str, &str) = (r#"<span class="account" hidden>"#, "</span>");
/// Every directive of the content security policy but `connect-src`.
/// Fonts may only be the `data:` URIs the stylesheet embeds.
const POLICY_BEFORE_CONNECT: &str =
    "default-src 'none'; script-src 'self'; style-src 'self'; font-src data:";
/// The directives after `connect-src`.
const POLICY_AFTER_CONNECT: &str =
    "img-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// The page as the client's files hold it.
const INDEX: &str = include_str!("../../ag-remote/assets/index.html");

/// One file the page loads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WebClientFile {
    /// Path relative to the page, such as `app.js`.
    pub path: &'static str,
    /// Value of the `Content-Type` header.
    pub content_type: &'static str,
    /// The file's content.
    pub body: &'static str,
}

/// One script of the web client, served at its file name.
macro_rules! script {
    ($file:literal) => {
        WebClientFile {
            path: $file,
            content_type: SCRIPT,
            body: include_str!(concat!("../../ag-remote/assets/", $file)),
        }
    };
}

/// Files the page loads.
const FILES: [WebClientFile; 24] = [
    // The generated design tokens, font faces, and shared base styles come
    // first, as Agentty's listener serves them.
    WebClientFile {
        path: "app.css",
        content_type: "text/css; charset=utf-8",
        body: concat!(
            include_str!("../../ag-remote/assets/tokens.css"),
            include_str!("../../ag-remote/assets/fonts.css"),
            include_str!("../../ag-remote/assets/base.css"),
            include_str!("../../ag-remote/assets/app.css")
        ),
    },
    script!("app.js"),
    script!("chat.mjs"),
    script!("composer.mjs"),
    script!("connection.mjs"),
    script!("device_key.mjs"),
    script!("diff.mjs"),
    script!("directory.mjs"),
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

/// Returns every file the page loads, other than the page itself. The page
/// names each by its path relative to the page, so serve them beside it.
pub fn web_client_files() -> &'static [WebClientFile] {
    &FILES
}

/// How a program that serves the page sets it up. The default is the
/// relay's own page.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WebClientOptions {
    /// Address of the relay, `wss://` or `ws://` with a host and an
    /// optional path, when it is not the page's own origin. The page then
    /// opens its WebSocket connections there, accepts pairing links that name
    /// this relay or none, and refuses links that name another.
    pub relay_url: Option<String>,
    /// Path on the page's origin of an instance directory, such as
    /// `/api/instances`. The page reads it with the browser's credentials
    /// and lists the Agenttys it names that this browser has not paired,
    /// each with how to pair it. It answers
    /// `{"instances": [{"instance_id": "<64 hex digits>", "name": "<label>",
    /// "last_connected_at": "<RFC 3339>" | null}]}`, or `401` when the
    /// page is no longer signed in, which the page then says. Being listed
    /// grants nothing: pairing still needs the link and the approval in
    /// the Agentty.
    pub directory_path: Option<String>,
    /// Text of the badge beside the brand, in place of `remote`.
    pub brand_badge: Option<String>,
    /// Links to the program's own sections, in order, shown above the
    /// session tools.
    pub sections: Vec<WebClientSection>,
    /// Link to the signed-in account, shown in the sidebar foot.
    pub account: Option<WebClientLink>,
}

/// One of [`WebClientOptions::sections`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebClientSection {
    /// Text of the link.
    pub label: String,
    /// Path on the page's origin, such as `/dashboard`.
    pub href: String,
    /// Whether this section is the one the page belongs to.
    pub current: bool,
    /// Short text shown at the end of the link, such as a count.
    pub badge: Option<String>,
}

/// A link with a label, as [`WebClientOptions::account`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebClientLink {
    /// Text of the link.
    pub label: String,
    /// Path on the page's origin.
    pub href: String,
}

/// Options [`WebClientPage::new`] cannot render.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum WebClientError {
    /// [`WebClientOptions::relay_url`] is not a `ws://` or `wss://`
    /// address with a host, without credentials, a query, or a fragment.
    #[error("The relay address must be a ws:// or wss:// URL with a host")]
    RelayUrl,
    /// A link or the directory is not a path on the page's origin.
    #[error("`{0}` is not a path on the page's origin")]
    Path(String),
    /// A label or a badge is empty.
    #[error("Labels and badges must not be empty")]
    EmptyLabel,
}

/// The page rendered for a set of [`WebClientOptions`], and the content
/// security policy it needs.
///
/// The page reaches Agentty instances through a relay, encrypted end to
/// end, exactly as on the relay's own page. A program that serves it on
/// another origin passes the relay's address, an optional instance
/// directory on its own origin, and its own navigation. It serves
/// [`Self::html`] at the page's address and each of [`web_client_files`]
/// at its path relative to the page, every response with its content type
/// and the headers [`Self::headers`] lists, which include the content
/// security policy the page needs. Pairing links that Agentty shows open
/// the relay's own page, so such a page takes a pasted link as well.
///
/// ```
/// use ag_relay::{
///     WebClientLink, WebClientOptions, WebClientPage, WebClientSection, web_client_files,
/// };
///
/// let options = WebClientOptions {
///     relay_url: Some("wss://relay.example".to_string()),
///     directory_path: Some("/api/instances".to_string()),
///     brand_badge: Some("cloud".to_string()),
///     sections: vec![WebClientSection {
///         label: "Sessions".to_string(),
///         href: "/sessions/".to_string(),
///         current: true,
///         badge: Some("2".to_string()),
///     }],
///     account: Some(WebClientLink {
///         label: "ada".to_string(),
///         href: "/account".to_string(),
///     }),
/// };
/// let page = WebClientPage::new(&options)?;
///
/// assert!(page.html().contains(r#"<meta name="agentty-relay" content="wss://relay.example">"#));
/// assert_eq!(page.connect_sources(), ["'self'", "wss://relay.example"]);
/// assert!(page.content_security_policy().contains("connect-src 'self' wss://relay.example;"));
/// assert!(web_client_files().iter().any(|file| file.path == "app.js"));
/// # Ok::<(), ag_relay::WebClientError>(())
/// ```
///
/// Whoever serves the page controls the code it runs, so its page is as
/// trusted as a relay's own page: it pairs only through the links an
/// Agentty shows, and every Agentty approves each device itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebClientPage {
    connect_sources: Vec<String>,
    content_security_policy: String,
    html: String,
}

impl WebClientPage {
    /// Renders the page for `options`. Labels and badges are escaped, so
    /// they may hold any text.
    ///
    /// # Errors
    ///
    /// Returns [`WebClientError::RelayUrl`] for an unusable relay address,
    /// [`WebClientError::Path`] for a directory or link that is not a path
    /// on the page's origin, such as `https://other.example/` or
    /// `//other.example/`, and [`WebClientError::EmptyLabel`] for an
    /// empty label or badge.
    pub fn new(options: &WebClientOptions) -> Result<Self, WebClientError> {
        let mut connect_sources = vec!["'self'".to_string()];
        let mut meta = vec![RELAY_TRANSPORT.to_string()];
        if let Some(relay_url) = &options.relay_url {
            connect_sources.push(relay_origin(relay_url)?);
            meta.push(format!(
                r#"<meta name="agentty-relay" content="{}">"#,
                escape(relay_url.trim_end_matches('/'))
            ));
        }
        if let Some(path) = &options.directory_path {
            meta.push(format!(
                r#"<meta name="agentty-directory" content="{}">"#,
                escape(same_origin_path(path)?)
            ));
        }
        let mut html = INDEX.replacen(LOCAL_TRANSPORT, &meta.concat(), 1);
        if let Some(badge) = &options.brand_badge {
            let badge = format!(r#"<span class="badge">{}</span>"#, escape(label(badge)?));
            html = html.replace(BRAND_BADGE, &badge);
        }
        if !options.sections.is_empty() {
            html = replace_elements(&html, SECTIONS, &sections(&options.sections)?);
        }
        if let Some(account) = &options.account {
            html = replace_elements(&html, ACCOUNT, &account_link(account)?);
        }

        Ok(Self::assemble(connect_sources, html))
    }

    /// The page `html` with the policy that lets it reach
    /// `connect_sources`.
    fn assemble(connect_sources: Vec<String>, html: String) -> Self {
        let content_security_policy = format!(
            "{POLICY_BEFORE_CONNECT}; connect-src {}; {POLICY_AFTER_CONNECT}",
            connect_sources.join(" ")
        );

        Self {
            connect_sources,
            content_security_policy,
            html,
        }
    }

    /// The page, to serve with the content type `text/html; charset=utf-8`.
    pub fn html(&self) -> &str {
        &self.html
    }

    /// The content security policy to send with the page and every file:
    /// scripts and styles only from the page's origin, fonts only as the
    /// stylesheet's `data:` URIs, and connections only to the page's origin
    /// and the relay.
    pub fn content_security_policy(&self) -> &str {
        &self.content_security_policy
    }

    /// The sources the page connects to, as `connect-src` lists them: the
    /// page's origin, for the directory, and the relay's origin when it is
    /// another. A program that builds its own policy needs at least these.
    pub fn connect_sources(&self) -> &[String] {
        &self.connect_sources
    }

    /// The headers to send with the page and with every file, besides its
    /// content type: the content security policy, no content type
    /// sniffing, no referrer, and no caching, so a page never runs with
    /// scripts of another version.
    pub fn headers(&self) -> [(&'static str, &str); 4] {
        [
            ("content-security-policy", &self.content_security_policy),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ]
    }
}

/// The relay's own page, which the default options render.
impl Default for WebClientPage {
    fn default() -> Self {
        Self::assemble(
            vec!["'self'".to_string()],
            INDEX.replacen(LOCAL_TRANSPORT, RELAY_TRANSPORT, 1),
        )
    }
}

/// The origin `relay_url` connects to, as a `connect-src` source.
fn relay_origin(relay_url: &str) -> Result<String, WebClientError> {
    let (scheme, rest) = relay_url
        .split_once("://")
        .filter(|(scheme, _)| matches!(*scheme, "ws" | "wss"))
        .ok_or(WebClientError::RelayUrl)?;
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let is_path = path
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#' | b'\\'));
    if !is_authority(authority) || !is_path {
        return Err(WebClientError::RelayUrl);
    }

    Ok(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// Whether `authority` is a host, a name or an address in brackets, with
/// an optional port of digits.
fn is_authority(authority: &str) -> bool {
    // `port` is empty or `:` and the port.
    let (is_host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((address, rest)) = bracketed.split_once(']') else {
            return false;
        };
        (address.parse::<std::net::Ipv6Addr>().is_ok(), rest)
    } else {
        let (name, rest) = authority.split_at(authority.find(':').unwrap_or(authority.len()));
        let is_name = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
        (is_name, rest)
    };
    let is_port = port.is_empty()
        || port.strip_prefix(':').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        });

    is_host && is_port
}

/// Returns `path` when it is a path on the page's origin: it starts with
/// one `/`, and holds no backslash, whitespace, or control character, so a
/// browser cannot read it as another origin.
fn same_origin_path(path: &str) -> Result<&str, WebClientError> {
    let bytes = path.as_bytes();
    let is_path = bytes.first() == Some(&b'/')
        && bytes.get(1).is_none_or(|byte| *byte != b'/')
        && path.chars().all(|character| {
            !character.is_whitespace() && !character.is_control() && character != '\\'
        });
    if !is_path {
        return Err(WebClientError::Path(path.to_string()));
    }

    Ok(path)
}

/// Returns `text` when it holds something other than whitespace.
fn label(text: &str) -> Result<&str, WebClientError> {
    if text.trim().is_empty() {
        return Err(WebClientError::EmptyLabel);
    }

    Ok(text)
}

/// The sections as a list of links.
fn sections(sections: &[WebClientSection]) -> Result<String, WebClientError> {
    let items = sections
        .iter()
        .map(section_item)
        .collect::<Result<Vec<_>, _>>()?
        .concat();

    Ok(format!(
        r#"<nav class="sections" aria-label="Sections"><ul>{items}</ul></nav>"#
    ))
}

/// One section as a list item holding its link.
fn section_item(section: &WebClientSection) -> Result<String, WebClientError> {
    let current = if section.current {
        r#" aria-current="page""#
    } else {
        ""
    };
    let badge = match &section.badge {
        Some(badge) => format!(r#"<span class="badge">{}</span>"#, escape(label(badge)?)),
        None => String::new(),
    };

    Ok(format!(
        r#"<li><a href="{}"{current}>{}{badge}</a></li>"#,
        escape(same_origin_path(&section.href)?),
        escape(label(&section.label)?),
    ))
}

/// The account link: a ring with the label's first letter, then the label.
fn account_link(account: &WebClientLink) -> Result<String, WebClientError> {
    let text = label(&account.label)?.trim();
    let initial: String = text.chars().take(1).collect();

    Ok(format!(
        r#"<a class="account" href="{}"><span class="account-glyph" aria-hidden="true">{}</span><span class="account-label">{}</span></a>"#,
        escape(same_origin_path(&account.href)?),
        escape(&initial),
        escape(text),
    ))
}

/// Replaces each element of `html` that starts with `open` and ends at the
/// next `close` with `replacement`.
fn replace_elements(html: &str, (open, close): (&str, &str), replacement: &str) -> String {
    let mut replaced = String::with_capacity(html.len() + replacement.len());
    let mut rest = html;
    while let Some(start) = rest.find(open) {
        let Some(length) = rest[start..].find(close) else {
            break;
        };
        replaced.push_str(&rest[..start]);
        replaced.push_str(replacement);
        rest = &rest[start + length + close.len()..];
    }
    replaced.push_str(rest);

    replaced
}

/// Escapes `text` for HTML text and attribute values.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }

    escaped
}

#[cfg(test)]
#[path = "web_client_test.rs"]
mod tests;
