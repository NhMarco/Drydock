//! Client for the "Denuvo Watch" Steam curator (id 26095454) — the authoritative, always-current
//! list of games that *actively* use Denuvo Anti-Tamper.
//!
//! The curator tags currently-protected titles "Not Recommended" (`color_not`); games that later
//! have Denuvo removed are re-tagged "Informational" (`color_informational`) and are therefore
//! excluded here. One request returns the whole list, so the activation-needed set is known
//! up front instead of probing tens of thousands of apps individually.

use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde::Deserialize;
use thiserror::Error;

use crate::version::user_agent;

/// The curator's paginated recommendation feed. `count` is set well above the current list size
/// (~250 active titles) so the whole list arrives in a single response.
const CURATOR_URL: &str = "https://store.steampowered.com/curator/26095454/ajaxgetfilteredrecommendations/render/?query=&start=0&count=1000&dynamic_data=&tagids=&sort=recent&app_types=&curations=&reset=false";
const MAXIMUM_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Default, Deserialize)]
struct CuratorResponse {
    #[serde(default)]
    results_html: String,
}

pub struct DenuvoWatchClient {
    client: Client,
}

impl DenuvoWatchClient {
    pub fn new() -> Result<Self, DenuvoError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }

    /// Fetches the App IDs of games that actively use Denuvo, sorted ascending and de-duplicated.
    pub fn fetch_active_appids(&self) -> Result<Vec<u32>, DenuvoError> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent())?);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        let response = self
            .client
            .get(CURATOR_URL)
            .headers(headers)
            .send()?
            .error_for_status()?;
        let mut bytes = Vec::new();
        response.take(MAXIMUM_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM_BYTES {
            return Err(DenuvoError::TooLarge);
        }
        let parsed: CuratorResponse = serde_json::from_slice(&bytes)?;
        let appids = parse_active_appids(&parsed.results_html);
        if appids.is_empty() {
            return Err(DenuvoError::Empty);
        }
        Ok(appids)
    }
}

/// Extracts the App IDs of "Not Recommended" (actively Denuvo-protected) games from the curator's
/// rendered HTML. Each recommendation block opens with `class="recommendation"` and carries one
/// `data-ds-appid` plus a rating class; only `color_not` blocks are active Denuvo.
fn parse_active_appids(html: &str) -> Vec<u32> {
    let mut appids = std::collections::BTreeSet::new();
    for block in html.split("class=\"recommendation\"").skip(1) {
        if block.contains("color_not")
            && let Some(app_id) = first_appid(block)
        {
            appids.insert(app_id);
        }
    }
    appids.into_iter().collect()
}

fn first_appid(block: &str) -> Option<u32> {
    const MARKER: &str = "data-ds-appid=\"";
    let start = block.find(MARKER)? + MARKER.len();
    let rest = &block[start..];
    let end = rest.find('"')?;
    // Bundle capsules can carry several comma-separated IDs; the first is the app itself.
    rest[..end].split(',').next()?.trim().parse().ok()
}

/// Reads the cached active-Denuvo App IDs if the cache is younger than `max_age`.
pub fn load_cached_denuvo_appids(path: &Path, max_age: Duration) -> Option<Vec<u32>> {
    let age = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())?;
    if age > max_age {
        return None;
    }
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Reads whatever active-Denuvo App IDs are cached, regardless of age (used as a fallback when a
/// fresh fetch fails).
pub fn read_denuvo_appids(path: &Path) -> Option<Vec<u32>> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Persists the active-Denuvo App IDs so later sessions start with the list already known.
pub fn save_denuvo_appids(path: &Path, appids: &[u32]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(appids)?)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum DenuvoError {
    #[error("The Denuvo Watch list was empty or could not be parsed")]
    Empty,
    #[error("The Denuvo Watch response exceeded the maximum allowed size")]
    TooLarge,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Header(#[from] reqwest::header::InvalidHeaderValue),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_active_denuvo_appids() {
        // Two "Not Recommended" (active) blocks and one "Informational" (Denuvo removed) block.
        let html = concat!(
            "<div class=\"recommendation\"><div class=\"store_capsule\" data-ds-appid=\"990080\">",
            "</div><div class=\"recommendation_type_ctn color_not\">Not Recommended</div></div>",
            "<div class=\"recommendation\"><div class=\"store_capsule\" data-ds-appid=\"1245620\">",
            "</div><div class=\"recommendation_type_ctn color_not\">Not Recommended</div></div>",
            "<div class=\"recommendation\"><div class=\"store_capsule\" data-ds-appid=\"1196590\">",
            "</div><div class=\"recommendation_type_ctn color_informational\">Informational</div></div>",
        );
        let appids = parse_active_appids(html);
        assert_eq!(appids, vec![990080, 1245620]);
    }

    #[test]
    fn ignores_blocks_without_an_app_id() {
        let html = "<div class=\"recommendation\"><div class=\"color_not\">no id here</div></div>";
        assert!(parse_active_appids(html).is_empty());
    }

    #[test]
    fn takes_the_first_id_of_a_bundle_capsule() {
        assert_eq!(first_appid("x data-ds-appid=\"111,222,333\" y"), Some(111));
    }
}
