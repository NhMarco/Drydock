use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, USER_AGENT};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::version::user_agent;

const MAXIMUM_CATALOG_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct CatalogApp {
    pub app_id: u32,
    pub name: String,
    /// True for apps whose activation this loader can perform (DRM-protected in the source).
    #[serde(default)]
    pub drm: bool,
    /// Store genre tags (e.g. "Indie", "Strategy") used by the search filter.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct AppCatalog {
    pub source_tree_sha: String,
    pub names_database_sha: String,
    pub apps: Vec<CatalogApp>,
}

impl AppCatalog {
    pub fn embedded() -> Result<Self, CatalogError> {
        Self::parse(include_bytes!("../../../assets/supported-apps.json"))
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, CatalogError> {
        if bytes.is_empty() || bytes.len() > MAXIMUM_CATALOG_BYTES {
            return Err(CatalogError::InvalidSize);
        }
        let catalog: Self = serde_json::from_slice(bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn load_cache(path: &Path) -> Result<Self, CatalogError> {
        Self::parse(&fs::read(path)?)
    }

    pub fn save_cache(&self, path: &Path) -> Result<(), CatalogError> {
        self.validate()?;
        let parent = path.parent().ok_or(CatalogError::InvalidCachePath)?;
        fs::create_dir_all(parent)?;
        let temporary = path.with_extension(format!("{}.new", std::process::id()));
        let result = (|| {
            fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
            if !path.exists() {
                fs::rename(&temporary, path)?;
                return Ok(());
            }
            let backup = path.with_extension("json.bak");
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(path, &backup)?;
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::rename(&backup, path);
                return Err(error.into());
            }
            let _ = fs::remove_file(backup);
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn validate(&self) -> Result<(), CatalogError> {
        if !valid_sha(&self.source_tree_sha) || !valid_sha(&self.names_database_sha) {
            return Err(CatalogError::InvalidSource);
        }
        let mut ids = BTreeSet::new();
        for app in &self.apps {
            if app.app_id == 0
                || app.name.trim().is_empty()
                || app.name.len() > 200
                || !ids.insert(app.app_id)
            {
                return Err(CatalogError::InvalidApp(app.app_id));
            }
        }
        if self.apps.is_empty() {
            return Err(CatalogError::Empty);
        }
        Ok(())
    }
}

pub struct RemoteCatalogClient {
    repository: String,
    client: Client,
}

impl RemoteCatalogClient {
    pub fn new(repository: &str) -> Result<Self, CatalogError> {
        if !valid_repository(repository) {
            return Err(CatalogError::InvalidRepository);
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self {
            repository: repository.to_owned(),
            client,
        })
    }

    pub fn fetch(&self) -> Result<AppCatalog, CatalogError> {
        let url = format!(
            "https://api.github.com/repos/{}/contents/assets/supported-apps.json",
            self.repository
        );
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent())?);
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github.raw+json"),
        );
        headers.insert("X-GitHub-Api-Version", HeaderValue::from_static("2022-11-28"));
        if let Some(token) = crate::config::github_token() {
            headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {token}"))?);
        }
        let response = self.client.get(url).headers(headers).send()?.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length == 0 || length > MAXIMUM_CATALOG_BYTES as u64)
        {
            return Err(CatalogError::InvalidSize);
        }
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or(64 * 1024)
                .min(MAXIMUM_CATALOG_BYTES as u64) as usize,
        );
        response
            .take((MAXIMUM_CATALOG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        AppCatalog::parse(&bytes)
    }
}

/// Persists a plain catalog (App ID + name) fetched from the Ryuu API, atomically.
pub fn save_catalog_apps(path: &Path, apps: &[CatalogApp]) -> Result<(), CatalogError> {
    let parent = path.parent().ok_or(CatalogError::InvalidCachePath)?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    let result = (|| {
        fs::write(&temporary, serde_json::to_vec(apps)?)?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Loads a plain catalog previously written by [`save_catalog_apps`].
pub fn load_catalog_apps(path: &Path) -> Result<Vec<CatalogApp>, CatalogError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn valid_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_repository(value: &str) -> bool {
    let mut parts = value.split('/');
    matches!((parts.next(), parts.next(), parts.next()), (Some(owner), Some(repository), None)
        if !owner.is_empty() && !repository.is_empty()
        && owner.bytes().all(valid_repository_character)
        && repository.bytes().all(valid_repository_character))
}

fn valid_repository_character(value: u8) -> bool {
    value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.')
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("The embedded app catalog is empty")]
    Empty,
    #[error("The embedded app catalog source metadata is invalid")]
    InvalidSource,
    #[error("The embedded app catalog contains an invalid or duplicate App ID: {0}")]
    InvalidApp(u32),
    #[error("The app catalog has an invalid size")]
    InvalidSize,
    #[error("The catalog repository must use the owner/repository format")]
    InvalidRepository,
    #[error("The app catalog cache path is invalid")]
    InvalidCachePath,
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
    fn embedded_catalog_is_complete_and_named() {
        let catalog = AppCatalog::embedded().expect("catalog");
        assert_eq!(catalog.apps.len(), 62);
        assert_eq!(
            catalog
                .apps
                .iter()
                .find(|app| app.app_id == 1_113_000)
                .map(|app| app.name.as_str()),
            Some("Persona 4 Golden")
        );
    }

    #[test]
    fn catalog_rejects_invalid_source_hashes_and_repositories() {
        let mut catalog = AppCatalog::embedded().expect("catalog");
        catalog.source_tree_sha = "z".repeat(40);
        assert!(matches!(catalog.validate(), Err(CatalogError::InvalidSource)));
        assert!(RemoteCatalogClient::new("owner/repository").is_ok());
        assert!(matches!(
            RemoteCatalogClient::new("https://github.com/owner/repository"),
            Err(CatalogError::InvalidRepository)
        ));
    }

    #[test]
    fn catalog_cache_round_trips_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("catalog.json");
        let catalog = AppCatalog::embedded().expect("catalog");
        catalog.save_cache(&path).expect("save cache");
        assert_eq!(AppCatalog::load_cache(&path).expect("load cache"), catalog);
        assert!(
            !path
                .with_extension(format!("{}.new", std::process::id()))
                .exists()
        );
        assert!(!path.with_extension("json.bak").exists());
    }
}
