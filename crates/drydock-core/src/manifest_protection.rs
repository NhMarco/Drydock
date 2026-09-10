use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub fn updates_enabled(path: &Path) -> Result<bool, ManifestProtectionError> {
    let metadata = fs::metadata(path).map_err(|source| ManifestProtectionError::Metadata {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        Ok(metadata.permissions().mode() & 0o200 != 0)
    }

    #[cfg(not(unix))]
    Ok(!metadata.permissions().readonly())
}

pub fn set_manifest_updates_enabled(path: &Path, enabled: bool) -> Result<(), ManifestProtectionError> {
    let metadata = fs::metadata(path).map_err(|source| ManifestProtectionError::Metadata {
        path: path.to_path_buf(),
        source,
    })?;
    let mut permissions = metadata.permissions();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let current = permissions.mode();
        let updated = if enabled {
            current | 0o200
        } else {
            current & !0o222
        };
        permissions.set_mode(updated);
    }

    #[cfg(not(unix))]
    permissions.set_readonly(!enabled);

    fs::set_permissions(path, permissions).map_err(|source| ManifestProtectionError::SetPermissions {
        path: path.to_path_buf(),
        source,
    })?;

    let actual = updates_enabled(path)?;
    if actual != enabled {
        return Err(ManifestProtectionError::Verification {
            path: path.to_path_buf(),
            expected_enabled: enabled,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ManifestProtectionError {
    #[error("cannot inspect Steam manifest {path}: {source}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot change Steam manifest permissions at {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "Steam manifest permission verification failed at {path}; expected updates enabled={expected_enabled}"
    )]
    Verification { path: PathBuf, expected_enabled: bool },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggles_and_verifies_manifest_permissions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("appmanifest_111300.acf");
        fs::write(&path, "test").expect("write manifest");

        set_manifest_updates_enabled(&path, false).expect("disable updates");
        assert!(!updates_enabled(&path).expect("read disabled state"));

        set_manifest_updates_enabled(&path, true).expect("enable updates");
        assert!(updates_enabled(&path).expect("read enabled state"));
    }
}
