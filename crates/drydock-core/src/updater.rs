//! SHA-256-verified self-updater against the configured GitHub Release.
//!
//! # Trust model — read before changing this
//!
//! The downloaded binary is checked against a `<asset>.sha256` file, the PE/ELF architecture is
//! verified, the staged file may only come from our own temp directory, and the comparison is
//! constant-time. That defends against a corrupted or truncated download and against a local process
//! swapping the staged file — but **not** against whoever controls the release itself: the checksum
//! ships from the same GitHub Release as the binary, so an attacker who can publish a release (or a
//! compromised repository token) simply publishes a matching pair.
//!
//! Closing that gap needs a signature the client can verify against a key it already holds —
//! Authenticode on the Windows binaries, or an embedded public key checking a detached signature over
//! the asset. Until then, the release credentials are the security boundary; treat them accordingly.

use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sysinfo::{Pid, ProcessesToUpdate, System};
use thiserror::Error;

use crate::version::{APP_VERSION, user_agent};

const APPLY_ARGUMENT: &str = "--apply-update";
const MINIMUM_BINARY_BYTES: usize = 1_000_000;
const MAXIMUM_BINARY_BYTES: usize = 100_000_000;
const MAXIMUM_CHECKSUM_BYTES: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct AppVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl std::fmt::Display for AppVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl AppVersion {
    pub fn parse(value: &str) -> Result<Self, UpdateError> {
        let value = value.trim().trim_start_matches(['v', 'V']);
        let value = value.split(['-', '+']).next().unwrap_or_default();
        let mut parts = value.split('.');
        let version = Self {
            major: parse_version_part(parts.next())?,
            minor: parse_version_part(parts.next())?,
            patch: parse_version_part(parts.next())?,
        };
        if parts.next().is_some() {
            return Err(UpdateError::InvalidVersion);
        }
        Ok(version)
    }
}

#[derive(Clone, Debug)]
pub struct PreparedUpdate {
    pub version: AppVersion,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub sha256: String,
}

pub struct AppUpdater {
    repository: String,
    client: Client,
}

impl AppUpdater {
    /// The `owner/repo` this build updates from, resolved through [`crate::config`] — so a fork can
    /// point at its own release channel via `DRYDOCK_UPDATE_REPOSITORY` or the Settings field
    /// instead of having to patch `build.rs`.
    pub fn configured_repository() -> Option<String> {
        crate::config::update_repository()
    }

    pub fn new() -> Result<Self, UpdateError> {
        let repository = Self::configured_repository().ok_or(UpdateError::NotConfigured)?;
        Self::for_repository(&repository)
    }

    pub fn for_repository(repository: &str) -> Result<Self, UpdateError> {
        if !valid_repository(repository) {
            return Err(UpdateError::InvalidRepository);
        }
        let client = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self {
            repository: repository.to_owned(),
            client,
        })
    }

    pub fn can_self_update() -> bool {
        if cfg!(debug_assertions) || Self::configured_repository().is_none() {
            return false;
        }
        env::current_exe().is_ok_and(|path| expected_target_name(&path))
    }

    pub fn prepare_update(&self) -> Result<Option<PreparedUpdate>, UpdateError> {
        if !Self::can_self_update() {
            return Ok(None);
        }
        let current = AppVersion::parse(APP_VERSION)?;
        let release = self.latest_release()?;
        let version = AppVersion::parse(&release.tag_name)?;
        if version <= current {
            return Ok(None);
        }
        let binary_name = target_asset_name().ok_or(UpdateError::UnsupportedPlatform)?;
        let checksum_name = format!("{binary_name}.sha256");
        let binary_url = asset_url(&release, binary_name)?;
        let checksum_url = asset_url(&release, &checksum_name)?;
        let checksum = parse_checksum(&read_limited(
            self.authorized_get(&checksum_url, "application/octet-stream")?,
            MAXIMUM_CHECKSUM_BYTES,
        )?)?;

        let update_directory = env::temp_dir().join("Drydock").join("updates");
        fs::create_dir_all(&update_directory)?;
        cleanup_old_updates(&update_directory);
        let destination = update_directory.join(format!(
            "Drydock-{}-{}",
            version,
            binary_name.replace(['/', '\\'], "_")
        ));
        let temporary = destination.with_extension(format!("{}.download", std::process::id()));
        let result = (|| {
            download_binary(
                self.authorized_get(&binary_url, "application/octet-stream")?,
                &temporary,
            )?;
            let actual = file_sha256(&temporary)?;
            if !constant_time_text_eq(&actual, &checksum) {
                return Err(UpdateError::ChecksumMismatch);
            }
            if !has_expected_binary_architecture(&temporary)? {
                return Err(UpdateError::WrongArchitecture);
            }
            make_executable(&temporary)?;
            fs::rename(&temporary, &destination)?;
            Ok(PreparedUpdate {
                version,
                source_path: destination,
                target_path: env::current_exe()?,
                sha256: actual,
            })
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map(Some)
    }

    pub fn launch(update: &PreparedUpdate) -> Result<(), UpdateError> {
        let mut command = Command::new(&update.source_path);
        command
            .arg(APPLY_ARGUMENT)
            .arg(&update.target_path)
            .arg(std::process::id().to_string())
            .arg(update.version.to_string())
            .arg(&update.sha256)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut command);
        command.spawn()?;
        Ok(())
    }

    pub fn try_apply_from_args(args: &[String]) -> bool {
        if args.first().map(String::as_str) != Some(APPLY_ARGUMENT) {
            return false;
        }
        let _ = apply_from_args(args);
        true
    }

    fn latest_release(&self) -> Result<GitHubRelease, UpdateError> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", self.repository);
        let response = self
            .authorized_get(&url, "application/vnd.github+json")?
            .error_for_status()?;
        if response.content_length().is_some_and(|size| size > 256 * 1024) {
            return Err(UpdateError::InvalidRelease);
        }
        Ok(response.json()?)
    }

    fn authorized_get(&self, url: &str, accept: &'static str) -> Result<Response, UpdateError> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(accept));
        if let Some(token) = crate::config::github_token() {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
        }
        Ok(self.client.get(url).headers(headers).send()?)
    }
}

fn apply_from_args(args: &[String]) -> Result<(), UpdateError> {
    if args.len() != 5 {
        return Err(UpdateError::InvalidApplyRequest);
    }
    let source = env::current_exe()?;
    if !expected_update_source(&source) || !has_expected_binary_architecture(&source)? {
        return Err(UpdateError::InvalidApplyRequest);
    }
    let target = PathBuf::from(&args[1]);
    if !target.is_absolute() || !target.is_file() || !expected_target_name(&target) {
        return Err(UpdateError::InvalidApplyRequest);
    }
    let process_id = args[2]
        .parse::<u32>()
        .map_err(|_| UpdateError::InvalidApplyRequest)?;
    let _version = AppVersion::parse(&args[3])?;
    let expected_hash = parse_checksum(args[4].as_bytes())?;
    if !constant_time_text_eq(&file_sha256(&source)?, &expected_hash) {
        return Err(UpdateError::ChecksumMismatch);
    }
    wait_for_process(process_id)?;

    let staged = target.with_extension(format!("{}.update", std::process::id()));
    let result = (|| {
        fs::copy(&source, &staged)?;
        make_executable(&staged)?;
        if !constant_time_text_eq(&file_sha256(&staged)?, &expected_hash)
            || !has_expected_binary_architecture(&staged)?
        {
            return Err(UpdateError::ChecksumMismatch);
        }
        replace_executable(&staged, &target)?;
        let mut command = Command::new(&target);
        command
            .current_dir(target.parent().ok_or(UpdateError::InvalidApplyRequest)?)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut command);
        command.spawn()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staged);
        let _ = Command::new(&target).spawn();
    }
    result
}

fn replace_executable(source: &Path, target: &Path) -> Result<(), UpdateError> {
    #[cfg(windows)]
    {
        let backup = target.with_extension("exe.previous");
        if backup.exists() {
            fs::remove_file(&backup)?;
        }
        fs::rename(target, &backup)?;
        if let Err(error) = fs::rename(source, target) {
            let _ = fs::rename(&backup, target);
            return Err(error.into());
        }
        let _ = fs::remove_file(backup);
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, target)?;
    }
    Ok(())
}

fn wait_for_process(process_id: u32) -> Result<(), UpdateError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let pid = Pid::from_u32(process_id);
    let mut system = System::new();
    loop {
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        if system.process(pid).is_none() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(UpdateError::PreviousProcessRunning);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn download_binary(mut response: Response, destination: &Path) -> Result<(), UpdateError> {
    response = response.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size < MINIMUM_BINARY_BYTES as u64 || size > MAXIMUM_BINARY_BYTES as u64)
    {
        return Err(UpdateError::InvalidBinarySize);
    }
    let mut file = File::create(destination)?;
    let mut buffer = [0_u8; 128 * 1024];
    let mut total = 0_usize;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read).ok_or(UpdateError::InvalidBinarySize)?;
        if total > MAXIMUM_BINARY_BYTES {
            return Err(UpdateError::InvalidBinarySize);
        }
        file.write_all(&buffer[..read])?;
    }
    file.sync_all()?;
    if total < MINIMUM_BINARY_BYTES {
        return Err(UpdateError::InvalidBinarySize);
    }
    Ok(())
}

fn read_limited(mut response: Response, maximum: usize) -> Result<Vec<u8>, UpdateError> {
    response = response.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size > maximum as u64)
    {
        return Err(UpdateError::InvalidRelease);
    }
    let mut bytes = Vec::new();
    response.take((maximum + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(UpdateError::InvalidRelease);
    }
    Ok(bytes)
}

fn target_asset_name() -> Option<&'static str> {
    asset_name_for(env::consts::OS, env::consts::ARCH)
}

fn asset_name_for(os: &str, architecture: &str) -> Option<&'static str> {
    match (os, architecture) {
        ("windows", "x86_64") => Some("Drydock-windows-x64.exe"),
        ("windows", "aarch64") => Some("Drydock-windows-arm64.exe"),
        ("linux", "x86_64") => Some("Drydock-linux-x64"),
        ("linux", "aarch64") => Some("Drydock-linux-arm64"),
        _ => None,
    }
}

fn has_expected_binary_architecture(path: &Path) -> Result<bool, UpdateError> {
    let mut file = File::open(path)?;
    let mut header = [0_u8; 64];
    file.read_exact(&mut header)?;
    match env::consts::OS {
        "windows" => {
            if &header[..2] != b"MZ" {
                return Ok(false);
            }
            let Ok(offset_bytes) = <[u8; 4]>::try_from(&header[0x3c..0x40]) else {
                return Ok(false);
            };
            let offset = u32::from_le_bytes(offset_bytes) as u64;
            if offset < 64 || offset > file.metadata()?.len().saturating_sub(6) {
                return Ok(false);
            }
            use std::io::Seek as _;
            file.seek(std::io::SeekFrom::Start(offset))?;
            let mut pe = [0_u8; 6];
            file.read_exact(&mut pe)?;
            let machine = u16::from_le_bytes([pe[4], pe[5]]);
            Ok(&pe[..4] == b"PE\0\0"
                && matches!(
                    (env::consts::ARCH, machine),
                    ("x86_64", 0x8664) | ("aarch64", 0xaa64)
                ))
        }
        "linux" => {
            if &header[..4] != b"\x7fELF" || header[5] != 1 {
                return Ok(false);
            }
            let machine = u16::from_le_bytes([header[18], header[19]]);
            Ok(matches!(
                (env::consts::ARCH, machine),
                ("x86_64", 0x003e) | ("aarch64", 0x00b7)
            ))
        }
        _ => Ok(false),
    }
}

fn expected_update_source(path: &Path) -> bool {
    let expected = env::temp_dir().join("Drydock").join("updates");
    path.parent().is_some_and(|parent| parent == expected)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("Drydock-") && !name.ends_with(".download"))
}

fn expected_target_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_installed_binary_name)
}

/// The names Drydock may legitimately run under: the canonical installed name, or this
/// platform's published release asset — so a directly downloaded `Drydock-<os>-<arch>[.exe]`
/// updates itself in place instead of silently disabling the updater on a name mismatch.
fn is_installed_binary_name(name: &str) -> bool {
    let canonical = if cfg!(windows) { "Drydock.exe" } else { "Drydock" };
    name == canonical || target_asset_name() == Some(name)
}

fn asset_url(release: &GitHubRelease, name: &str) -> Result<String, UpdateError> {
    release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .map(|asset| asset.browser_download_url.clone())
        .ok_or_else(|| UpdateError::MissingAsset(name.to_owned()))
}

fn parse_version_part(part: Option<&str>) -> Result<u64, UpdateError> {
    part.filter(|value| !value.is_empty())
        .ok_or(UpdateError::InvalidVersion)?
        .parse()
        .map_err(|_| UpdateError::InvalidVersion)
}

fn parse_checksum(bytes: &[u8]) -> Result<String, UpdateError> {
    let value = std::str::from_utf8(bytes).map_err(|_| UpdateError::InvalidChecksum)?;
    let hash = value.split_whitespace().next().unwrap_or_default();
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateError::InvalidChecksum);
    }
    Ok(hash.to_ascii_uppercase())
}

fn file_sha256(path: &Path) -> Result<String, UpdateError> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(uppercase_hex(hash.finalize()))
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

fn constant_time_text_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.as_bytes()
        .iter()
        .zip(right.as_bytes())
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

/// `owner/repo` validation — shared with [`crate::config`] so the updater and the config layer can
/// never disagree about what counts as a usable repository.
fn valid_repository(value: &str) -> bool {
    crate::config::is_valid_repository(value)
}

fn cleanup_old_updates(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(2 * 24 * 60 * 60));
        if old {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), UpdateError> {
    Ok(())
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(0x08000000);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    // The public CDN download URL. Unlike the API asset URL it needs no token and does not
    // count against GitHub's anonymous API rate limit, which matters when many users update at
    // once from the public release repo.
    browser_download_url: String,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("The Drydock update repository is not configured in this build")]
    NotConfigured,
    #[error("The update repository must use the owner/repository format")]
    InvalidRepository,
    #[error("The release uses an invalid version")]
    InvalidVersion,
    #[error("The update channel returned invalid release data")]
    InvalidRelease,
    #[error("The release is missing {0}")]
    MissingAsset(String),
    #[error("The update checksum is invalid")]
    InvalidChecksum,
    #[error("The downloaded update checksum does not match the release")]
    ChecksumMismatch,
    #[error("The downloaded update has an invalid size")]
    InvalidBinarySize,
    #[error("The downloaded update is for a different operating system or CPU")]
    WrongArchitecture,
    #[error("This operating system or CPU is not supported by the updater")]
    UnsupportedPlatform,
    #[error("The self-update request is invalid")]
    InvalidApplyRequest,
    #[error("Drydock did not close in time")]
    PreviousProcessRunning,
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
    fn versions_are_strict_and_ordered() {
        assert!(AppVersion::parse(APP_VERSION).is_ok());
        assert!(AppVersion::parse("1.2").is_err());
        assert!(AppVersion::parse("1.2.3.4").is_err());
        assert!(AppVersion::parse("not-a-version").is_err());
        assert!(AppVersion::parse("v1.2.3").expect("version") > AppVersion::parse("1.2.2").expect("version"));
    }

    #[test]
    fn release_asset_and_checksum_are_selected_strictly() {
        let release: GitHubRelease = serde_json::from_str(
            r#"{"tag_name":"v1.2.0","assets":[{"name":"Drydock-linux-x64","browser_download_url":"https://github.com/NhMarco/Drydock/releases/download/v1.2.0/Drydock-linux-x64"}]}"#,
        )
        .expect("release");
        assert_eq!(
            asset_url(&release, "Drydock-linux-x64").expect("asset"),
            "https://github.com/NhMarco/Drydock/releases/download/v1.2.0/Drydock-linux-x64"
        );
        assert!(asset_url(&release, "Drydock-windows-x64.exe").is_err());
        assert_eq!(
            parse_checksum(b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  file")
                .expect("hash"),
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
        );
        assert!(parse_checksum(b"abc").is_err());
    }

    #[test]
    fn accepts_canonical_and_platform_asset_names() {
        let canonical = if cfg!(windows) { "Drydock.exe" } else { "Drydock" };
        assert!(is_installed_binary_name(canonical));
        // The directly downloaded release asset for this platform must self-update in place.
        assert!(is_installed_binary_name(
            target_asset_name().expect("this test runs on a supported platform")
        ));
        assert!(!is_installed_binary_name("notepad.exe"));
        assert!(!is_installed_binary_name("Drydock-unknown-x64"));
    }

    #[test]
    fn repository_name_is_constrained() {
        assert!(valid_repository("NhMarco/Drydock"));
        assert!(!valid_repository("NhMarco/Drydock/extra"));
        assert!(!valid_repository("https://github.com/NhMarco/Drydock"));
    }

    #[test]
    fn updater_assets_match_every_native_release_workflow_target() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let release =
            fs::read_to_string(root.join(".github/workflows/release.yml")).expect("release workflow");
        let validation = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("CI workflow");
        for (os, architecture, asset, runner) in [
            ("windows", "x86_64", "Drydock-windows-x64.exe", "windows-2025"),
            (
                "windows",
                "aarch64",
                "Drydock-windows-arm64.exe",
                "windows-11-arm",
            ),
            ("linux", "x86_64", "Drydock-linux-x64", "ubuntu-24.04"),
            ("linux", "aarch64", "Drydock-linux-arm64", "ubuntu-24.04-arm"),
        ] {
            assert_eq!(asset_name_for(os, architecture), Some(asset));
            assert!(release.contains(&format!("asset: {asset}")));
            assert!(release.contains(&format!("runner: {runner}")));
            assert!(validation.contains(&format!("runner: {runner}")));
        }
        assert!(release.contains("needs: build"));
        assert!(release.contains("DRYDOCK_RELEASE_VERSION"));
        assert!(release.contains("sha256sum"));
    }

    #[test]
    fn staged_executable_replaces_target_without_leaving_backup() {
        let directory = tempfile::tempdir().expect("tempdir");
        let source = directory.path().join("new.exe");
        let target = directory.path().join("Drydock.exe");
        fs::write(&source, b"new release").expect("source");
        fs::write(&target, b"previous release").expect("target");

        replace_executable(&source, &target).expect("replace");

        assert_eq!(fs::read(&target).expect("new target"), b"new release");
        assert!(!source.exists());
        assert!(!target.with_extension("exe.previous").exists());
    }
}
