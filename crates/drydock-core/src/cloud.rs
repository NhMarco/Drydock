//! CloudRedirect integration ("Steam Cloud" for lua games).
//!
//! CloudRedirect ([`Selectively11/CloudRedirect`]) ships a `cloud_redirect.dll` that the
//! OST/SteamTools payload's code cave loads automatically when it sits next to `steam.exe`. The
//! DLL self-initializes on Steam start and reads its provider configuration from
//! `%APPDATA%\CloudRedirect\config.json` (per-user). So — unlike the official WPF companion, which
//! also handles Google Drive / OneDrive OAuth — Drydock only has to:
//!
//! 1. download the DLL from the official GitHub release, verified against its published SHA-256,
//! 2. copy it beside `steam.exe`, and
//! 3. write `config.json` (plus an S3/R2 credentials file) for the chosen provider.
//!
//! The credential-free providers (**Local only**, **Folder / mapped drive**, **Cloudflare R2**,
//! **S3-compatible**) are configured by writing files. **Google Drive** and **OneDrive** run the
//! same OAuth authorization-code + PKCE flow the companion uses ([`authorize`]) — browser sign-in,
//! loopback capture, token exchange — and write the DLL's DPAPI-encrypted token file.
//!
//! [`Selectively11/CloudRedirect`]: https://github.com/Selectively11/CloudRedirect

use std::fs;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore as _;
use rand::rngs::OsRng;
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Official upstream repository. Only its public release assets are fetched (no token).
pub const CLOUD_REPOSITORY: &str = "Selectively11/CloudRedirect";
/// Release asset and on-disk name of the redirect DLL.
pub const DLL_FILE_NAME: &str = "cloud_redirect.dll";

const MIN_DLL_BYTES: u64 = 256 * 1024;
const MAX_DLL_BYTES: u64 = 32 * 1024 * 1024;

/// Which cloud backend the redirect DLL should use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloudProvider {
    /// No cloud sync — saves are staged locally only (just clears the Steam Cloud error).
    LocalOnly,
    /// A local folder or mapped/network drive (e.g. a synced Google Drive/OneDrive desktop folder).
    Folder,
    /// Cloudflare R2 (S3-compatible; endpoint derived from the account id).
    R2,
    /// Any S3-compatible endpoint (AWS S3, MinIO, Backblaze B2, Wasabi, …).
    S3,
    /// Google Drive (OAuth browser sign-in).
    GoogleDrive,
    /// Microsoft OneDrive (OAuth browser sign-in).
    OneDrive,
}

impl CloudProvider {
    /// The `provider` token written to `config.json`, matching the DLL's parser.
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            Self::LocalOnly => "local",
            Self::Folder => "folder",
            Self::R2 => "r2",
            Self::S3 => "s3",
            Self::GoogleDrive => "gdrive",
            Self::OneDrive => "onedrive",
        }
    }

    #[must_use]
    pub fn display_name(self) -> &'static str {
        match self {
            Self::LocalOnly => "Local only",
            Self::Folder => "Folder / mapped drive",
            Self::R2 => "Cloudflare R2",
            Self::S3 => "S3-compatible",
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
        }
    }

    /// True for providers that need an interactive OAuth browser sign-in.
    #[must_use]
    pub fn is_oauth(self) -> bool {
        matches!(self, Self::GoogleDrive | Self::OneDrive)
    }
}

/// Credentials for an S3-compatible or R2 backend. `account_id` is only used (and required) for R2.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct S3Credentials {
    pub account_id: String,
    pub endpoint: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub bucket: String,
    pub key_prefix: String,
}

/// A fully resolved CloudRedirect provider configuration ready to persist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloudSettings {
    /// Local-only staging directory (defaults to `<steam>/localcloud` when empty).
    LocalOnly { path: String },
    /// A folder / mapped drive to sync into.
    Folder { path: String },
    /// Cloudflare R2 credentials.
    R2(S3Credentials),
    /// Generic S3-compatible credentials.
    S3(S3Credentials),
    /// An OAuth provider (Google Drive / OneDrive) whose token file was already written.
    OAuth {
        provider: CloudProvider,
        token_path: String,
    },
}

impl CloudSettings {
    #[must_use]
    pub fn provider(&self) -> CloudProvider {
        match self {
            Self::LocalOnly { .. } => CloudProvider::LocalOnly,
            Self::Folder { .. } => CloudProvider::Folder,
            Self::R2(_) => CloudProvider::R2,
            Self::S3(_) => CloudProvider::S3,
            Self::OAuth { provider, .. } => *provider,
        }
    }
}

/// Deployment state of the DLL beside `steam.exe`. `Default` is the "not installed" state, which is
/// what callers show before the first [`dll_status`] snapshot has been taken.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DllStatus {
    pub installed: bool,
    pub size: u64,
    pub sha256: Option<String>,
}

/// `%APPDATA%\CloudRedirect` — where the DLL reads `config.json`, matching the companion app.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    if appdata.is_empty() {
        return None;
    }
    Some(PathBuf::from(appdata).join("CloudRedirect"))
}

#[must_use]
fn config_file() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.json"))
}

/// The DLL's target path beside `steam.exe`.
#[must_use]
pub fn dll_path(steam_root: &Path) -> PathBuf {
    steam_root.join(DLL_FILE_NAME)
}

/// Inspect the deployed DLL, if any.
#[must_use]
pub fn dll_status(steam_root: &Path) -> DllStatus {
    let path = dll_path(steam_root);
    match fs::metadata(&path) {
        Ok(meta) if meta.is_file() => DllStatus {
            installed: true,
            size: meta.len(),
            sha256: file_sha256(&path).ok(),
        },
        _ => DllStatus {
            installed: false,
            size: 0,
            sha256: None,
        },
    }
}

/// Atomically write the verified DLL bytes beside `steam.exe`.
pub fn deploy_dll(steam_root: &Path, dll: &[u8]) -> Result<(), CloudError> {
    if !(MIN_DLL_BYTES..=MAX_DLL_BYTES).contains(&(dll.len() as u64)) {
        return Err(CloudError::InvalidDllSize);
    }
    let target = dll_path(steam_root);
    let temporary = target.with_extension(format!("dll.{}.new", std::process::id()));
    fs::write(&temporary, dll)?;
    if let Err(error) = persist(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
}

/// Remove a deployed DLL. Missing is treated as success.
pub fn remove_dll(steam_root: &Path) -> Result<(), CloudError> {
    match fs::remove_file(dll_path(steam_root)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Read the currently persisted provider, if any (best effort; returns the raw `provider` token).
#[must_use]
pub fn current_provider() -> Option<CloudProvider> {
    let path = config_file()?;
    let text = fs::read_to_string(path).ok()?;
    let document: serde_json::Value = serde_json::from_str(&text).ok()?;
    match document.get("provider")?.as_str()? {
        "local" => Some(CloudProvider::LocalOnly),
        "folder" => Some(CloudProvider::Folder),
        "r2" => Some(CloudProvider::R2),
        "s3" => Some(CloudProvider::S3),
        "gdrive" => Some(CloudProvider::GoogleDrive),
        "onedrive" => Some(CloudProvider::OneDrive),
        _ => None,
    }
}

/// Persist `config.json` (and an S3/R2 credentials file) so the DLL picks up the provider. Existing
/// unrelated keys in `config.json` and other providers' `token_paths` entries are preserved.
pub fn write_settings(settings: &CloudSettings) -> Result<(), CloudError> {
    let dir = config_dir().ok_or(CloudError::NoConfigDir)?;
    write_settings_to(&dir, settings)
}

fn write_settings_to(dir: &Path, settings: &CloudSettings) -> Result<(), CloudError> {
    fs::create_dir_all(dir)?;
    let config_path = dir.join("config.json");

    // Preserve unrelated top-level keys and the per-provider token_paths registry.
    let mut document = read_json_object(&config_path);
    let mut token_paths = document
        .get("token_paths")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    document.remove("sync_path");
    document.remove("token_path");

    let provider = settings.provider();
    document.insert(
        "provider".to_owned(),
        serde_json::Value::String(provider.config_key().to_owned()),
    );

    match settings {
        CloudSettings::Folder { path } => {
            document.insert("sync_path".to_owned(), serde_json::Value::String(path.clone()));
        }
        CloudSettings::LocalOnly { path } => {
            document.insert("token_path".to_owned(), serde_json::Value::String(path.clone()));
        }
        CloudSettings::R2(credentials) | CloudSettings::S3(credentials) => {
            let file_name = match provider {
                CloudProvider::R2 => "r2_credentials.json",
                _ => "s3_credentials.json",
            };
            let credentials_path = dir.join(file_name);
            write_credentials(&credentials_path, provider, credentials)?;
            let credentials_string = credentials_path.to_string_lossy().into_owned();
            document.insert(
                "token_path".to_owned(),
                serde_json::Value::String(credentials_string.clone()),
            );
            token_paths.insert(
                provider.config_key().to_owned(),
                serde_json::Value::String(credentials_string),
            );
        }
        CloudSettings::OAuth { token_path, .. } => {
            // The OAuth flow already wrote the encrypted token file at token_path.
            document.insert(
                "token_path".to_owned(),
                serde_json::Value::String(token_path.clone()),
            );
            token_paths.insert(
                provider.config_key().to_owned(),
                serde_json::Value::String(token_path.clone()),
            );
        }
    }

    document.insert("token_paths".to_owned(), serde_json::Value::Object(token_paths));

    let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(document))?;
    write_atomic(&config_path, serialized.as_bytes())?;
    Ok(())
}

fn write_credentials(
    path: &Path,
    provider: CloudProvider,
    credentials: &S3Credentials,
) -> Result<(), CloudError> {
    let mut object = serde_json::Map::new();
    object.insert(
        "access_key_id".to_owned(),
        serde_json::Value::String(credentials.access_key_id.clone()),
    );
    object.insert(
        "secret_access_key".to_owned(),
        serde_json::Value::String(credentials.secret_access_key.clone()),
    );
    object.insert(
        "bucket".to_owned(),
        serde_json::Value::String(credentials.bucket.clone()),
    );
    if !credentials.key_prefix.trim().is_empty() {
        object.insert(
            "key_prefix".to_owned(),
            serde_json::Value::String(credentials.key_prefix.clone()),
        );
    }
    if provider == CloudProvider::R2 {
        object.insert(
            "account_id".to_owned(),
            serde_json::Value::String(credentials.account_id.clone()),
        );
    }
    // R2 derives its endpoint from the account id; only send an explicit endpoint when given.
    if !credentials.endpoint.trim().is_empty() {
        object.insert(
            "endpoint".to_owned(),
            serde_json::Value::String(credentials.endpoint.clone()),
        );
    }
    if !credentials.region.trim().is_empty() {
        object.insert(
            "region".to_owned(),
            serde_json::Value::String(credentials.region.clone()),
        );
    }
    let serialized = serde_json::to_string_pretty(&serde_json::Value::Object(object))?;
    write_atomic(path, serialized.as_bytes())
}

/// Downloads and verifies the official `cloud_redirect.dll` from the latest GitHub release.
pub struct CloudRedirect {
    client: Client,
    repository: String,
}

impl CloudRedirect {
    pub fn new() -> Result<Self, CloudError> {
        let client = Client::builder()
            .user_agent(concat!("Drydock/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            client,
            repository: CLOUD_REPOSITORY.to_owned(),
        })
    }

    /// The latest published release tag (e.g. `v2.6.5`).
    pub fn latest_version(&self) -> Result<String, CloudError> {
        Ok(self.latest_release()?.tag_name)
    }

    /// Fetch the DLL bytes for the latest release, verified against its published SHA-256.
    pub fn download_dll(&self) -> Result<DownloadedDll, CloudError> {
        let release = self.latest_release()?;
        let dll_url = release.asset_url(DLL_FILE_NAME).ok_or(CloudError::AssetMissing)?;
        let checksum_url = release
            .asset_url(&format!("{DLL_FILE_NAME}.sha256"))
            .ok_or(CloudError::AssetMissing)?;

        let expected = parse_checksum(&self.get_text(&checksum_url)?)?;
        let bytes = self.get_bytes(&dll_url)?;
        let actual = uppercase_hex(Sha256::digest(&bytes));
        if !constant_time_eq(&actual, &expected) {
            return Err(CloudError::ChecksumMismatch);
        }
        Ok(DownloadedDll {
            version: release.tag_name,
            bytes,
            sha256: actual,
        })
    }

    fn latest_release(&self) -> Result<GitHubRelease, CloudError> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repository);
        let response = self
            .client
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()?
            .error_for_status()?;
        if response.content_length().is_some_and(|size| size > 512 * 1024) {
            return Err(CloudError::InvalidRelease);
        }
        Ok(response.json()?)
    }

    fn get_text(&self, url: &str) -> Result<String, CloudError> {
        let response = self.client.get(url).send()?.error_for_status()?;
        if response.content_length().is_some_and(|size| size > 4096) {
            return Err(CloudError::InvalidRelease);
        }
        Ok(response.text()?)
    }

    fn get_bytes(&self, url: &str) -> Result<Vec<u8>, CloudError> {
        let mut response = self.client.get(url).send()?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|size| !(MIN_DLL_BYTES..=MAX_DLL_BYTES).contains(&size))
        {
            return Err(CloudError::InvalidDllSize);
        }
        let mut buffer = Vec::new();
        response.read_to_end(&mut buffer)?;
        if !(MIN_DLL_BYTES..=MAX_DLL_BYTES).contains(&(buffer.len() as u64)) {
            return Err(CloudError::InvalidDllSize);
        }
        Ok(buffer)
    }
}

/// A verified DLL download ready to deploy.
pub struct DownloadedDll {
    pub version: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

fn read_json_object(path: &Path) -> serde_json::Map<String, serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), CloudError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    fs::write(&temporary, data)?;
    persist(&temporary, path)
}

fn persist(temporary: &Path, target: &Path) -> Result<(), CloudError> {
    match fs::rename(temporary, target) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-device or replace-in-place fallback.
            fs::copy(temporary, target)?;
            let _ = fs::remove_file(temporary);
            Ok(())
        }
    }
}

fn file_sha256(path: &Path) -> Result<String, CloudError> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(uppercase_hex(hasher.finalize()))
}

/// The `.sha256` asset is `HASH  filename` (or bare hash); take the leading 64-hex token.
fn parse_checksum(text: &str) -> Result<String, CloudError> {
    let token = text.split_whitespace().next().unwrap_or("");
    if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(token.to_ascii_uppercase())
    } else {
        Err(CloudError::InvalidChecksum)
    }
}

fn uppercase_hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

impl GitHubRelease {
    fn asset_url(&self, name: &str) -> Option<String> {
        self.assets
            .iter()
            .find(|asset| asset.name == name)
            .map(|asset| asset.browser_download_url.clone())
    }
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CloudError {
    #[error("%APPDATA% could not be resolved for the CloudRedirect config directory")]
    NoConfigDir,
    #[error("the release is missing the expected CloudRedirect asset")]
    AssetMissing,
    #[error("the release metadata was unexpectedly large")]
    InvalidRelease,
    #[error("the published checksum could not be parsed")]
    InvalidChecksum,
    #[error("the downloaded DLL checksum does not match the release")]
    ChecksumMismatch,
    #[error("the downloaded DLL has an invalid size")]
    InvalidDllSize,
    #[error("this provider does not use an OAuth sign-in")]
    NotOAuth,
    #[error("the OAuth callback port is already in use")]
    PortInUse,
    #[error("the browser sign-in was not completed in time")]
    OAuthTimeout,
    #[error("the provider returned an authorization error: {0}")]
    OAuthDenied(String),
    #[error("the sign-in response could not be verified (state mismatch)")]
    OAuthStateMismatch,
    #[error("the provider did not return a refresh token — revoke access and try again")]
    NoRefreshToken,
    #[error("the token exchange failed: {0}")]
    TokenExchange(String),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

// ── OAuth (Google Drive / OneDrive) ────────────────────────────────────────
//
// The DLL reads its token file (`token_path`) as `{access_token, refresh_token,
// expires_at}` and auto-refreshes. These client credentials, endpoints and the
// loopback redirect scheme are exactly the ones the official companion uses, so
// the token file the DLL expects is produced identically.

struct OAuthClient {
    client_id: &'static str,
    client_secret: &'static str,
    scope: &'static str,
    auth_url: &'static str,
    token_url: &'static str,
}

const GDRIVE: OAuthClient = OAuthClient {
    client_id: "1072944905499-vm2v2i5dvn0a0d2o4ca36i1vge8cvbn0.apps.googleusercontent.com",
    client_secret: "v6V3fKV_zWU7iw1DrpO1rknX",
    scope: "https://www.googleapis.com/auth/drive.file",
    auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
};

const ONEDRIVE: OAuthClient = OAuthClient {
    // rclone's public client id — the companion uses it because its own Azure app
    // has redirect-uri restrictions. The fixed port below is the one it registers.
    client_id: "b15665d9-eda6-4092-8539-0eec376afd59",
    client_secret: "qtyfaBBYA403=unZUP40~_#",
    scope: "Files.ReadWrite offline_access",
    auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
    token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token",
};

const ONEDRIVE_PORT: u16 = 53682;
const OAUTH_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Run the interactive OAuth sign-in for Google Drive or OneDrive: open the browser,
/// capture the loopback redirect, exchange the code, write the DLL's encrypted token
/// file, and return the settings to persist. `log` receives progress lines.
pub fn authorize<F: Fn(&str)>(provider: CloudProvider, log: F) -> Result<CloudSettings, CloudError> {
    let client = match provider {
        CloudProvider::GoogleDrive => &GDRIVE,
        CloudProvider::OneDrive => &ONEDRIVE,
        _ => return Err(CloudError::NotOAuth),
    };
    let dir = config_dir().ok_or(CloudError::NoConfigDir)?;
    fs::create_dir_all(&dir)?;
    let token_path = dir.join(match provider {
        CloudProvider::GoogleDrive => "google_tokens.json",
        _ => "onedrive_tokens.json",
    });

    let verifier = random_url_string(64);
    let challenge = code_challenge(&verifier);
    let state = random_url_string(32);

    let (listener, redirect_uri) = match provider {
        CloudProvider::OneDrive => {
            let listener =
                TcpListener::bind(("127.0.0.1", ONEDRIVE_PORT)).map_err(|_| CloudError::PortInUse)?;
            (listener, format!("http://localhost:{ONEDRIVE_PORT}/"))
        }
        _ => {
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port();
            (listener, format!("http://localhost:{port}/callback"))
        }
    };

    let auth_url = build_auth_url(client, provider, &redirect_uri, &state, &challenge);
    log("Opening your browser to sign in…");
    if open::that(&auth_url).is_err() {
        log(&format!(
            "Could not open a browser. Open this URL manually:\n{auth_url}"
        ));
    }

    log("Waiting for the sign-in to complete…");
    let code = capture_authorization_code(&listener, &state)?;

    log("Exchanging the authorization code for tokens…");
    let tokens = exchange_code(client, provider, &code, &redirect_uri, &verifier)?;
    if tokens.refresh_token.trim().is_empty() {
        return Err(CloudError::NoRefreshToken);
    }

    let expires_at = now_unix().saturating_add(tokens.expires_in);
    let token_json = serde_json::to_string_pretty(&serde_json::json!({
        "access_token": tokens.access_token,
        "refresh_token": tokens.refresh_token,
        "expires_at": expires_at,
    }))?;
    write_token_file(&token_path, token_json.as_bytes())?;
    log("Sign-in successful. Tokens saved; the DLL will refresh them automatically.");

    Ok(CloudSettings::OAuth {
        provider,
        token_path: token_path.to_string_lossy().into_owned(),
    })
}

fn build_auth_url(
    client: &OAuthClient,
    provider: CloudProvider,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    let mut url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&prompt=consent\
         &state={}&code_challenge={}&code_challenge_method=S256",
        client.auth_url,
        url_encode(client.client_id),
        url_encode(redirect_uri),
        url_encode(client.scope),
        url_encode(state),
        url_encode(challenge),
    );
    // Google needs access_type=offline to return a refresh token.
    if provider == CloudProvider::GoogleDrive {
        url.push_str("&access_type=offline");
    }
    url
}

struct OAuthTokens {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

fn exchange_code(
    client: &OAuthClient,
    provider: CloudProvider,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<OAuthTokens, CloudError> {
    let http = Client::builder()
        .user_agent(concat!("Drydock/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut form = vec![
        ("code", code),
        ("client_id", client.client_id),
        ("client_secret", client.client_secret),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
        ("code_verifier", verifier),
    ];
    if provider == CloudProvider::OneDrive {
        form.push(("scope", client.scope));
    }
    let response = http.post(client.token_url).form(&form).send()?;
    let status = response.status();
    let body = response.text()?;
    if !status.is_success() {
        return Err(CloudError::TokenExchange(format!(
            "HTTP {}: {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        )));
    }
    let document: serde_json::Value = serde_json::from_str(&body)?;
    Ok(OAuthTokens {
        access_token: document
            .get("access_token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned(),
        refresh_token: document
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned(),
        expires_in: document
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(3600),
    })
}

/// Accept loopback connections until the OAuth redirect arrives; validate the CSRF
/// `state` and return the authorization `code`. Non-OAuth hits (favicon, etc.) are
/// answered and skipped. Bounded by [`OAUTH_TIMEOUT`].
fn capture_authorization_code(listener: &TcpListener, expected_state: &str) -> Result<String, CloudError> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + OAUTH_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err(CloudError::OAuthTimeout);
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let target = match read_request_target(&mut stream) {
            Some(target) => target,
            None => continue,
        };
        let (code, error, state) = parse_oauth_query(&target);
        if code.is_none() && error.is_none() && state.is_none() {
            // Not the OAuth callback (e.g. a favicon request).
            let _ = write_http_response(&mut stream, "204 No Content", "");
            continue;
        }
        if let Some(error) = error {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "<h1>Sign-in failed</h1><p>You can close this tab and return to Drydock.</p>",
            );
            return Err(CloudError::OAuthDenied(error));
        }
        if state.as_deref() != Some(expected_state) {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "<h1>Sign-in failed</h1><p>You can close this tab and return to Drydock.</p>",
            );
            return Err(CloudError::OAuthStateMismatch);
        }
        let _ = write_http_response(
            &mut stream,
            "200 OK",
            "<h1>Signed in</h1><p>You can close this tab and return to Drydock.</p>",
        );
        return code.ok_or(CloudError::OAuthStateMismatch);
    }
}

/// Read just the request line and return the request target (e.g. `/callback?code=…`).
fn read_request_target(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut buffer = [0_u8; 8192];
    let mut filled = 0;
    while filled < buffer.len() {
        let read = stream.read(&mut buffer[filled..]).ok()?;
        if read == 0 {
            break;
        }
        filled += read;
        if buffer[..filled].windows(2).any(|pair| pair == b"\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer[..filled]);
    let line = text.lines().next()?;
    // "GET /callback?code=... HTTP/1.1"
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    Some(parts.next()?.to_owned())
}

fn parse_oauth_query(target: &str) -> (Option<String>, Option<String>, Option<String>) {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut error = None;
    let mut state = None;
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            let value = url_decode(value);
            match key {
                "code" => code = Some(value),
                "error" => error = Some(value),
                "state" => state = Some(value),
                _ => {}
            }
        }
    }
    (code, error, state)
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: &str,
    body_html: &str,
) -> std::io::Result<()> {
    let body = if body_html.is_empty() {
        String::new()
    } else {
        format!(
            "<html><body style=\"font-family:Segoe UI,sans-serif;text-align:center;\
             padding:60px;background:#1e1e1e;color:#fff\">{body_html}</body></html>"
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())
}

/// Random URL-safe token of `length` characters (base64url of `length` random bytes,
/// truncated), matching the companion's PKCE verifier/state generation.
fn random_url_string(length: usize) -> String {
    let mut bytes = vec![0_u8; length];
    OsRng.fill_bytes(&mut bytes);
    let mut encoded = URL_SAFE_NO_PAD.encode(&bytes);
    encoded.truncate(length);
    encoded
}

/// PKCE S256 challenge: base64url(SHA-256(verifier)) with no padding.
fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// Write the token file the way the DLL reads it: DPAPI-encrypted (CurrentUser) on
/// Windows. On other platforms the DLL also accepts plaintext JSON, so write that.
fn write_token_file(path: &Path, json: &[u8]) -> Result<(), CloudError> {
    #[cfg(windows)]
    {
        match dpapi_protect(json) {
            Some(blob) => write_atomic(path, &blob),
            // The DLL treats a leading '{' as legacy plaintext and re-encrypts it.
            None => write_atomic(path, json),
        }
    }
    #[cfg(not(windows))]
    {
        write_atomic(path, json)
    }
}

#[cfg(windows)]
fn dpapi_protect(plain: &[u8]) -> Option<Vec<u8>> {
    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }
    #[link(name = "crypt32")]
    unsafe extern "system" {
        fn CryptProtectData(
            data_in: *const DataBlob,
            data_descr: *const u16,
            optional_entropy: *const DataBlob,
            reserved: *mut core::ffi::c_void,
            prompt_struct: *mut core::ffi::c_void,
            flags: u32,
            data_out: *mut DataBlob,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let mut input = plain.to_vec();
    let data_in = DataBlob {
        cb_data: input.len() as u32,
        pb_data: input.as_mut_ptr(),
    };
    let mut data_out = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    // SAFETY: pointers are valid for the call; the output blob is copied out and freed.
    unsafe {
        if CryptProtectData(
            &data_in,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            &mut data_out,
        ) == 0
            || data_out.pb_data.is_null()
        {
            return None;
        }
        let blob = std::slice::from_raw_parts(data_out.pb_data, data_out.cb_data as usize).to_vec();
        LocalFree(data_out.pb_data.cast());
        Some(blob)
    }
}

fn url_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(byte as char);
            }
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                    Some(decoded) => {
                        output.push(decoded);
                        index += 3;
                    }
                    None => {
                        output.push(b'%');
                        index += 1;
                    }
                }
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            other => {
                output.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_keys_match_dll_parser() {
        assert_eq!(CloudProvider::LocalOnly.config_key(), "local");
        assert_eq!(CloudProvider::Folder.config_key(), "folder");
        assert_eq!(CloudProvider::R2.config_key(), "r2");
        assert_eq!(CloudProvider::S3.config_key(), "s3");
    }

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // RFC 7636 Appendix B known-answer vector.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_url_string(48).len(), 48);
    }

    #[test]
    fn oauth_query_parse_and_url_codec_roundtrip() {
        let (code, error, state) = parse_oauth_query("/callback?code=abc%2F123&state=xyz&foo=1");
        assert_eq!(code.as_deref(), Some("abc/123"));
        assert_eq!(state.as_deref(), Some("xyz"));
        assert!(error.is_none());
        let (_, error, _) = parse_oauth_query("/?error=access_denied&state=s");
        assert_eq!(error.as_deref(), Some("access_denied"));
        assert_eq!(url_decode(&url_encode("a b/c?=&~x")), "a b/c?=&~x");
    }

    #[test]
    fn oauth_settings_write_uses_provider_schema() {
        let temp = std::env::temp_dir().join(format!("cr-oauth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        write_settings_to(
            &temp,
            &CloudSettings::OAuth {
                provider: CloudProvider::GoogleDrive,
                token_path: "C:/tokens/google_tokens.json".to_owned(),
            },
        )
        .expect("write oauth");
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(temp.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["provider"], "gdrive");
        assert_eq!(config["token_path"], "C:/tokens/google_tokens.json");
        assert_eq!(config["token_paths"]["gdrive"], "C:/tokens/google_tokens.json");
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn checksum_parses_hash_with_and_without_filename() {
        let bare = "a".repeat(64);
        assert_eq!(parse_checksum(&bare).unwrap(), bare.to_ascii_uppercase());
        let labelled = format!("{bare}  cloud_redirect.dll");
        assert_eq!(parse_checksum(&labelled).unwrap(), bare.to_ascii_uppercase());
        assert!(parse_checksum("not-a-hash").is_err());
        assert!(parse_checksum(&"a".repeat(63)).is_err());
    }

    #[test]
    fn write_settings_produces_the_dll_schema() {
        let temp = std::env::temp_dir().join(format!("cr-cfg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);

        // S3: config.json points token_path at s3_credentials.json with the exact field names.
        let creds = S3Credentials {
            endpoint: "https://s3.example.com".to_owned(),
            region: "us-east-1".to_owned(),
            access_key_id: "AKIA".to_owned(),
            secret_access_key: "secret".to_owned(),
            bucket: "saves".to_owned(),
            key_prefix: "prefix".to_owned(),
            ..Default::default()
        };
        write_settings_to(&temp, &CloudSettings::S3(creds)).expect("write s3");

        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(temp.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["provider"], "s3");
        let token_path = config["token_path"].as_str().unwrap();
        assert!(token_path.ends_with("s3_credentials.json"));
        assert_eq!(config["token_paths"]["s3"], token_path);

        let credentials: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(token_path).unwrap()).unwrap();
        assert_eq!(credentials["access_key_id"], "AKIA");
        assert_eq!(credentials["secret_access_key"], "secret");
        assert_eq!(credentials["bucket"], "saves");
        assert_eq!(credentials["endpoint"], "https://s3.example.com");
        assert_eq!(credentials["region"], "us-east-1");

        // Switching to a folder keeps the s3 token_paths entry but swaps provider + sync_path.
        write_settings_to(
            &temp,
            &CloudSettings::Folder {
                path: "D:/saves".to_owned(),
            },
        )
        .expect("write folder");
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(temp.join("config.json")).unwrap()).unwrap();
        assert_eq!(config["provider"], "folder");
        assert_eq!(config["sync_path"], "D:/saves");
        assert!(config.get("token_path").is_none());
        assert!(!config["token_paths"]["s3"].as_str().unwrap().is_empty());

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn r2_credentials_include_account_id() {
        let temp = std::env::temp_dir().join(format!("cr-r2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        write_settings_to(
            &temp,
            &CloudSettings::R2(S3Credentials {
                account_id: "acct123".to_owned(),
                access_key_id: "key".to_owned(),
                secret_access_key: "sec".to_owned(),
                bucket: "b".to_owned(),
                ..Default::default()
            }),
        )
        .expect("write r2");
        let creds: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(temp.join("r2_credentials.json")).unwrap()).unwrap();
        assert_eq!(creds["account_id"], "acct123");
        assert_eq!(creds["bucket"], "b");
        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn deploy_and_status_roundtrip() {
        let steam = std::env::temp_dir().join(format!("cr-steam-{}", std::process::id()));
        let _ = fs::remove_dir_all(&steam);
        fs::create_dir_all(&steam).unwrap();
        let dll = vec![0_u8; MIN_DLL_BYTES as usize + 16];
        deploy_dll(&steam, &dll).expect("deploy");
        let status = dll_status(&steam);
        assert!(status.installed);
        assert_eq!(status.size, dll.len() as u64);
        remove_dll(&steam).expect("remove");
        assert!(!dll_status(&steam).installed);
        let _ = fs::remove_dir_all(&steam);
    }
}
