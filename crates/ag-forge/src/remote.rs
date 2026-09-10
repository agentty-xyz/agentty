//! Forge remote detection helpers shared across provider adapters.

use super::{
    ForgeKind, ForgeRemote, GitHubReviewRequestAdapter, GitLabReviewRequestAdapter,
    ReviewRequestError,
};

/// Detects one supported forge remote from `repo_url`.
///
/// # Errors
/// Returns [`ReviewRequestError::UnsupportedRemote`] when the repository
/// remote does not map to a supported forge.
pub fn detect_remote(repo_url: &str) -> Result<ForgeRemote, ReviewRequestError> {
    if let Some(remote) = GitHubReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    if let Some(remote) = GitLabReviewRequestAdapter::detect_remote(repo_url) {
        return Ok(remote);
    }

    Err(ReviewRequestError::UnsupportedRemote {
        repo_url: display_safe_remote_url(repo_url),
    })
}

/// Parsed remote components extracted from one git remote URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedRemote {
    /// Canonical forge host used for browser and API requests.
    ///
    /// SSH transport ports are stripped so review-request commands target the
    /// authenticated HTTPS host instead of the SSH daemon port.
    pub(crate) host: String,
    /// Repository namespace or owner path.
    pub(crate) namespace: String,
    /// Repository name without a trailing `.git` suffix.
    pub(crate) project: String,
    /// Credential-free remote URL suitable for display and diagnostics.
    pub(crate) repo_url: String,
    /// Browser-openable repository URL derived from the remote.
    pub(crate) web_url: String,
}

impl ParsedRemote {
    /// Converts the parsed remote into one supported forge remote.
    pub(crate) fn into_forge_remote(self, forge_kind: ForgeKind) -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind,
            host: self.host,
            namespace: self.namespace,
            project: self.project,
            repo_url: self.repo_url,
            web_url: self.web_url,
        }
    }

    /// Builds one parsed remote from extracted host and path components.
    ///
    /// When `strip_transport_port` is `true`, the parsed host is normalized for
    /// browser and API access by dropping any SSH transport port.
    fn from_parts(
        repo_url: &str,
        host: &str,
        path: &str,
        strip_transport_port: bool,
    ) -> Option<ParsedRemote> {
        let host = host.trim().trim_matches('/').to_ascii_lowercase();
        let host = if strip_transport_port {
            strip_port(&host).to_string()
        } else {
            host
        };
        let path = path.trim().trim_matches('/').trim_end_matches(".git");
        if host.is_empty() || path.is_empty() {
            return None;
        }

        let (namespace, project) = path.rsplit_once('/')?;
        if namespace.is_empty() || project.is_empty() {
            return None;
        }

        Some(Self {
            host: host.clone(),
            namespace: namespace.to_string(),
            project: project.to_string(),
            repo_url: display_safe_remote_url(repo_url),
            web_url: format!("https://{host}/{path}"),
        })
    }
}

/// Parses a git remote URL into normalized hostname and repository components.
///
/// URL remotes may include `username[:password]@` userinfo, which is removed
/// before the remote is retained or used for diagnostics.
pub(crate) fn parse_remote_url(repo_url: &str) -> Option<ParsedRemote> {
    let trimmed_url = repo_url.trim().trim_end_matches('/');
    if trimmed_url.is_empty() {
        return None;
    }

    if let Some((authority, path)) = trimmed_url.split_once(':')
        && authority.contains('@')
    {
        let host = strip_userinfo(authority);

        return ParsedRemote::from_parts(trimmed_url, host, path, true);
    }

    let (scheme, scheme_rest) = trimmed_url.split_once("://")?;
    let scheme_rest = scheme_rest.strip_prefix("git@").unwrap_or(scheme_rest);
    let (authority, path) = scheme_rest.split_once('/')?;
    let host = strip_userinfo(authority);
    let strip_transport_port = scheme.eq_ignore_ascii_case("ssh");

    ParsedRemote::from_parts(trimmed_url, host, path, strip_transport_port)
}

/// Removes any `:port` suffix from `host`.
pub(crate) fn strip_port(host: &str) -> &str {
    host.split(':').next().unwrap_or(host)
}

/// Removes URL userinfo so repository remotes are safe to retain or display.
fn display_safe_remote_url(repo_url: &str) -> String {
    let trimmed_url = repo_url.trim();
    let Some((scheme, scheme_rest)) = trimmed_url.split_once("://") else {
        if let Some((authority, suffix)) = trimmed_url.split_once(':')
            && authority.contains('@')
        {
            return format!("{}:{suffix}", strip_userinfo(authority));
        }

        return trimmed_url.to_string();
    };
    let (authority, suffix) = scheme_rest
        .split_once('/')
        .map_or((scheme_rest, ""), |(authority, path)| (authority, path));
    let authority = strip_userinfo(authority);
    if suffix.is_empty() {
        return format!("{scheme}://{authority}");
    }

    format!("{scheme}://{authority}/{suffix}")
}

/// Removes any `username[:password]@` prefix from one URL authority segment.
fn strip_userinfo(authority: &str) -> &str {
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host)
}

#[cfg(test)]
#[path = "remote_test.rs"]
mod tests;
