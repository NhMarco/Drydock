//! Client for the self-hosted Drydock proxy.
//!
//! The proxy replaces both the direct Ryuu API and the direct GitHub access:
//!
//! * The catalog comes from `GET /v1/gamelist` (steamtools, cached + compacted by the proxy).
//! * Each app's unlock is a single `.lua` from `GET /v1/lua/{appid}` (steamtools, rate limited).
//! * The Steam Service (OST) payload comes from `GET /v1/service/manifest` + `/v1/service/file/{name}`.
//! * The per-app fixes come from `GET /v1/fixes` + `/v1/fixes/file/{name}`.
//!
//! No steamtools key or GitHub token ships in the app anymore. Instead every request is signed
//! with a shared HMAC secret embedded at build time (see [`build.rs`](../build.rs)); the proxy
//! rejects anything without a valid, fresh signature. Downloaded service/fix files are still
//! verified against the git-blob SHA the proxy advertised.

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hmac::{Hmac, Mac};
use rand::RngCore;
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::USER_AGENT;
use serde::Deserialize;
use sha2::Sha256;
use thiserror::Error;

use crate::catalog::CatalogApp;
use crate::mfb::{
    DenuvoFix, RepositoryFile, SteamServiceManifest, SteamServicePackage, matches_git_blob_sha,
};
use crate::repacks::{RepackApp, RepackSource, is_http_url};
use crate::version::user_agent;

type HmacSha256 = Hmac<Sha256>;

const MAXIMUM_CATALOG_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_LUA_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// Fix zip parts (game-folder payloads) can be far larger than Lua files (GitHub caps a single
/// tracked file at 100 MB).
const MAXIMUM_FIX_ZIP_BYTES: u64 = 100 * 1024 * 1024;
/// The depot package ZIP bundles every depot manifest plus the key-bearing `.lua` for one app.
const MAXIMUM_DEPOT_MANIFEST_ZIP_BYTES: u64 = 256 * 1024 * 1024;

pub struct ProxyClient {
    client: Client,
    base_url: String,
    secret: String,
}

impl ProxyClient {
    pub fn new() -> Result<Self, ProxyError> {
        let base_url = proxy_base_url().ok_or(ProxyError::MissingConfig)?;
        let secret = hmac_secret().ok_or(ProxyError::MissingConfig)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self {
            client,
            base_url,
            secret,
        })
    }

    /// Fetches and parses the catalog. The proxy already compacted the upstream gamelist to
    /// `{ appid, name, tags }` (NSFW filtered, tag prefixes stripped). Entries with a zero App ID
    /// or empty name are skipped; duplicate App IDs keep their first occurrence.
    pub fn fetch_catalog(&self) -> Result<Vec<CatalogApp>, ProxyError> {
        let response = self.send(self.signed_get("/v1/gamelist")?)?;
        let bytes = read_capped(response, MAXIMUM_CATALOG_BYTES)?;
        let list: GamelistDto = serde_json::from_slice(&bytes)?;

        let mut seen = std::collections::HashSet::new();
        let mut apps = Vec::with_capacity(list.games.len());
        for game in list.games {
            let name = game.name.trim();
            if game.appid == 0 || name.is_empty() || name.len() > 200 || !seen.insert(game.appid) {
                continue;
            }
            apps.push(CatalogApp {
                app_id: game.appid,
                name: name.to_owned(),
                // Type/DRM is not in the bulk gamelist; activation need is decided separately by
                // the Denuvo Watch list. The flag is kept only for cache-schema compatibility.
                drm: false,
                tags: game.tags,
            });
        }
        apps.sort_by_key(|app| app.name.to_lowercase());
        Ok(apps)
    }

    /// Downloads the single `.lua` unlock file for `app_id`.
    pub fn download_lua(&self, app_id: u32) -> Result<Vec<u8>, ProxyError> {
        let response = self.send(self.signed_get(&format!("/v1/lua/{app_id}"))?)?;
        let bytes = read_capped(response, MAXIMUM_LUA_BYTES)?;
        if bytes.is_empty() || !looks_like_lua(&bytes) {
            return Err(ProxyError::InvalidLua(app_id));
        }
        Ok(bytes)
    }

    /// Returns the manifest describing the current Steam Service files and version.
    pub fn steam_service_manifest(&self) -> Result<SteamServiceManifest, ProxyError> {
        let response = self.send(self.signed_get("/v1/service/manifest")?)?;
        let bytes = read_capped(response, MAXIMUM_MANIFEST_BYTES)?;
        let manifest: ServiceManifestDto = serde_json::from_slice(&bytes)?;
        Ok(SteamServiceManifest {
            version: manifest.version,
            files: manifest
                .files
                .into_iter()
                .map(|file| repository_file(file, "/v1/service/file/"))
                .collect(),
        })
    }

    /// Downloads and verifies every Steam Service file.
    pub fn steam_service_package(&self) -> Result<SteamServicePackage, ProxyError> {
        let manifest = self.steam_service_manifest()?;
        let mut files = std::collections::BTreeMap::new();
        for file in &manifest.files {
            files.insert(file.file_name().to_owned(), self.fetch_file(file)?);
        }
        Ok(SteamServicePackage {
            version: manifest.version,
            files,
        })
    }

    /// Downloads a single service/fix file and verifies it against its git-blob SHA.
    pub fn fetch_file(&self, file: &RepositoryFile) -> Result<Vec<u8>, ProxyError> {
        self.download_verified(file, MAXIMUM_FILE_BYTES)
    }

    /// Downloads and verifies a single Denuvo-fix zip part (larger size cap than Lua files).
    pub fn fetch_fix_part(&self, part: &RepositoryFile) -> Result<Vec<u8>, ProxyError> {
        self.download_verified(part, MAXIMUM_FIX_ZIP_BYTES)
    }

    /// Downloads the Ubisoft "magicfiles" ZIP for `app_id` (the DRM helper files Drydock places next
    /// to the game exe). A `404` means no magicfiles exist for that app, surfaced as
    /// [`ProxyError::MagicfilesUnavailable`] so the UI can say the game is not supported for Ubisoft
    /// activation rather than showing a hard error.
    pub fn magicfiles(&self, app_id: u32) -> Result<Vec<u8>, ProxyError> {
        let response = self.signed_get(&format!("/v1/magicfiles/{app_id}"))?.send()?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ProxyError::MagicfilesUnavailable(app_id));
        }
        let response = Self::check_status(response)?;
        read_capped(response, MAXIMUM_FIX_ZIP_BYTES)
    }

    /// Downloads the per-app depot package ZIP (all `<depot>_<manifest>.manifest` files plus the
    /// key-bearing `.lua`/`.key`) via the proxy. A `404` means no depot data exists for the app.
    pub fn depot_package(&self, app_id: u32) -> Result<Vec<u8>, ProxyError> {
        // The upstream packages the depot on demand, which can take minutes for a large title, so
        // override the client's default request timeout with a generous one.
        let response = self
            .signed_get(&format!("/v1/depot/package/{app_id}"))?
            .timeout(Duration::from_secs(600))
            .send()?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(ProxyError::DepotUnavailable(app_id));
        }
        let response = Self::check_status(response)?;
        read_capped(response, MAXIMUM_DEPOT_MANIFEST_ZIP_BYTES)
    }

    /// The app's achievement schema as a gbe_fork `achievements.json` array (text), for the local
    /// emulator-template generator. The Steam Web API key lives on the proxy; when it's unset the
    /// proxy returns `[]`, so this is always a valid JSON array.
    pub fn app_schema(&self, app_id: u32) -> Result<String, ProxyError> {
        let response = self.send(self.signed_get(&format!("/v1/app-schema/{app_id}"))?)?;
        let bytes = read_capped(response, MAXIMUM_MANIFEST_BYTES)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Resolves every app with a complete GitHub "Denuvo fix" (build-locked Lua + zip parts), as
    /// assembled by the proxy from the GitHub `Files/fix` directory. Keyed by App ID for merging.
    pub fn denuvo_fixes(&self) -> Result<Vec<(u32, DenuvoFix)>, ProxyError> {
        let response = self.send(self.signed_get("/v1/denuvo-fixes")?)?;
        let bytes = read_capped(response, MAXIMUM_MANIFEST_BYTES)?;
        let list: DenuvoFixesDto = serde_json::from_slice(&bytes)?;
        Ok(list
            .fixes
            .into_iter()
            .filter_map(|fix| {
                (fix.appid != 0).then_some((
                    fix.appid,
                    DenuvoFix {
                        lua: repository_file(fix.lua, "/v1/denuvo-fixes/file/"),
                        zip_parts: fix
                            .zip_parts
                            .into_iter()
                            .map(|part| repository_file(part, "/v1/denuvo-fixes/file/"))
                            .collect(),
                    },
                ))
            })
            .collect())
    }

    /// Resolves every app that has one or more external repack download sources, as assembled by
    /// the proxy from the GitHub `Files/repacks.json` file. Non-http(s) links are dropped as a
    /// second line of defence (the proxy already filters them), and apps left with no valid source
    /// are omitted, so the UI only ever offers openable links.
    pub fn repacks(&self) -> Result<Vec<RepackApp>, ProxyError> {
        let response = self.send(self.signed_get("/v1/repacks")?)?;
        let bytes = read_capped(response, MAXIMUM_MANIFEST_BYTES)?;
        let list: RepacksDto = serde_json::from_slice(&bytes)?;
        Ok(list
            .repacks
            .into_iter()
            .filter_map(|entry| {
                let sources: Vec<RepackSource> = entry
                    .sources
                    .into_iter()
                    .filter(|source| !source.repacker.trim().is_empty() && is_http_url(&source.link))
                    .map(|source| RepackSource {
                        repacker: source.repacker,
                        link: source.link,
                    })
                    .collect();
                (entry.appid != 0 && !sources.is_empty()).then_some(RepackApp {
                    app_id: entry.appid,
                    sources,
                })
            })
            .collect())
    }

    fn download_verified(&self, file: &RepositoryFile, limit: u64) -> Result<Vec<u8>, ProxyError> {
        // `source_url` holds the proxy-relative path (e.g. `/v1/service/file/hid.dll`), built from a
        // name the proxy supplied. Reject anything that would not survive URL encoding unchanged
        // before signing it — see `is_safe_file_name`.
        if !is_safe_file_name(file.file_name()) {
            return Err(ProxyError::UnsafeFileName(file.file_name().to_owned()));
        }
        let response = self.send(self.signed_get(&file.source_url)?)?;
        let bytes = read_capped(response, limit)?;
        if !matches_git_blob_sha(&bytes, &file.sha) {
            return Err(ProxyError::ContentChanged(file.file_name().to_owned()));
        }
        Ok(bytes)
    }

    /// Builds a signed GET request for a proxy-relative path (`/v1/…`). The signature covers the
    /// exact path the proxy receives, so a captured request cannot be replayed or retargeted.
    fn signed_get(&self, path: &str) -> Result<RequestBuilder, ProxyError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ProxyError::Clock)?
            .as_secs()
            .to_string();
        let nonce = random_nonce();
        let signing_string = format!("GET\n{path}\n{timestamp}\n{nonce}");
        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes()).map_err(|_| ProxyError::Signing)?;
        mac.update(signing_string.as_bytes());
        let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());

        Ok(self
            .client
            .get(format!("{}{path}", self.base_url))
            .header(USER_AGENT, user_agent())
            .header("X-Drydock-Timestamp", timestamp)
            .header("X-Drydock-Nonce", nonce)
            .header("X-Drydock-Signature", signature))
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, ProxyError> {
        let response = request.send()?;
        Self::check_status(response)
    }

    /// Maps a proxy response's status onto a diagnosable error.
    ///
    /// `401` gets its own variant because the two realistic causes are both invisible otherwise: the
    /// signature covers a timestamp the proxy checks against a tight window, so a system clock that
    /// is off by more than a few minutes rejects *every* request, and a rotated `DRYDOCK_HMAC_SECRET`
    /// looks identical. A bare "HTTP status client error (401 Unauthorized)" told the user neither.
    fn check_status(response: Response) -> Result<Response, ProxyError> {
        match response.status() {
            StatusCode::TOO_MANY_REQUESTS => Err(ProxyError::RateLimited),
            StatusCode::UNAUTHORIZED => Err(ProxyError::Unauthorized),
            _ => Ok(response.error_for_status()?),
        }
    }
}

/// Whether a proxy-supplied file name is safe to put in a request path.
///
/// The name goes straight into the URL that gets HMAC-signed. Anything reqwest would percent-encode
/// on the wire (spaces, non-ASCII) makes the path the proxy verifies differ from the one we signed,
/// so the request fails with an opaque `401` — and a name containing `/` or `..` would retarget the
/// request entirely. Restricting to characters that survive a URL round-trip unchanged avoids both.
fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
}

fn repository_file(file: FileDto, path_prefix: &str) -> RepositoryFile {
    RepositoryFile {
        source_url: format!("{path_prefix}{}", file.name),
        relative_path: file.name,
        sha: file.sha,
    }
}

fn read_capped(response: Response, limit: u64) -> Result<Vec<u8>, ProxyError> {
    if response.content_length().is_some_and(|length| length > limit) {
        return Err(ProxyError::TooLarge);
    }
    let mut bytes = Vec::new();
    response.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(ProxyError::TooLarge);
    }
    Ok(bytes)
}

fn random_nonce() -> String {
    let mut raw = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut raw);
    let mut hex = String::with_capacity(32);
    for byte in raw {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn looks_like_lua(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(256)];
    let text = String::from_utf8_lossy(prefix);
    text.contains("addappid") || text.contains("setManifestid")
}

/// Resolves the proxy base URL (origin only, e.g. `https://proxy.example`).
///
/// Thin re-export of [`crate::config::proxy_base_url`]; see that module for the resolution order
/// (environment → Settings → build-time default).
#[must_use]
pub fn proxy_base_url() -> Option<String> {
    crate::config::proxy_base_url()
}

/// Resolves the shared HMAC secret used to sign proxy requests.
///
/// Thin re-export of [`crate::config::hmac_secret`].
#[must_use]
pub fn hmac_secret() -> Option<String> {
    crate::config::hmac_secret()
}

#[derive(Debug, Deserialize)]
struct GamelistDto {
    #[serde(default)]
    games: Vec<GameDto>,
}

#[derive(Debug, Deserialize)]
struct GameDto {
    #[serde(default)]
    appid: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ServiceManifestDto {
    #[serde(default)]
    version: String,
    #[serde(default)]
    files: Vec<FileDto>,
}

#[derive(Debug, Deserialize)]
struct DenuvoFixesDto {
    #[serde(default)]
    fixes: Vec<DenuvoFixDto>,
}

#[derive(Debug, Deserialize)]
struct DenuvoFixDto {
    #[serde(default)]
    appid: u32,
    lua: FileDto,
    #[serde(default)]
    zip_parts: Vec<FileDto>,
}

#[derive(Debug, Deserialize)]
struct FileDto {
    #[serde(default)]
    name: String,
    #[serde(default)]
    sha: String,
}

#[derive(Debug, Deserialize)]
struct RepacksDto {
    #[serde(default)]
    repacks: Vec<RepackDto>,
}

#[derive(Debug, Deserialize)]
struct RepackDto {
    #[serde(default)]
    appid: u32,
    #[serde(default)]
    sources: Vec<RepackSourceDto>,
}

#[derive(Debug, Deserialize)]
struct RepackSourceDto {
    #[serde(default)]
    repacker: String,
    #[serde(default)]
    link: String,
}

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("The proxy address or access secret is not configured in this build")]
    MissingConfig,
    #[error("Too many requests — wait a minute before adding another game")]
    RateLimited,
    #[error(
        "The proxy rejected this build's credentials. Check that your system clock is correct (requests \
         are signed with a timestamp) — if it is, this Drydock build is too old and needs updating."
    )]
    Unauthorized,
    #[error("The proxy offered a file with an unusable name: {0}")]
    UnsafeFileName(String),
    #[error("A proxy response exceeded the maximum allowed size")]
    TooLarge,
    #[error("The unlock file for App {0} was empty or not a valid Lua script")]
    InvalidLua(u32),
    #[error("This game is not available for Ubisoft activation (no magicfiles for App {0})")]
    MagicfilesUnavailable(u32),
    #[error("No depot download data is available for App {0}")]
    DepotUnavailable(u32),
    #[error("{0} changed while it was being loaded. Refresh and try again.")]
    ContentChanged(String),
    #[error("The proxy request could not be signed")]
    Signing,
    #[error("The system clock is set before the Unix epoch")]
    Clock,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lua_looking_payloads_are_accepted() {
        assert!(looks_like_lua(b"addappid(440)\n"));
        assert!(looks_like_lua(b"-- setManifestid(440,\"1\")"));
        assert!(!looks_like_lua(b"<!DOCTYPE html><html>error</html>"));
    }

    #[test]
    fn builds_relative_source_url() {
        let file = repository_file(
            FileDto {
                name: "hid.dll".to_owned(),
                sha: "aa".to_owned(),
            },
            "/v1/service/file/",
        );
        assert_eq!(file.source_url, "/v1/service/file/hid.dll");
        assert_eq!(file.relative_path, "hid.dll");
        assert_eq!(file.file_name(), "hid.dll");
    }

    #[test]
    fn only_url_safe_file_names_are_signed() {
        assert!(is_safe_file_name("hid.dll"));
        assert!(is_safe_file_name("fix_part-01.zip"));
        // These all change under URL encoding, so the proxy would verify a different path than the
        // one we signed and answer 401 with no way for the user to tell why.
        assert!(!is_safe_file_name("my file.dll"));
        assert!(!is_safe_file_name("größe.dll"));
        assert!(!is_safe_file_name("a%20b.dll"));
        // …and these would retarget the request entirely.
        assert!(!is_safe_file_name("../../etc/passwd"));
        assert!(!is_safe_file_name("sub/file.dll"));
        assert!(!is_safe_file_name(""));
        assert!(!is_safe_file_name(".."));
    }

    #[test]
    fn hmac_matches_known_answer_vector() {
        // Same vector as proxy/README.md so both sides are provably identical.
        let signing_string = "GET\n/v1/lua/730\n1700000000\nabc123def4567890";
        let mut mac = HmacSha256::new_from_slice(b"test-secret").expect("hmac key");
        mac.update(signing_string.as_bytes());
        let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        assert_eq!(signature, "l+/pk0jM9UXgbgGYkAfnE+IS3yJ51BvqxMv8MMoY5cs=");
    }
}
