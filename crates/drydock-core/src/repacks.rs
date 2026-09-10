//! Per-app "repacks": external download sources the proxy relays from `Files/repacks.json`.
//!
//! Unlike unlocks and fixes, a repack is not a file Drydock installs — it is one or more http(s)
//! links to a repacker's site. The desktop app lists them in the Repacks tab and opens the chosen
//! link in the user's browser. The proxy already drops non-http(s) links, and [`open_link`] refuses
//! them again here, so a malformed or hostile entry can never open a dangerous URI (`file:`,
//! `javascript:`, a `steam:` action, …).

use thiserror::Error;

/// One external download source for an app (e.g. a specific repacker's page).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepackSource {
    pub repacker: String,
    pub link: String,
}

/// An app that has one or more repack download sources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepackApp {
    pub app_id: u32,
    pub sources: Vec<RepackSource>,
}

/// Opens an external repack link in the user's default browser, but only when it is a plain
/// `http`/`https` URL. Any other scheme is rejected instead of handed to the OS opener.
pub fn open_link(url: &str) -> Result<(), LinkError> {
    if !is_http_url(url) {
        return Err(LinkError::UnsupportedScheme);
    }
    open::that(url).map_err(LinkError::Open)
}

/// Whether `url` is a plain http(s) URL. Scheme matching is case-insensitive per RFC 3986; the
/// authority must be non-empty so `http://` alone is rejected.
#[must_use]
pub fn is_http_url(url: &str) -> bool {
    let rest = strip_prefix_ignore_ascii_case(url, "https://")
        .or_else(|| strip_prefix_ignore_ascii_case(url, "http://"));
    rest.is_some_and(|authority| !authority.is_empty() && !authority.starts_with('/'))
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    (value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

#[derive(Debug, Error)]
pub enum LinkError {
    #[error("Only http and https links can be opened")]
    UnsupportedScheme,
    #[error("The link could not be opened: {0}")]
    Open(std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_http_and_https() {
        assert!(is_http_url("https://fitgirl-repacks.site/game"));
        assert!(is_http_url("http://example.com/x"));
        assert!(is_http_url("HTTPS://EXAMPLE.COM"));
    }

    #[test]
    fn rejects_dangerous_or_malformed_schemes() {
        assert!(!is_http_url("file:///etc/passwd"));
        assert!(!is_http_url("javascript:alert(1)"));
        assert!(!is_http_url("steam://run/730"));
        assert!(!is_http_url("ftp://example.com"));
        assert!(!is_http_url("https://"));
        assert!(!is_http_url("http:///no-host"));
        assert!(!is_http_url(""));
    }

    #[test]
    fn open_link_refuses_non_http() {
        assert!(matches!(
            open_link("steam://run/730"),
            Err(LinkError::UnsupportedScheme)
        ));
    }
}
