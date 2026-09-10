//! Shared payload types for the Steam Service (OST) and per-app fixes, plus Git blob SHA
//! verification.
//!
//! Networking moved to [`crate::proxy`]: the app no longer talks to GitHub directly. The proxy
//! lists the `NhMarco/MFB` repository server-side and returns ready-made manifests, but the
//! files are still identified by their Git blob SHA and verified after download, so these types
//! and the hashing helpers are shared by the proxy client and the install logic.

use std::collections::BTreeMap;

use sha1::{Digest, Sha1};

/// Service files that earlier releases installed and that must be removed on update.
pub const OBSOLETE_STEAM_SERVICE_FILE_NAMES: [&str; 2] = ["OnlineFix.dll", "mktl.dll"];

/// A single file advertised by the proxy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryFile {
    /// Base file name, e.g. `1234.lua`.
    pub relative_path: String,
    /// Proxy-relative download path, e.g. `/v1/service/file/hid.dll`.
    pub source_url: String,
    /// 40-character hexadecimal Git blob SHA-1 of the file content.
    pub sha: String,
}

impl RepositoryFile {
    #[must_use]
    pub fn file_name(&self) -> &str {
        self.relative_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&self.relative_path)
    }
}

/// A build-locked "Denuvo fix" from the GitHub `Files/fix` folder: a `{appid}.lua` unlock that
/// replaces the normal token Lua in the plug-in folder, plus a game-folder zip (single or split
/// parts). Both are git-blob-SHA verified after download.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DenuvoFix {
    /// `{appid}.lua` — replaces the normal token Lua in the plug-in folder.
    pub lua: RepositoryFile,
    /// The game-folder zip, extracted into `steamapps/common/{installdir}`. Either a single
    /// `{appid}.zip`, or ordered raw byte-split parts `{appid}.zip.001`, `…zip.002`, ….
    pub zip_parts: Vec<RepositoryFile>,
}

/// A game with a GitHub-sourced build-locked "Denuvo" fix available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixEntry {
    pub app_id: u32,
    /// A cleaned display name from upstream (the UI prefers the catalog name when available).
    pub name: String,
    /// The GitHub build-locked Denuvo fix, if one exists for this app.
    pub denuvo: Option<DenuvoFix>,
}

/// Version plus the ordered list of installable Steam Service files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamServiceManifest {
    pub version: String,
    pub files: Vec<RepositoryFile>,
}

/// A fully downloaded and verified Steam Service package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SteamServicePackage {
    pub version: String,
    /// File name (not path) mapped to verified content.
    pub files: BTreeMap<String, Vec<u8>>,
}

/// Verifies `content` against a 40-character hexadecimal Git blob SHA-1.
#[must_use]
pub fn matches_git_blob_sha(content: &[u8], expected_sha: &str) -> bool {
    if expected_sha.len() != 40 || !expected_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    compute_git_blob_sha(content).eq_ignore_ascii_case(expected_sha)
}

/// Computes the Git blob SHA-1 (`sha1("blob {len}\0" + content)`) as an uppercase hex string.
#[must_use]
pub fn compute_git_blob_sha(content: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", content.len()).as_bytes());
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(40);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02X}");
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_blob_sha_matches_known_git_object() {
        // `printf 'hello' | git hash-object --stdin` == b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0
        assert!(matches_git_blob_sha(
            b"hello",
            "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
        ));
        assert!(!matches_git_blob_sha(
            b"hello!",
            "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
        ));
        // The empty blob is a well-known Git object hash.
        assert!(matches_git_blob_sha(
            b"",
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        ));
        assert!(!matches_git_blob_sha(b"hello", "not-a-sha"));
    }

    #[test]
    fn repository_file_name_from_path() {
        let file = RepositoryFile {
            relative_path: "3321460.zip.001".to_owned(),
            source_url: "/v1/fixes/file/3321460.zip.001".to_owned(),
            sha: "aa".to_owned(),
        };
        assert_eq!(file.file_name(), "3321460.zip.001");
    }
}
