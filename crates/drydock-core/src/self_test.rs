use std::fs;
use std::path::PathBuf;

use thiserror::Error;

use crate::{ActivationRequestService, AppCatalog, Settings};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelfTestReport {
    pub target: String,
    pub catalog_apps: usize,
    pub activation_request_created: bool,
    pub settings_round_trip: bool,
}

pub fn run_self_test() -> Result<SelfTestReport, SelfTestError> {
    if !matches!(std::env::consts::OS, "windows" | "linux")
        || !matches!(std::env::consts::ARCH, "x86_64" | "aarch64")
    {
        return Err(SelfTestError::UnsupportedTarget {
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        });
    }

    let executable = std::env::current_exe()?;
    if !executable.is_file() || fs::metadata(&executable)?.len() == 0 {
        return Err(SelfTestError::MissingExecutable(executable));
    }

    let catalog = AppCatalog::embedded()?;
    let directory = temporary_test_directory();
    fs::create_dir_all(&directory)?;
    let result = (|| {
        let settings_path = directory.join("settings").join("settings.json");
        let settings = Settings {
            games_directory: "self-test".into(),
            ..Settings::default()
        };
        settings.save(&settings_path)?;
        let loaded = Settings::load(&settings_path)?;
        if loaded.games_directory != "self-test" {
            return Err(SelfTestError::SettingsMismatch);
        }

        let activation = ActivationRequestService::new(directory.join("activation"))?;
        let request = activation.create_request_code(1_113_000)?;
        if !request.starts_with("CSL1.") || request.split('.').count() != 3 {
            return Err(SelfTestError::ActivationRequest);
        }

        Ok(SelfTestReport {
            target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            catalog_apps: catalog.apps.len(),
            activation_request_created: true,
            settings_round_trip: true,
        })
    })();
    let _ = fs::remove_dir_all(&directory);
    result
}

fn temporary_test_directory() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!("Drydock-self-test-{}-{stamp}", std::process::id()))
}

#[derive(Debug, Error)]
pub enum SelfTestError {
    #[error("Unsupported release target: {os}/{architecture}")]
    UnsupportedTarget { os: String, architecture: String },
    #[error("The running executable is missing or empty: {0}")]
    MissingExecutable(PathBuf),
    #[error("The settings round-trip changed data")]
    SettingsMismatch,
    #[error("The activation request self-test returned an invalid envelope")]
    ActivationRequest,
    #[error(transparent)]
    Catalog(#[from] crate::CatalogError),
    #[error(transparent)]
    Settings(#[from] crate::SettingsError),
    #[error(transparent)]
    Activation(#[from] crate::ActivationError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_self_test_exercises_portable_startup_state() {
        let report = run_self_test().expect("self test");
        assert_eq!(report.catalog_apps, 62);
        assert!(report.activation_request_created);
        assert!(report.settings_round_trip);
    }
}
