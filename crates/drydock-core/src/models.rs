use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AddedAppState {
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppInfo {
    pub app_id: u32,
    pub name: String,
    pub install_dir: Option<PathBuf>,
    pub manifest_path: Option<PathBuf>,
}

impl AppInfo {
    #[must_use]
    pub fn subtitle(&self) -> String {
        format!("App {}", self.app_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamManifest {
    pub app_id: u32,
    pub name: String,
    pub install_dir_name: String,
    pub library_path: PathBuf,
    pub manifest_path: PathBuf,
    pub state_flags: u64,
    pub bytes_to_download: u64,
    pub bytes_downloaded: u64,
    pub bytes_to_stage: u64,
    pub bytes_staged: u64,
    pub size_on_disk: Option<u64>,
}

impl SteamManifest {
    #[must_use]
    pub fn install_dir(&self) -> PathBuf {
        self.library_path
            .join("steamapps")
            .join("common")
            .join(&self.install_dir_name)
    }

    #[must_use]
    pub fn is_fully_installed(&self) -> bool {
        const STATE_FULLY_INSTALLED: u64 = 4;
        const INCOMPLETE_FLAGS: u64 = 1
            | 2
            | 8
            | 16
            | 32
            | 128
            | 256
            | 512
            | 1024
            | 2048
            | 4096
            | 65_536
            | 131_072
            | 262_144
            | 524_288
            | 1_048_576
            | 2_097_152
            | 4_194_304
            | 8_388_608;
        let app_id = self.app_id.to_string();
        let steamapps = self.library_path.join("steamapps");
        let pending_download = directory_has_content(&steamapps.join("downloading").join(&app_id))
            || directory_has_content(&steamapps.join("temp").join(&app_id));
        self.state_flags & STATE_FULLY_INSTALLED != 0
            && self.state_flags & INCOMPLETE_FLAGS == 0
            && !pending_download
            && (self.bytes_to_download == 0 || self.bytes_downloaded >= self.bytes_to_download)
            && (self.bytes_to_stage == 0 || self.bytes_staged >= self.bytes_to_stage)
            && self.size_on_disk != Some(0)
            && directory_has_content(&self.install_dir())
    }
}

fn directory_has_content(path: &std::path::Path) -> bool {
    std::fs::read_dir(path)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some()
}
