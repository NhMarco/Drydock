use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand::RngCore as _;
use rand::distributions::{Alphanumeric, DistString};
use rand::rngs::OsRng;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use rsa::{Oaep, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::version::user_agent;

/// Wire constants. These are shared with the activation bot and are **not** branding: renaming one
/// silently breaks activation for everybody, because the bot compares the bytes and nothing else.
/// `CSL1` and `CSLTKN1\0` predate even the TIDES name. Leave them alone.
const REQUEST_PREFIX: &str = "CSL1";
/// Product discriminator embedded in a Ubisoft activation request (Steam requests omit it).
const PRODUCT_UBISOFT: &str = "ubisoft";
/// AES-GCM additional authenticated data for the request envelope, which the bot must supply
/// byte-identically for the tag to verify. The rename to `Drydock` shipped in v1.0.0 and the bot
/// follows it; it also still accepts the old `TIDES-` value so pre-rename clients keep working.
const REQUEST_AAD: &[u8] = b"Drydock-ACTIVATION-REQUEST-V1";
const SERVICE_HOST: &str = "paste.rtech.support";
const CODE_LENGTH: usize = 8;
const TOKEN_MAGIC: &[u8; 8] = b"CSLTKN1\0";
const MAXIMUM_TOKEN_BYTES: usize = 30 * 1024 * 1024;
const MAXIMUM_METADATA_BYTES: usize = 128 * 1024;
/// The decrypted payload is a zip of game-folder files; cap its expanded size and file count.
const MAXIMUM_PAYLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAXIMUM_TOKEN_FILES: usize = 50_000;
/// Hidden folder in the game directory that records a successful activation. A token payload may
/// not write into it.
const MARKER_DIRECTORY: &str = ".Drydock";
const SIGNING_PUBLIC_KEY_SPKI: &str = "MIIBojANBgkqhkiG9w0BAQEFAAOCAY8AMIIBigKCAYEAltx1ISsLBDtHDIrcX7pTBbgmD95eBo8d/tsPZ5kQdGGKn0mOq990nI1y38d7pYgLkEixBcI15X/TjMlOXgfPRZv0+Q3KzrGc7kc7rted9YyxfYbfSCk3BRYyJnQIfgT46ujPKp0WBr2kkqx2IjHB1UurwWHLAzb1OrLQ8bt32kLgCyq2XsLmyo/Nz4zNiKYcZDVnClFRZX4ddbbsoSUZ4r1eBtRBLWynyQsv2J886XVY3LkwFPSM3JOstwSdaSsNnv0vlB3+h9syuR6vT50Oqb7U8abXvddlD/JoS3K/2XH4ISY1sVDKYXyxM1jpVtsIg5o9EAp7/F+A8QYE7X2uGzrGS+0K5o5uZjuYjJL8kSKhbRNyqehBXoXps9MWZ+wFrBdvIntxubgv1sbCz8cacLlf1v94oGv3xxcuNgF6kYCmDSrjQE2DF80EE6y8VfAnZyV9XkQvbTnlBwEn9P7yjHxVP6c4C+RJQmChqn3eAxIGa6Hat2D5la/i5x15dpudAgMBAAE=";

pub struct ActivationRequestService {
    settings_directory: PathBuf,
    client: Client,
}

impl ActivationRequestService {
    pub fn new(settings_directory: impl Into<PathBuf>) -> Result<Self, ActivationError> {
        let client = Client::builder()
            .user_agent(user_agent())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(8))
            .timeout(std::time::Duration::from_secs(25))
            .build()?;
        Ok(Self {
            settings_directory: settings_directory.into(),
            client,
        })
    }

    pub fn generate_delivery_code(&self, app_id: u32) -> Result<String, ActivationError> {
        self.deliver(self.create_request_code(app_id)?)
    }

    /// Like [`Self::generate_delivery_code`] but for Ubisoft activation: the request additionally
    /// carries the game's `token_req.txt` text so staff can mint the matching token. The request is
    /// still machine/App-bound and encrypted to the authority exactly like the Steam flow.
    pub fn generate_ubisoft_delivery_code(
        &self,
        app_id: u32,
        token_request: &str,
    ) -> Result<String, ActivationError> {
        self.deliver(self.create_ubisoft_request_code(app_id, token_request)?)
    }

    /// Encrypts a request code to the authority key and uploads it to the paste service, returning
    /// the short code; falls back to the full request string if the upload fails.
    fn deliver(&self, request: String) -> Result<String, ActivationError> {
        let authority = RsaPublicKey::from_public_key_der(&STANDARD.decode(SIGNING_PUBLIC_KEY_SPKI)?)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let encrypted = encrypt_request(&request, &authority)?;
        Ok(self.upload(&encrypted).unwrap_or(request))
    }

    /// Downloads the response token for `response_code`, verifies it for `app_id`, and installs
    /// its payload into `target_root` (the game's install directory).
    pub fn download_and_install(
        &self,
        response_code: &str,
        app_id: u32,
        target_root: &Path,
    ) -> Result<VerifiedEntitlement, ActivationError> {
        let token = self.download_token(response_code)?;
        self.install_token(&token, app_id, target_root)
    }

    /// Fetches the raw response-token bytes from the paste service.
    fn download_token(&self, response_code: &str) -> Result<Vec<u8>, ActivationError> {
        let code = normalize_short_code(response_code)?;
        let url = format!("https://{SERVICE_HOST}/selif/{}.token", code.to_ascii_lowercase());
        let response = self
            .client
            .get(url)
            .header("Linx-Access-Key", access_key(&code))
            .send()?;
        ensure_service_url(response.url())?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(ActivationError::ResponseNotFound);
        }
        let response = response.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length == 0 || length > MAXIMUM_TOKEN_BYTES as u64)
        {
            return Err(ActivationError::InvalidTokenSize);
        }
        let mut token = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or(64 * 1024)
                .min(MAXIMUM_TOKEN_BYTES as u64) as usize,
        );
        response
            .take((MAXIMUM_TOKEN_BYTES + 1) as u64)
            .read_to_end(&mut token)?;
        if token.is_empty() || token.len() > MAXIMUM_TOKEN_BYTES {
            return Err(ActivationError::InvalidTokenSize);
        }
        Ok(token)
    }

    /// Verifies the token for `app_id`, then extracts its payload archive into `target_root` and
    /// writes an activation marker. The decrypted payload is wiped from memory before returning.
    pub fn install_token(
        &self,
        token: &[u8],
        app_id: u32,
        target_root: &Path,
    ) -> Result<VerifiedEntitlement, ActivationError> {
        if !target_root.is_dir() {
            return Err(ActivationError::AppFolderMissing);
        }
        let authority = RsaPublicKey::from_public_key_der(&STANDARD.decode(SIGNING_PUBLIC_KEY_SPKI)?)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let (entitlement, machine_id, mut payload) = self.verify_and_decrypt(token, app_id, &authority)?;
        let staging_parent = self.settings_directory.join("activation-staging");
        let installed = install_activation_archive(
            &payload,
            target_root,
            &staging_parent,
            app_id,
            &machine_id,
            &entitlement.expires_utc,
        );
        payload.fill(0);
        installed?;
        Ok(entitlement)
    }

    pub fn verify_token(&self, token: &[u8], app_id: u32) -> Result<VerifiedEntitlement, ActivationError> {
        let authority = RsaPublicKey::from_public_key_der(&STANDARD.decode(SIGNING_PUBLIC_KEY_SPKI)?)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        self.verify_token_with_authority(token, app_id, &authority)
    }

    fn verify_token_with_authority(
        &self,
        token: &[u8],
        app_id: u32,
        authority: &RsaPublicKey,
    ) -> Result<VerifiedEntitlement, ActivationError> {
        let (entitlement, _machine_id, mut payload) = self.verify_and_decrypt(token, app_id, authority)?;
        // Verify-only path (diagnostics/tests): never keep the decrypted payload around.
        payload.fill(0);
        Ok(entitlement)
    }

    /// Verifies the signed token and decrypts its payload archive. Returns the entitlement, the
    /// machine id it is bound to (for the activation marker), and the decrypted payload bytes.
    /// Callers own the payload and must wipe it once they are done.
    fn verify_and_decrypt(
        &self,
        token: &[u8],
        app_id: u32,
        authority: &RsaPublicKey,
    ) -> Result<(VerifiedEntitlement, String, Vec<u8>), ActivationError> {
        if token.len() < TOKEN_MAGIC.len() + 4 || &token[..TOKEN_MAGIC.len()] != TOKEN_MAGIC {
            return Err(ActivationError::InvalidToken);
        }
        let metadata_length = u32::from_be_bytes(
            token[TOKEN_MAGIC.len()..TOKEN_MAGIC.len() + 4]
                .try_into()
                .map_err(|_| ActivationError::InvalidToken)?,
        ) as usize;
        if metadata_length == 0 || metadata_length > MAXIMUM_METADATA_BYTES {
            return Err(ActivationError::InvalidToken);
        }
        let payload_offset = TOKEN_MAGIC.len() + 4 + metadata_length;
        if payload_offset >= token.len() {
            return Err(ActivationError::InvalidToken);
        }
        let metadata: TokenMetadata = serde_json::from_slice(&token[TOKEN_MAGIC.len() + 4..payload_offset])?;
        let ciphertext = &token[payload_offset..];
        if metadata.version != 1 {
            return Err(ActivationError::UnsupportedToken);
        }
        if metadata.app_id != app_id {
            return Err(ActivationError::WrongAppId);
        }

        let device = self.load_or_create_device()?;
        let private_der = STANDARD.decode(&device.private_key)?;
        let private = RsaPrivateKey::from_pkcs8_der(&private_der)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let public_der = private
            .to_public_key()
            .to_public_key_der()
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let expected_machine = machine_fingerprint(&device.fallback_machine_id);
        let expected_key_id = uppercase_hex(Sha256::digest(public_der.as_bytes()));
        if !constant_time_text_eq(&metadata.machine_id, &expected_machine) {
            return Err(ActivationError::WrongMachine);
        }
        if !constant_time_text_eq(&metadata.device_key_id, &expected_key_id) {
            return Err(ActivationError::WrongDevice);
        }

        let issued = metadata
            .issued_utc
            .parse::<jiff::Timestamp>()
            .map_err(|_| ActivationError::InvalidLifetime)?;
        let expires = metadata
            .expires_utc
            .parse::<jiff::Timestamp>()
            .map_err(|_| ActivationError::InvalidLifetime)?;
        if expires <= issued || jiff::Timestamp::now() > expires {
            return Err(ActivationError::InvalidLifetime);
        }

        let payload_hash = uppercase_hex(Sha256::digest(ciphertext));
        if !constant_time_text_eq(&metadata.payload_sha256, &payload_hash) {
            return Err(ActivationError::DamagedPayload);
        }
        let signed_text = format!(
            "CSLTKN1\n{}\n{}\n{}\n{}\n{}\n{}",
            metadata.app_id,
            metadata.machine_id,
            metadata.device_key_id,
            metadata.issued_utc,
            metadata.expires_utc,
            payload_hash
        );
        let signature = STANDARD.decode(&metadata.signature)?;
        authority
            .verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(signed_text.as_bytes()),
                &signature,
            )
            .map_err(|_| ActivationError::InvalidSignature)?;

        let content_key = private
            .decrypt(Oaep::new::<Sha256>(), &STANDARD.decode(&metadata.wrapped_key)?)
            .map_err(|_| ActivationError::WrongDevice)?;
        let cipher = Aes256Gcm::new_from_slice(&content_key)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let nonce = STANDARD.decode(&metadata.nonce)?;
        let tag = STANDARD.decode(&metadata.tag)?;
        if nonce.len() != 12 || tag.len() != 16 {
            return Err(ActivationError::InvalidToken);
        }
        let mut plaintext = ciphertext.to_vec();
        cipher
            .decrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                b"",
                &mut plaintext,
                tag.as_slice().into(),
            )
            .map_err(|_| ActivationError::DamagedPayload)?;
        let payload_bytes = plaintext.len();

        Ok((
            VerifiedEntitlement {
                app_id,
                issued_utc: metadata.issued_utc,
                expires_utc: metadata.expires_utc,
                payload_bytes,
            },
            metadata.machine_id,
            plaintext,
        ))
    }

    pub fn create_request_code(&self, app_id: u32) -> Result<String, ActivationError> {
        self.build_request_code(app_id, None, None)
    }

    /// Builds a Ubisoft activation request: the standard machine/App-bound request plus a
    /// `product: "ubisoft"` discriminator and the captured `token_req.txt` text.
    pub fn create_ubisoft_request_code(
        &self,
        app_id: u32,
        token_request: &str,
    ) -> Result<String, ActivationError> {
        let token_request = token_request.trim();
        if token_request.is_empty() {
            return Err(ActivationError::InvalidRequest);
        }
        self.build_request_code(app_id, Some(PRODUCT_UBISOFT), Some(token_request.to_owned()))
    }

    fn build_request_code(
        &self,
        app_id: u32,
        product: Option<&'static str>,
        token_request: Option<String>,
    ) -> Result<String, ActivationError> {
        if app_id == 0 {
            return Err(ActivationError::InvalidAppId);
        }
        let device = self.load_or_create_device()?;
        let private_der = STANDARD.decode(&device.private_key)?;
        let private = RsaPrivateKey::from_pkcs8_der(&private_der)
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let public_der = private
            .to_public_key()
            .to_public_key_der()
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let public_bytes = public_der.as_bytes();
        let machine_id = machine_fingerprint(&device.fallback_machine_id);

        let mut nonce = [0_u8; 12];
        OsRng.fill_bytes(&mut nonce);
        let body = serde_json::to_vec(&RequestDocument {
            version: 1,
            app_id,
            machine_id,
            device_public_key: STANDARD.encode(public_bytes),
            device_key_id: uppercase_hex(Sha256::digest(public_bytes)),
            issued_utc: jiff::Timestamp::now().to_string(),
            nonce: uppercase_hex(nonce),
            product,
            token_request,
        })?;
        let encoded = URL_SAFE_NO_PAD.encode(body);
        let checksum = uppercase_hex(Sha256::digest(encoded.as_bytes()));
        Ok(format!("{REQUEST_PREFIX}.{encoded}.{}", &checksum[..12]))
    }

    fn upload(&self, encrypted: &[u8]) -> Result<String, ActivationError> {
        for _ in 0..5 {
            let mut rng = OsRng;
            let code = Alphanumeric
                .sample_string(&mut rng, CODE_LENGTH)
                .to_ascii_lowercase();
            let mut headers = HeaderMap::new();
            headers.insert("Linx-Access-Key", HeaderValue::from_str(&access_key(&code))?);
            headers.insert("Linx-Expiry", HeaderValue::from_static("1800"));
            headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));

            let url = format!("https://{SERVICE_HOST}/upload/{code}.txt");
            let response = self
                .client
                .put(url)
                .headers(headers)
                .body(encrypted.to_vec())
                .send()?;
            ensure_service_url(response.url())?;
            if response.status() == reqwest::StatusCode::CONFLICT {
                continue;
            }
            let response = response.error_for_status()?;
            let upload: PasteUploadResponse = response.json()?;
            let uploaded_code = parse_upload_code(&upload, ".txt")?;
            if !uploaded_code.eq_ignore_ascii_case(&code) {
                return Err(ActivationError::UnexpectedFilename);
            }
            return Ok(uploaded_code);
        }
        Err(ActivationError::CodeAllocation)
    }

    fn load_or_create_device(&self) -> Result<DeviceDocument, ActivationError> {
        let path = self.settings_directory.join("activation-device.json");
        if path.is_file() {
            let document: DeviceDocument = serde_json::from_slice(&fs::read(&path)?)?;
            if document.private_key.trim().is_empty() || document.fallback_machine_id.trim().is_empty() {
                return Err(ActivationError::DamagedDevice);
            }
            return Ok(document);
        }

        fs::create_dir_all(&self.settings_directory)?;
        let mut rng = OsRng;
        let private =
            RsaPrivateKey::new(&mut rng, 3072).map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let private_der = private
            .to_pkcs8_der()
            .map_err(|error| ActivationError::Crypto(error.to_string()))?;
        let mut fallback = [0_u8; 32];
        rng.fill_bytes(&mut fallback);
        let document = DeviceDocument {
            private_key: STANDARD.encode(private_der.as_bytes()),
            fallback_machine_id: uppercase_hex(fallback),
        };
        write_private_file(&path, &serde_json::to_vec_pretty(&document)?)?;
        Ok(document)
    }
}

/// Extracts the decrypted payload zip into `target_root` (the game's install directory),
/// overwriting existing files, transactionally: everything is staged and validated first, then
/// applied with per-file backups so a mid-install failure rolls back to the original state.
/// Returns the number of files written. Mirrors the C# `InstallArchive`.
fn install_activation_archive(
    payload: &[u8],
    target_root: &Path,
    staging_parent: &Path,
    app_id: u32,
    machine_id: &str,
    expires_utc: &str,
) -> Result<usize, ActivationError> {
    let full_root = std::path::absolute(target_root)?;
    let staging_root = staging_parent.join(random_staging_name());
    let extracted_root = staging_root.join("files");
    let backup_root = staging_root.join("backup");

    let result = stage_and_apply(
        payload,
        &full_root,
        &extracted_root,
        &backup_root,
        app_id,
        machine_id,
        expires_utc,
    );

    // Best-effort staging cleanup; remove the shared parent only when it is now empty.
    let _ = fs::remove_dir_all(&staging_root);
    let _ = fs::remove_dir(staging_parent);
    result
}

fn stage_and_apply(
    payload: &[u8],
    full_root: &Path,
    extracted_root: &Path,
    backup_root: &Path,
    app_id: u32,
    machine_id: &str,
    expires_utc: &str,
) -> Result<usize, ActivationError> {
    fs::create_dir_all(extracted_root)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(payload))?;
    if archive.len() > MAXIMUM_TOKEN_FILES {
        return Err(ActivationError::TooManyTokenFiles);
    }

    // Stage and validate every entry before touching the game folder.
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut expanded: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        if entry.is_dir() {
            continue;
        }
        let Some(relative) = entry.enclosed_name() else {
            return Err(ActivationError::UnsafeTokenPath(entry.name().to_owned()));
        };
        if touches_marker_directory(&relative) {
            return Err(ActivationError::TokenTouchesMarker);
        }
        expanded = expanded.saturating_add(entry.size());
        if expanded > MAXIMUM_PAYLOAD_BYTES {
            return Err(ActivationError::PayloadTooLarge);
        }
        let target = full_root.join(&relative);
        if !target.starts_with(full_root) {
            return Err(ActivationError::UnsafeTokenPath(entry.name().to_owned()));
        }
        if !seen.insert(target.to_string_lossy().to_ascii_lowercase()) {
            return Err(ActivationError::DuplicateTokenPath(entry.name().to_owned()));
        }
        ensure_no_reparse_points(full_root, &target)?;
        let staged = extracted_root.join(&relative);
        if let Some(parent) = staged.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&staged)?;
        std::io::copy(&mut entry, &mut output)?;
        files.push((staged, target));
    }
    if files.is_empty() {
        return Err(ActivationError::TokenHasNoFiles);
    }

    // Apply with rollback. `changed` records every touched target and its backup (if any).
    let mut changed: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let applied = apply_staged_files(full_root, backup_root, &files, &mut changed)
        .and_then(|()| write_activation_marker(full_root, app_id, machine_id, expires_utc, files.len()));
    if let Err(error) = applied {
        for (target, backup) in changed.iter().rev() {
            match backup {
                Some(backup) => {
                    let _ = fs::copy(backup, target);
                }
                None => {
                    let _ = fs::remove_file(target);
                }
            }
        }
        return Err(error);
    }
    Ok(files.len())
}

fn apply_staged_files(
    full_root: &Path,
    backup_root: &Path,
    files: &[(PathBuf, PathBuf)],
    changed: &mut Vec<(PathBuf, Option<PathBuf>)>,
) -> Result<(), ActivationError> {
    // Back up and remove any pre-existing proxy DLLs so the token's own copies win cleanly.
    for name in ["version.dll", "winmm.dll"] {
        let target = full_root.join(name);
        if !target.is_file() {
            continue;
        }
        let backup = backup_root.join("preexisting-proxy").join(name);
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&target, &backup)?;
        changed.push((target.clone(), Some(backup)));
        fs::remove_file(&target)?;
    }

    for (staged, target) in files {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let backup = if target.is_file() {
            let relative = target.strip_prefix(full_root).unwrap_or(target);
            let backup = backup_root.join(relative);
            if let Some(parent) = backup.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(target, &backup)?;
            Some(backup)
        } else {
            None
        };
        changed.push((target.clone(), backup));
        fs::copy(staged, target)?;
    }
    Ok(())
}

/// Rejects a token target whose path passes through a symbolic link or junction, so a planted
/// reparse point in the game folder cannot redirect a write outside it.
fn ensure_no_reparse_points(root: &Path, target: &Path) -> Result<(), ActivationError> {
    let Ok(relative) = target.strip_prefix(root) else {
        return Ok(());
    };
    let mut current = root.to_path_buf();
    for segment in relative.components() {
        current.push(segment);
        if is_reparse_point(&current) {
            return Err(ActivationError::ReparsePointInPath(
                relative.display().to_string(),
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_reparse_point(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn touches_marker_directory(relative: &Path) -> bool {
    relative.components().next().is_some_and(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(MARKER_DIRECTORY)
    })
}

fn random_staging_name() -> String {
    let mut bytes = [0_u8; 16];
    OsRng.fill_bytes(&mut bytes);
    uppercase_hex(bytes)
}

/// Records a successful activation in `<game>/.Drydock/activation.json`.
fn write_activation_marker(
    root: &Path,
    app_id: u32,
    machine_id: &str,
    expires_utc: &str,
    installed_files: usize,
) -> Result<(), ActivationError> {
    let directory = root.join(MARKER_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let marker = ActivationMarker {
        version: 1,
        app_id,
        machine_id: machine_id.to_owned(),
        activated_utc: jiff::Timestamp::now().to_string(),
        token_expires_utc: expires_utc.to_owned(),
        installed_files,
    };
    let path = directory.join("activation.json");
    let temporary = path.with_extension(format!("json.{}.new", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(&marker)?)?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn encrypt_request(request: &str, authority: &RsaPublicKey) -> Result<Vec<u8>, ActivationError> {
    if !request.starts_with("CSL1.") {
        return Err(ActivationError::InvalidRequest);
    }
    let mut rng = OsRng;
    let mut content_key = [0_u8; 32];
    let mut nonce = [0_u8; 12];
    rng.fill_bytes(&mut content_key);
    rng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new_from_slice(&content_key)
        .map_err(|error| ActivationError::Crypto(error.to_string()))?;
    let mut ciphertext = request.as_bytes().to_vec();
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), REQUEST_AAD, &mut ciphertext)
        .map_err(|error| ActivationError::Crypto(error.to_string()))?;
    let wrapped_key = authority
        .encrypt(&mut rng, Oaep::new::<Sha256>(), &content_key)
        .map_err(|error| ActivationError::Crypto(error.to_string()))?;
    content_key.fill(0);

    Ok(serde_json::to_vec(&RequestEnvelope {
        version: 1,
        wrapped_key: STANDARD.encode(wrapped_key),
        nonce: STANDARD.encode(nonce),
        tag: STANDARD.encode(tag),
        ciphertext: STANDARD.encode(ciphertext),
    })?)
}

pub fn normalize_short_code(value: &str) -> Result<String, ActivationError> {
    let code = value.trim().trim_matches('`');
    if !(6..=32).contains(&code.len()) || !code.chars().all(|character| character.is_ascii_alphanumeric()) {
        return Err(ActivationError::InvalidShortCode);
    }
    Ok(code.to_ascii_uppercase())
}

fn access_key(code: &str) -> String {
    code.to_ascii_uppercase().chars().rev().collect()
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

fn parse_upload_code(upload: &PasteUploadResponse, extension: &str) -> Result<String, ActivationError> {
    if !upload.filename.ends_with(extension) {
        return Err(ActivationError::UnexpectedFilename);
    }
    let direct = Url::parse(&upload.direct_url).map_err(|_| ActivationError::UnsafeServiceUrl)?;
    ensure_service_url(&direct)?;
    if !direct.path().starts_with("/selif/") || direct.query().is_some() || direct.fragment().is_some() {
        return Err(ActivationError::UnsafeServiceUrl);
    }
    let filename = direct
        .path_segments()
        .and_then(Iterator::last)
        .ok_or(ActivationError::UnsafeServiceUrl)?;
    if !filename.eq_ignore_ascii_case(&upload.filename) {
        return Err(ActivationError::UnexpectedFilename);
    }
    normalize_short_code(filename.trim_end_matches(extension))
}

fn ensure_service_url(url: &Url) -> Result<(), ActivationError> {
    if url.scheme() != "https" || url.host_str() != Some(SERVICE_HOST) {
        return Err(ActivationError::UnsafeServiceUrl);
    }
    Ok(())
}

fn machine_fingerprint(fallback: &str) -> String {
    let raw = platform_machine_id().unwrap_or_else(|| fallback.trim().to_owned());
    uppercase_hex(Sha256::digest(format!("Drydock|{raw}").as_bytes()))
}

#[cfg(windows)]
fn platform_machine_id() -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\Microsoft\\Cryptography")
        .ok()?
        .get_value::<String, _>("MachineGuid")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(not(windows))]
fn platform_machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), ActivationError> {
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
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

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DeviceDocument {
    private_key: String,
    fallback_machine_id: String,
}

#[derive(Serialize)]
struct RequestDocument {
    #[serde(rename = "v")]
    version: u8,
    app_id: u32,
    machine_id: String,
    device_public_key: String,
    device_key_id: String,
    issued_utc: String,
    nonce: String,
    // Absent for Steam (so its request bytes are unchanged); "ubisoft" for the Ubisoft flow, whose
    // request also carries the game's token_req.txt text for staff to answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    product: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_request: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct RequestEnvelope {
    #[serde(rename = "v")]
    version: u8,
    wrapped_key: String,
    nonce: String,
    tag: String,
    ciphertext: String,
}

#[derive(Debug, Deserialize)]
struct TokenMetadata {
    #[serde(rename = "v")]
    version: u8,
    app_id: u32,
    machine_id: String,
    device_key_id: String,
    issued_utc: String,
    expires_utc: String,
    payload_sha256: String,
    wrapped_key: String,
    nonce: String,
    tag: String,
    signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedEntitlement {
    pub app_id: u32,
    pub issued_utc: String,
    pub expires_utc: String,
    pub payload_bytes: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
struct ActivationMarker {
    version: u8,
    app_id: u32,
    machine_id: String,
    activated_utc: String,
    token_expires_utc: String,
    installed_files: usize,
}

#[derive(Deserialize)]
struct PasteUploadResponse {
    filename: String,
    direct_url: String,
}

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error("The selected app has no App ID")]
    InvalidAppId,
    #[error("The local activation device identity is damaged")]
    DamagedDevice,
    #[error("The activation request is invalid")]
    InvalidRequest,
    #[error("Enter only the short activation code, not a link")]
    InvalidShortCode,
    #[error("The activation service returned an unsafe address")]
    UnsafeServiceUrl,
    #[error("The activation service returned an unexpected filename")]
    UnexpectedFilename,
    #[error("The activation service could not allocate a unique short code")]
    CodeAllocation,
    #[error("The response code was not found or has expired")]
    ResponseNotFound,
    #[error("The activation response has an invalid size")]
    InvalidTokenSize,
    #[error("This is not a valid Drydock activation response")]
    InvalidToken,
    #[error("This activation response version is not supported")]
    UnsupportedToken,
    #[error("The activation response belongs to a different App ID")]
    WrongAppId,
    #[error("The activation response belongs to a different machine")]
    WrongMachine,
    #[error("The activation response belongs to a different Drydock installation")]
    WrongDevice,
    #[error("The activation response has expired or carries an invalid lifetime")]
    InvalidLifetime,
    #[error("The activation response payload is damaged")]
    DamagedPayload,
    #[error("The activation response signature is invalid")]
    InvalidSignature,
    #[error("The game folder was not found. Install the game through Steam first.")]
    AppFolderMissing,
    #[error("The activation response contains too many files")]
    TooManyTokenFiles,
    #[error("The activation payload is too large")]
    PayloadTooLarge,
    #[error("The activation response contains no files to install")]
    TokenHasNoFiles,
    #[error("The activation response contains an unsafe path: {0}")]
    UnsafeTokenPath(String),
    #[error("The activation response lists the same path twice: {0}")]
    DuplicateTokenPath(String),
    #[error("The activation response may not modify protected metadata")]
    TokenTouchesMarker,
    #[error("An activation target passes through a symbolic link: {0}")]
    ReparsePointInPath(String),
    #[error("Activation cryptography failed: {0}")]
    Crypto(String),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Header(#[from] reqwest::header::InvalidHeaderValue),
    #[error(transparent)]
    Base64(#[from] base64::DecodeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn signed_token(
        service: &ActivationRequestService,
        app_id: u32,
        authority: &RsaPrivateKey,
        machine_override: Option<&str>,
    ) -> Vec<u8> {
        let request = service.create_request_code(app_id).expect("request");
        let encoded = request.split('.').nth(1).expect("request body");
        let request: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).expect("base64")).expect("request json");
        let device_public = RsaPublicKey::from_public_key_der(
            &STANDARD
                .decode(request["device_public_key"].as_str().expect("device key"))
                .expect("device key base64"),
        )
        .expect("device public key");

        let mut rng = OsRng;
        let mut key = [0_u8; 32];
        let mut nonce = [0_u8; 12];
        rng.fill_bytes(&mut key);
        rng.fill_bytes(&mut nonce);
        let cipher = Aes256Gcm::new_from_slice(&key).expect("cipher");
        let mut ciphertext = b"verified entitlement payload".to_vec();
        let tag = cipher
            .encrypt_in_place_detached(Nonce::from_slice(&nonce), b"", &mut ciphertext)
            .expect("encrypt");
        let wrapped_key = device_public
            .encrypt(&mut rng, Oaep::new::<Sha256>(), &key)
            .expect("wrap key");
        let payload_hash = uppercase_hex(Sha256::digest(&ciphertext));
        let issued = "2026-01-01T00:00:00Z";
        let expires = "2099-01-01T00:00:00Z";
        let machine_id =
            machine_override.unwrap_or_else(|| request["machine_id"].as_str().expect("machine fingerprint"));
        let signed_text = format!(
            "CSLTKN1\n{app_id}\n{}\n{}\n{issued}\n{expires}\n{payload_hash}",
            machine_id,
            request["device_key_id"].as_str().expect("key id"),
        );
        let signature = authority
            .sign(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(signed_text.as_bytes()),
            )
            .expect("sign");
        let metadata = serde_json::to_vec(&json!({
            "v": 1,
            "app_id": app_id,
            "machine_id": machine_id,
            "device_key_id": request["device_key_id"],
            "issued_utc": issued,
            "expires_utc": expires,
            "payload_sha256": payload_hash,
            "wrapped_key": STANDARD.encode(wrapped_key),
            "nonce": STANDARD.encode(nonce),
            "tag": STANDARD.encode(tag),
            "signature": STANDARD.encode(signature),
        }))
        .expect("metadata");

        let mut token = TOKEN_MAGIC.to_vec();
        token.extend_from_slice(&(metadata.len() as u32).to_be_bytes());
        token.extend_from_slice(&metadata);
        token.extend_from_slice(&ciphertext);
        token
    }

    #[test]
    fn request_is_app_bound_and_checksummed() {
        let root = tempfile::tempdir().expect("tempdir");
        let service = ActivationRequestService::new(root.path()).expect("service");
        let request = service.create_request_code(42).expect("request");
        let parts: Vec<_> = request.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "CSL1");
        let body: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).expect("body")).expect("json");
        assert_eq!(body["app_id"], 42);
        let checksum = uppercase_hex(Sha256::digest(parts[1].as_bytes()));
        assert_eq!(parts[2], &checksum[..12]);
    }

    #[test]
    fn steam_request_omits_ubisoft_fields_but_ubisoft_request_carries_them() {
        let root = tempfile::tempdir().expect("tempdir");
        let service = ActivationRequestService::new(root.path()).expect("service");

        let steam = service.create_request_code(42).expect("steam request");
        let steam_body: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(steam.split('.').nth(1).unwrap()).unwrap())
                .expect("steam json");
        assert!(steam_body.get("product").is_none());
        assert!(steam_body.get("token_request").is_none());

        let ubi = service
            .create_ubisoft_request_code(42, "  ubi-token-request-blob  ")
            .expect("ubisoft request");
        let parts: Vec<_> = ubi.split('.').collect();
        assert_eq!(parts[0], "CSL1");
        assert_eq!(
            &uppercase_hex(Sha256::digest(parts[1].as_bytes()))[..12],
            parts[2]
        );
        let ubi_body: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).expect("ubisoft json");
        assert_eq!(ubi_body["product"], "ubisoft");
        assert_eq!(ubi_body["token_request"], "ubi-token-request-blob");
        assert_eq!(ubi_body["app_id"], 42);

        assert!(matches!(
            service.create_ubisoft_request_code(42, "   "),
            Err(ActivationError::InvalidRequest)
        ));
    }

    #[test]
    fn request_envelope_round_trips_with_authority_key() {
        let mut rng = OsRng;
        let authority = RsaPrivateKey::new(&mut rng, 2048).expect("authority");
        let encoded = encrypt_request("CSL1.example.checksum", &authority.to_public_key()).expect("encrypt");
        let envelope: RequestEnvelope = serde_json::from_slice(&encoded).expect("envelope");
        let content_key = authority
            .decrypt(
                Oaep::new::<Sha256>(),
                &STANDARD.decode(envelope.wrapped_key).expect("wrapped key"),
            )
            .expect("decrypt key");
        let cipher = Aes256Gcm::new_from_slice(&content_key).expect("cipher");
        let mut plaintext = STANDARD.decode(envelope.ciphertext).expect("ciphertext");
        let nonce = STANDARD.decode(envelope.nonce).expect("nonce");
        let tag = STANDARD.decode(envelope.tag).expect("tag");
        cipher
            .decrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                REQUEST_AAD,
                &mut plaintext,
                tag.as_slice().into(),
            )
            .expect("decrypt request");
        assert_eq!(plaintext, b"CSL1.example.checksum");
    }

    #[test]
    fn the_wire_constants_are_pinned_to_what_the_bot_expects() {
        // These bytes are the protocol, not the product name. A rename swept `TIDES` into the AAD
        // once already, which silently broke every activation: the bot's AES-GCM tag check fails and
        // the code is rejected with no hint as to why. If this test fails because the bot changed,
        // change the bot and the client together — never one alone.
        assert_eq!(REQUEST_AAD, b"Drydock-ACTIVATION-REQUEST-V1");
        assert_eq!(REQUEST_PREFIX, "CSL1");
        assert_eq!(TOKEN_MAGIC, b"CSLTKN1\0");
    }

    #[test]
    fn short_code_is_normalized_and_password_is_reversed() {
        assert_eq!(normalize_short_code(" `a1b2c3d4` ").expect("code"), "A1B2C3D4");
        assert_eq!(access_key("a1b2c3d4"), "4D3C2B1A");
        assert!(normalize_short_code("https://example.test").is_err());
    }

    #[test]
    fn signed_entitlement_is_verified_and_decrypted() {
        let root = tempfile::tempdir().expect("tempdir");
        let service = ActivationRequestService::new(root.path()).expect("service");
        let authority = RsaPrivateKey::new(&mut OsRng, 2048).expect("authority");
        let token = signed_token(&service, 42, &authority, None);

        let verified = service
            .verify_token_with_authority(&token, 42, &authority.to_public_key())
            .expect("verify");
        assert_eq!(verified.app_id, 42);
        assert_eq!(verified.payload_bytes, b"verified entitlement payload".len());
    }

    #[test]
    fn entitlement_rejects_wrong_app_and_machine() {
        let root = tempfile::tempdir().expect("tempdir");
        let service = ActivationRequestService::new(root.path()).expect("service");
        let authority = RsaPrivateKey::new(&mut OsRng, 2048).expect("authority");
        let token = signed_token(&service, 42, &authority, None);
        assert!(matches!(
            service.verify_token_with_authority(&token, 43, &authority.to_public_key()),
            Err(ActivationError::WrongAppId)
        ));

        let wrong_machine_token = signed_token(&service, 42, &authority, Some("WRONG-MACHINE"));
        assert!(matches!(
            service.verify_token_with_authority(&wrong_machine_token, 42, &authority.to_public_key()),
            Err(ActivationError::WrongMachine)
        ));
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, content) in entries {
                writer.start_file(*name, options).expect("start");
                Write::write_all(&mut writer, content).expect("write");
            }
            writer.finish().expect("finish");
        }
        buffer
    }

    #[test]
    fn archive_install_overwrites_and_writes_marker() {
        let game = tempfile::tempdir().expect("game");
        let staging = tempfile::tempdir().expect("staging");
        fs::write(game.path().join("steam_api64.dll"), b"original").expect("seed");
        let payload = zip_with(&[
            ("steam_api64.dll", b"unlocked"),
            ("steam_settings/config.ini", b"[cfg]"),
        ]);

        let count = install_activation_archive(
            &payload,
            game.path(),
            staging.path(),
            42,
            "MACHINE",
            "2099-01-01T00:00:00Z",
        )
        .expect("install");

        assert_eq!(count, 2);
        assert_eq!(
            fs::read(game.path().join("steam_api64.dll")).unwrap(),
            b"unlocked"
        );
        assert_eq!(
            fs::read(game.path().join("steam_settings/config.ini")).unwrap(),
            b"[cfg]"
        );
        let marker = fs::read_to_string(game.path().join(".Drydock/activation.json")).expect("marker");
        assert!(marker.contains("\"app_id\": 42"));
        assert!(marker.contains("\"installed_files\": 2"));
    }

    #[test]
    fn archive_install_rejects_zip_slip_and_marker_writes() {
        let game = tempfile::tempdir().expect("game");
        let staging = tempfile::tempdir().expect("staging");
        assert!(matches!(
            install_activation_archive(
                &zip_with(&[("../escape.dll", b"evil")]),
                game.path(),
                staging.path(),
                42,
                "M",
                "2099-01-01T00:00:00Z",
            ),
            Err(ActivationError::UnsafeTokenPath(_))
        ));
        assert!(matches!(
            install_activation_archive(
                &zip_with(&[(".Drydock/activation.json", b"forged")]),
                game.path(),
                staging.path(),
                42,
                "M",
                "2099-01-01T00:00:00Z",
            ),
            Err(ActivationError::TokenTouchesMarker)
        ));
        // Nothing was written outside the game folder, and no marker was forged.
        assert!(!game.path().parent().unwrap().join("escape.dll").exists());
        assert!(!game.path().join(".Drydock/activation.json").exists());
    }

    #[test]
    fn entitlement_rejects_tampering() {
        let root = tempfile::tempdir().expect("tempdir");
        let service = ActivationRequestService::new(root.path()).expect("service");
        let authority = RsaPrivateKey::new(&mut OsRng, 2048).expect("authority");
        let mut token = signed_token(&service, 42, &authority, None);
        *token.last_mut().expect("payload") ^= 0x40;
        assert!(matches!(
            service.verify_token_with_authority(&token, 42, &authority.to_public_key()),
            Err(ActivationError::DamagedPayload)
        ));
    }
}
