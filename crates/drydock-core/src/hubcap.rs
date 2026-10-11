//! Hubcap Manifest (hubcapmanifest.com) with the user's own API key.
//!
//! Hubcap's manifest downloads are capped per day and per key, so it is not a provider of the proxy:
//! whoever wants it brings a key, and the app talks to Hubcap directly. The key is kept beside the
//! settings, sealed with DPAPI on Windows (readable only by this Windows user on this machine) and
//! readable only by the user elsewhere; it never goes to the proxy.
//!
//! Endpoints used:
//!   GET /api/v1/status/{appid}           -> whether it has the app, and whether that is current (free)
//!   GET /api/v1/manifest/{appid}         -> application/zip (`<appid>.lua` + `<depot>_<gid>.manifest`)
//!   GET /api/v1/generate/usage           -> what is left of today's downloads (free)
//!   GET /api/v1/library?limit=&offset=   -> every app it has, for the catalogue (free)

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::{AUTHORIZATION, USER_AGENT};
use serde::Deserialize;
use thiserror::Error;

use crate::version::user_agent;

const BASE_URL: &str = "https://hubcapmanifest.com";
const KEY_FILE: &str = "hubcap-key.dat";
/// A depot package can be tens of megabytes; nothing Hubcap sends is legitimately larger.
const MAXIMUM_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAXIMUM_JSON_BYTES: u64 = 64 * 1024 * 1024;
/// Entries per library page (the API answers 20k in about two seconds).
const LIBRARY_PAGE: usize = 20_000;
/// Stops a library walk that would otherwise never end if the API ignored `offset`.
const MAXIMUM_LIBRARY_PAGES: usize = 200;

#[derive(Debug, Error)]
pub enum HubcapError {
    #[error("Hubcap Manifest did not accept the API key — check it in Settings")]
    Unauthorized,
    #[error("Hubcap Manifest has no manifests for App {0}")]
    NotFound(u32),
    #[error("today's Hubcap Manifest downloads are used up — try again tomorrow or pick another source")]
    LimitReached,
    #[error("Hubcap Manifest answered {0}")]
    Status(u16),
    #[error("Hubcap Manifest sent more than expected")]
    TooLarge,
    #[error("Hubcap Manifest sent something unreadable: {0}")]
    Invalid(String),
    #[error("stopped")]
    Cancelled,
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What Hubcap says about one app.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HubcapStatus {
    pub available: bool,
    /// When its manifests were last refreshed, in Unix seconds.
    pub updated: Option<u64>,
    /// Hubcap's own judgement that its manifests are older than the game's current build.
    pub needs_update: bool,
}

/// What is left of today's manifest downloads for the key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HubcapUsage {
    pub remaining: u64,
    pub limit: u64,
}

#[derive(Deserialize)]
struct StatusDto {
    #[serde(default)]
    status: String,
    #[serde(default)]
    manifest_file_exists: bool,
    #[serde(default)]
    file_modified: Option<String>,
    #[serde(default)]
    needs_update: bool,
}

#[derive(Deserialize)]
struct UsageDto {
    single: UsageBucket,
}

#[derive(Deserialize)]
struct UsageBucket {
    #[serde(default)]
    limit: u64,
    #[serde(default)]
    remaining: u64,
}

#[derive(Deserialize)]
struct LibraryDto {
    #[serde(default)]
    total_count: Option<u64>,
    #[serde(default)]
    games: Vec<LibraryGame>,
}

#[derive(Deserialize)]
struct LibraryGame {
    #[serde(default)]
    game_id: serde_json::Value,
    #[serde(default)]
    game_name: String,
    #[serde(default = "available_by_default")]
    manifest_available: bool,
}

const fn available_by_default() -> bool {
    true
}

/// Hubcap's timestamps, `2026-10-08T19:06:53.328658` in UTC, as Unix seconds.
fn unix_seconds(text: &str) -> Option<u64> {
    let civil: jiff::civil::DateTime = text.trim().trim_end_matches('Z').parse().ok()?;
    let seconds = civil
        .to_zoned(jiff::tz::TimeZone::UTC)
        .ok()?
        .timestamp()
        .as_second();
    u64::try_from(seconds).ok()
}

fn status_from(dto: StatusDto) -> HubcapStatus {
    HubcapStatus {
        available: dto.status == "available" && dto.manifest_file_exists,
        updated: dto.file_modified.as_deref().and_then(unix_seconds),
        needs_update: dto.needs_update,
    }
}

pub struct HubcapClient {
    key: String,
    http: Client,
}

impl HubcapClient {
    pub fn new(key: &str) -> Result<Self, HubcapError> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self {
            key: key.trim().to_owned(),
            http,
        })
    }

    fn get(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        self.http
            .get(format!("{BASE_URL}{path}"))
            .header(USER_AGENT, user_agent())
            .header(AUTHORIZATION, format!("Bearer {}", self.key))
    }

    fn checked(response: Response, app_id: Option<u32>) -> Result<Response, HubcapError> {
        match response.status() {
            status if status.is_success() => Ok(response),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(HubcapError::Unauthorized),
            StatusCode::NOT_FOUND => Err(app_id.map_or(HubcapError::Status(404), HubcapError::NotFound)),
            StatusCode::TOO_MANY_REQUESTS => Err(HubcapError::LimitReached),
            status => Err(HubcapError::Status(status.as_u16())),
        }
    }

    fn json<T: serde::de::DeserializeOwned>(response: Response) -> Result<T, HubcapError> {
        let bytes = read_capped(response, MAXIMUM_JSON_BYTES, &AtomicBool::new(false))?;
        serde_json::from_slice(&bytes).map_err(|error| HubcapError::Invalid(error.to_string()))
    }

    /// Whether Hubcap has `app_id`, and how current it is. Costs nothing of the daily allowance.
    pub fn status(&self, app_id: u32) -> Result<HubcapStatus, HubcapError> {
        let response = Self::checked(
            self.get(&format!("/api/v1/status/{app_id}")).send()?,
            Some(app_id),
        )?;
        Ok(status_from(Self::json(response)?))
    }

    /// What is left of today's manifest downloads — also how the settings check a key.
    pub fn usage(&self) -> Result<HubcapUsage, HubcapError> {
        let response = Self::checked(self.get("/api/v1/generate/usage").send()?, None)?;
        let usage: UsageDto = Self::json(response)?;
        Ok(HubcapUsage {
            remaining: usage.single.remaining,
            limit: usage.single.limit,
        })
    }

    /// The app's depot package (its Lua and depot manifests), stopping as soon as `cancel` is set.
    /// Spends one of the key's daily downloads unless Hubcap has it ready.
    pub fn package(&self, app_id: u32, cancel: &AtomicBool) -> Result<Vec<u8>, HubcapError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(HubcapError::Cancelled);
        }
        let response = self
            .get(&format!("/api/v1/manifest/{app_id}"))
            .timeout(Duration::from_secs(600))
            .send()?;
        let response = Self::checked(response, Some(app_id))?;
        let bytes = read_capped(response, MAXIMUM_PACKAGE_BYTES, cancel)?;
        if !bytes.starts_with(b"PK") {
            return Err(HubcapError::Invalid("not a ZIP archive".to_owned()));
        }
        Ok(bytes)
    }

    /// Every app Hubcap has manifests for, as `(App ID, name)`. Costs nothing of the allowance.
    pub fn library(&self) -> Result<Vec<(u32, String)>, HubcapError> {
        let mut apps = Vec::new();
        for page in 0..MAXIMUM_LIBRARY_PAGES {
            let offset = page * LIBRARY_PAGE;
            let response = self
                .get(&format!("/api/v1/library?limit={LIBRARY_PAGE}&offset={offset}"))
                .timeout(Duration::from_secs(120))
                .send()?;
            let library: LibraryDto = Self::json(Self::checked(response, None)?)?;
            let count = library.games.len();
            apps.extend(library.games.into_iter().filter_map(|game| {
                let id = match &game.game_id {
                    serde_json::Value::Number(number) => number.as_u64(),
                    serde_json::Value::String(text) => text.parse().ok(),
                    _ => None,
                }?;
                let id = u32::try_from(id).ok().filter(|id| *id > 0)?;
                (game.manifest_available && !game.game_name.trim().is_empty())
                    .then(|| (id, game.game_name.trim().to_owned()))
            }));
            let total = library.total_count.unwrap_or(0);
            if count < LIBRARY_PAGE || (total > 0 && (offset + count) as u64 >= total) {
                break;
            }
        }
        Ok(apps)
    }
}

fn read_capped(response: Response, limit: u64, cancel: &AtomicBool) -> Result<Vec<u8>, HubcapError> {
    if response.content_length().is_some_and(|length| length > limit) {
        return Err(HubcapError::TooLarge);
    }
    let mut reader = response.take(limit + 1);
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(HubcapError::Cancelled);
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    if bytes.len() as u64 > limit {
        return Err(HubcapError::TooLarge);
    }
    Ok(bytes)
}

/// Hubcap's library as cached on disk (`(App ID, name)`), empty when there is none.
#[must_use]
pub fn load_library(path: &Path) -> Vec<(u32, String)> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Caches Hubcap's library on disk.
pub fn save_library(path: &Path, apps: &[(u32, String)]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec(apps).map_err(io::Error::other)?)
}

/// Where the key is kept, beside the settings.
#[must_use]
pub fn key_path(settings_dir: &Path) -> PathBuf {
    settings_dir.join(KEY_FILE)
}

/// The saved key, if there is one this user can read.
#[must_use]
pub fn load_key(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    #[cfg(windows)]
    let bytes = crate::cloud::dpapi_unprotect(&bytes)?;
    let key = String::from_utf8(bytes).ok()?;
    let key = key.trim();
    (!key.is_empty()).then(|| key.to_owned())
}

/// Keeps the key: DPAPI-sealed on Windows, readable only by the user elsewhere.
pub fn save_key(path: &Path, key: &str) -> io::Result<()> {
    let key = key.trim().as_bytes().to_vec();
    #[cfg(windows)]
    let key =
        crate::cloud::dpapi_protect(&key).ok_or_else(|| io::Error::other("the key could not be sealed"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("dat.tmp");
    fs::write(&temporary, key)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&temporary, path)
}

/// Forgets the saved key.
pub fn forget_key(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hubcap_status_reads_as_available_dated_and_current() {
        let available: StatusDto = serde_json::from_str(
            r#"{"app_id":"730","game_name":"Counter-Strike 2","status":"available","manifest_file_exists":true,
                "file_modified":"2026-10-08T19:06:53.328658","needs_update":false,"update_reason":"manifest_current"}"#,
        )
        .unwrap();
        let status = status_from(available);
        assert!(status.available && !status.needs_update);
        assert_eq!(status.updated, Some(1_791_486_413));

        let missing: StatusDto = serde_json::from_str(
            r#"{"app_id":"999999999","status":"not_found","message":"Game not found in library"}"#,
        )
        .unwrap();
        assert_eq!(
            status_from(missing),
            HubcapStatus {
                available: false,
                updated: None,
                needs_update: false
            }
        );
    }

    #[test]
    fn the_key_is_kept_and_forgotten() {
        let directory = tempfile::tempdir().unwrap();
        let path = key_path(directory.path());
        assert_eq!(load_key(&path), None);
        save_key(&path, "  hc_example_key \n").unwrap();
        assert_eq!(load_key(&path).as_deref(), Some("hc_example_key"));
        #[cfg(windows)]
        assert!(
            !fs::read(&path)
                .unwrap()
                .windows(14)
                .any(|window| window == b"hc_example_key"),
            "the key is not on disk in the clear"
        );
        forget_key(&path);
        assert_eq!(load_key(&path), None);
    }
}
