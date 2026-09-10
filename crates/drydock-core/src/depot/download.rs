//! Fetch + parse orchestration and the download/verify engine.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use thiserror::Error;
use zip::ZipArchive;

use super::cdn::{CdnClient, CdnError, ContentServer};
use super::crypto::steam_adler_hash;
use super::keys::DepotKeys;
use super::manifest::{DepotManifest, ManifestError};
use crate::proxy::{ProxyClient, ProxyError};

/// How many times to try a chunk across rotating CDN hosts before giving up.
const CHUNK_ATTEMPTS: usize = 4;

#[derive(Debug, Error)]
pub enum DepotDownloadError {
    #[error("no depot download data is available for App {0}")]
    Unavailable(u32),
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error("manifest archive was invalid: {0}")]
    Archive(String),
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("no depot key was provided for depot {0}")]
    MissingKey(u32),
    #[error(transparent)]
    Cdn(#[from] CdnError),
    #[error("filesystem error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "not enough free space at {path}: the download needs about {} GB but only {} GB is available",
        needed / 1_073_741_824,
        available / 1_073_741_824
    )]
    NotEnoughSpace {
        path: PathBuf,
        needed: u64,
        available: u64,
    },
    #[error("download was cancelled")]
    Cancelled,
}

/// The depot key(s) and parsed manifest(s) for one app, ready to download or verify.
#[derive(Clone, Debug, Default)]
pub struct DepotData {
    pub app_id: u32,
    pub keys: DepotKeys,
    pub manifests: Vec<DepotManifest>,
    /// The manifest blobs exactly as they arrived, keyed by the name Steam stores them under in
    /// `depotcache` (`<depot id>_<manifest gid>.manifest`).
    ///
    /// Kept verbatim rather than re-serialised from [`Self::manifests`]: Steam reads these files
    /// itself, and only the untouched bytes are guaranteed to be what it expects. Parsing decrypts
    /// file names in the parsed copy, which must never leak back into what is written to disk.
    pub raw_manifests: BTreeMap<String, Vec<u8>>,
}

impl DepotData {
    /// Fetches the single depot **package** ZIP for an app via the proxy and parses it: every
    /// `.manifest` file becomes a [`DepotManifest`], and the depot keys are read from the bundled
    /// `.lua` (`addappid(<depot>, 1, "<hex>")`) and/or any `.key` file. Encrypted filenames are
    /// decrypted with the matching depot key.
    pub fn fetch(proxy: &ProxyClient, app_id: u32) -> Result<Self, DepotDownloadError> {
        let zip_bytes = proxy.depot_package(app_id)?;
        let mut archive = ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|error| DepotDownloadError::Archive(error.to_string()))?;

        // First pass: collect keys (from `.lua`/`.key`) and the raw manifest blobs.
        let mut keys = DepotKeys::default();
        let mut raw_manifests: Vec<Vec<u8>> = Vec::new();
        for index in 0..archive.len() {
            let mut entry = archive
                .by_index(index)
                .map_err(|error| DepotDownloadError::Archive(error.to_string()))?;
            let name = entry.name().to_ascii_lowercase();
            let mut bytes = Vec::with_capacity(crate::safe_path::capacity_hint(entry.size()));
            entry
                .read_to_end(&mut bytes)
                .map_err(|error| DepotDownloadError::Archive(error.to_string()))?;
            if name.ends_with(".manifest") {
                raw_manifests.push(bytes);
            } else if name.ends_with(".lua") {
                keys.merge_from(DepotKeys::parse_lua(&String::from_utf8_lossy(&bytes)));
            } else if name.ends_with(".key") {
                keys.merge_from(DepotKeys::parse(&String::from_utf8_lossy(&bytes)));
            }
        }

        let mut manifests = Vec::new();
        let mut stored: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for bytes in &raw_manifests {
            let mut manifest = DepotManifest::parse(bytes)?;
            // Record the untouched blob under Steam depotcache naming before decrypting names.
            stored.insert(
                format!("{}_{}.manifest", manifest.depot_id, manifest.manifest_gid),
                bytes.clone(),
            );
            if let Some(key) = keys.get(manifest.depot_id) {
                manifest.decrypt_filenames(key)?;
            }
            manifests.push(manifest);
        }
        if manifests.is_empty() {
            return Err(DepotDownloadError::Unavailable(app_id));
        }
        Ok(Self {
            app_id,
            keys,
            manifests,
            raw_manifests: stored,
        })
    }

    /// True when the package's manifests are **all** shared-redistributable depots — i.e. no real
    /// game content was packaged. Some upstream builds ship only the Common Redistributables (Visual
    /// C++, DirectX, …) with keys for the game's content depots but no manifest for them, usually
    /// because the source is still building the package. Downloading that would leave the game
    /// unplayable, so callers should refuse to report a successful download in this case.
    ///
    /// Note: having depot **keys** without a matching manifest is normal even for complete games (the
    /// App ID itself and optional per-language/OS depots are keyed but not always packaged), so the
    /// signal here is specifically "there is not one content manifest", not "some key lacks a
    /// manifest".
    #[must_use]
    pub fn has_no_content(&self) -> bool {
        !self
            .manifests
            .iter()
            .any(|manifest| !is_shared_redistributable_depot(manifest.depot_id))
    }

    /// Depot IDs of the manifests actually in the package (for user-facing diagnostics).
    #[must_use]
    pub fn manifest_depots(&self) -> Vec<u32> {
        let mut depots: Vec<u32> = self.manifests.iter().map(|manifest| manifest.depot_id).collect();
        depots.sort_unstable();
        depots
    }

    /// Total on-disk size across all real files (sum of file sizes) — the download's denominator.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.manifests
            .iter()
            .flat_map(|manifest| &manifest.files)
            .filter(|file| !file.is_directory())
            .map(|file| file.size)
            .sum()
    }
}

/// What the engine is doing, for the UI's Downloads bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadStage {
    Downloading,
    Verifying,
}

/// A progress tick emitted through the caller's callback.
#[derive(Clone, Debug)]
pub struct DownloadProgress {
    pub app_id: u32,
    pub stage: DownloadStage,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current_file: String,
}

/// Result of a completed download.
#[derive(Clone, Debug)]
pub struct DownloadOutcome {
    pub app_id: u32,
    pub files_written: u64,
    pub bytes_written: u64,
}

/// Result of a verify pass.
#[derive(Clone, Debug)]
pub struct VerifyOutcome {
    pub app_id: u32,
    pub total_chunks: u64,
    /// Chunks whose on-disk bytes are missing or fail their Adler-32 (i.e. need re-downloading).
    pub bad_chunks: u64,
}

impl VerifyOutcome {
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.bad_chunks == 0
    }
}

/// Downloads `data` into `install_root` (usually `steamapps/common/<installdir>`). Existing, already
/// valid chunks (matching Adler-32 on disk) are skipped, so this both **resumes** and repairs. The
/// callback receives progress ticks; set `cancel` to abort between chunks.
pub fn download(
    data: &DepotData,
    install_root: &Path,
    cdn: &CdnClient,
    cancel: &AtomicBool,
    connections: usize,
    max_bps: Option<u64>,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<DownloadOutcome, DepotDownloadError> {
    let total = data.total_bytes();
    let app_id = data.app_id;
    let servers = cdn.content_servers(0)?;
    if servers.is_empty() {
        return Err(DepotDownloadError::Cdn(CdnError::NoServers));
    }

    // Refuse before writing anything if the volume plainly cannot hold the download. Sizing every
    // file up front means a full disk would otherwise fail somewhere in the middle, leaving a
    // half-written install behind and an I/O error the user has to interpret.
    check_free_space(install_root, total)?;

    // Create every directory + size every file up front, then flatten all chunks into one work list
    // the workers pull from. `files_meta[i]` = (target path, manifest-relative path for display,
    // whether the file was newly created by this call).
    let mut files_meta: Vec<FileSlot> = Vec::new();
    let mut tasks: Vec<(usize, u32, &super::manifest::ChunkEntry)> = Vec::new();
    for manifest in &data.manifests {
        let depot_id = manifest.depot_id;
        if data.keys.get(depot_id).is_none() {
            return Err(DepotDownloadError::MissingKey(depot_id));
        }
        for file in &manifest.files {
            let target = joined(install_root, &file.path);
            if file.is_directory() {
                create_dir(&target)?;
                continue;
            }
            if let Some(parent) = target.parent() {
                create_dir(parent)?;
            }
            // A file we just created holds nothing but zeroes, so checksumming its chunks before
            // downloading them is pure waste — on a fresh install that meant reading back (and
            // Adler-32-ing) the entire game before fetching a single byte of it.
            let existed = target.exists();
            open_sized(&target, file.size)?; // create + set the final length
            let index = files_meta.len();
            files_meta.push(FileSlot {
                path: target,
                relative: file.path.clone(),
                may_resume: existed,
            });
            for chunk in &file.chunks {
                tasks.push((index, depot_id, chunk));
            }
        }
    }
    let files_written = files_meta.len() as u64;
    let task_count = tasks.len();

    let cursor = AtomicUsize::new(0);
    let completed = AtomicUsize::new(0);
    let done = AtomicU64::new(0);
    let bytes_written = AtomicU64::new(0);
    let server_cursor = AtomicUsize::new(0);
    let current_file: Mutex<String> = Mutex::new(String::new());
    let hard_error: Mutex<Option<DepotDownloadError>> = Mutex::new(None);
    let limiter = max_bps.filter(|bps| *bps > 0).map(RateLimiter::new);
    let workers = connections.clamp(1, 32);

    // N worker threads download chunks concurrently (each with its own file handles, writing to its
    // chunk's byte offset), while the coordinating thread reports aggregate progress. `thread::scope`
    // lets the workers borrow the shared state without `Arc`.
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut handles: HashMap<usize, File> = HashMap::new();
                loop {
                    if cancel.load(Ordering::Relaxed) || hard_error.lock().unwrap().is_some() {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::Relaxed);
                    if index >= task_count {
                        break;
                    }
                    let (file_index, depot_id, chunk) = tasks[index];
                    let slot = &files_meta[file_index];
                    let path = &slot.path;
                    let Some(key) = data.keys.get(depot_id) else {
                        // Unreachable given the up-front key check above, but record it as a hard
                        // error rather than just breaking: a silent break would leave `completed`
                        // short of `task_count` with no error and no cancel, and the progress loop
                        // below would spin forever.
                        *hard_error.lock().unwrap() = Some(DepotDownloadError::MissingKey(depot_id));
                        break;
                    };
                    let handle = match handles.entry(file_index) {
                        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            match OpenOptions::new().read(true).write(true).open(path) {
                                Ok(file) => entry.insert(file),
                                Err(source) => {
                                    *hard_error.lock().unwrap() = Some(io_err(path, source));
                                    break;
                                }
                            }
                        }
                    };
                    // Resume/repair: a chunk whose on-disk bytes already verify is skipped. Only
                    // worth checking for files that predate this call — see `FileSlot::may_resume`.
                    if slot.may_resume
                        && chunk_on_disk_ok(handle, chunk.offset, chunk.uncompressed_len, chunk.crc, path)
                            .unwrap_or(false)
                    {
                        done.fetch_add(u64::from(chunk.uncompressed_len), Ordering::Relaxed);
                        completed.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    if let Ok(mut name) = current_file.lock() {
                        slot.relative.clone_into(&mut name);
                    }
                    if let Some(limiter) = &limiter {
                        limiter.take(u64::from(chunk.compressed_len.max(1)));
                    }
                    let raw =
                        match download_chunk_rotating(cdn, &servers, &server_cursor, depot_id, chunk, key) {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                *hard_error.lock().unwrap() = Some(error);
                                break;
                            }
                        };
                    if let Err(source) = write_at(handle, chunk.offset, &raw, path) {
                        *hard_error.lock().unwrap() = Some(source);
                        break;
                    }
                    done.fetch_add(raw.len() as u64, Ordering::Relaxed);
                    bytes_written.fetch_add(raw.len() as u64, Ordering::Relaxed);
                    completed.fetch_add(1, Ordering::Relaxed);
                }
            });
        }

        // Report aggregate progress ~5×/s until every chunk is accounted for (or cancel/error).
        loop {
            let name = current_file.lock().map(|name| name.clone()).unwrap_or_default();
            progress(DownloadProgress {
                app_id,
                stage: DownloadStage::Downloading,
                done_bytes: done.load(Ordering::Relaxed).min(total),
                total_bytes: total,
                current_file: name,
            });
            if completed.load(Ordering::Relaxed) >= task_count
                || cancel.load(Ordering::Relaxed)
                || hard_error.lock().unwrap().is_some()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    if let Some(error) = hard_error.into_inner().unwrap() {
        return Err(error);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(DepotDownloadError::Cancelled);
    }
    progress(DownloadProgress {
        app_id,
        stage: DownloadStage::Downloading,
        done_bytes: total,
        total_bytes: total,
        current_file: String::new(),
    });
    Ok(DownloadOutcome {
        app_id,
        files_written,
        bytes_written: bytes_written.load(Ordering::Relaxed),
    })
}

/// One target file in the flattened work list.
struct FileSlot {
    path: PathBuf,
    /// Manifest-relative path, shown as the "current file" in progress ticks.
    relative: String,
    /// Whether the file already existed before this call. Only then can its on-disk bytes hold
    /// anything worth verifying; a file `open_sized` just created is all zeroes, so checksumming it
    /// before downloading would read the whole install back for nothing.
    may_resume: bool,
}

/// Fails early when the target volume clearly cannot hold `needed` bytes.
///
/// Best-effort: if free space cannot be determined (an unusual filesystem, a platform without the
/// syscall) the download proceeds as before rather than blocking on a number we do not have.
fn check_free_space(install_root: &Path, needed: u64) -> Result<(), DepotDownloadError> {
    let Some(available) = available_space(install_root) else {
        return Ok(());
    };
    // Leave a little headroom so the volume is not driven to exactly zero.
    const HEADROOM_BYTES: u64 = 256 * 1024 * 1024;
    if available < needed.saturating_add(HEADROOM_BYTES) {
        return Err(DepotDownloadError::NotEnoughSpace {
            path: install_root.to_path_buf(),
            needed,
            available,
        });
    }
    Ok(())
}

#[cfg(windows)]
fn available_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt as _;

    // `GetDiskFreeSpaceExW` wants a directory that exists; walk up until we find one, since the
    // install root itself may not have been created yet.
    let existing = path.ancestors().find(|candidate| candidate.is_dir())?;
    let mut wide: Vec<u16> = existing.as_os_str().encode_wide().collect();
    wide.push(0);

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: *const u16,
            free_bytes_available_to_caller: *mut u64,
            total_bytes: *mut u64,
            total_free_bytes: *mut u64,
        ) -> i32;
    }

    let mut available: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, and the three output
    // pointers are valid, correctly aligned `u64` locals.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(available)
}

#[cfg(not(windows))]
fn available_space(_path: &Path) -> Option<u64> {
    // No portable std API for this; the Windows build is the one that ships to users.
    None
}

/// A shared token-bucket that caps the *aggregate* download rate across all workers at `max_bps`
/// bytes/second (a ~1-second burst allowance). Each worker calls [`RateLimiter::take`] for the bytes
/// it is about to fetch, which blocks until that much budget is available.
struct RateLimiter {
    max_bps: f64,
    /// `(last refill instant, available budget in bytes)`.
    state: Mutex<(Instant, f64)>,
}

impl RateLimiter {
    fn new(max_bps: u64) -> Self {
        Self {
            max_bps: max_bps.max(1) as f64,
            state: Mutex::new((Instant::now(), 0.0)),
        }
    }

    fn take(&self, bytes: u64) {
        let bytes = bytes as f64;
        loop {
            let wait = {
                let mut state = self.state.lock().unwrap();
                let now = Instant::now();
                let elapsed = now.duration_since(state.0).as_secs_f64();
                let tokens = (state.1 + elapsed * self.max_bps).min(self.max_bps);
                state.0 = now;
                if tokens >= bytes {
                    state.1 = tokens - bytes;
                    return;
                }
                state.1 = tokens;
                (bytes - tokens) / self.max_bps
            };
            std::thread::sleep(Duration::from_secs_f64(wait.min(0.5)));
        }
    }
}

/// Verifies an installed app against its manifests without downloading, counting chunks whose
/// on-disk bytes are missing or fail their Adler-32.
pub fn verify(
    data: &DepotData,
    install_root: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<VerifyOutcome, DepotDownloadError> {
    let total = data.total_bytes();
    let mut done: u64 = 0;
    let mut total_chunks: u64 = 0;
    let mut bad_chunks: u64 = 0;

    for manifest in &data.manifests {
        for file in &manifest.files {
            if cancel.load(Ordering::Relaxed) {
                return Err(DepotDownloadError::Cancelled);
            }
            if file.is_directory() {
                continue;
            }
            let target = joined(install_root, &file.path);
            let mut handle = std::fs::File::open(&target).ok();
            for chunk in &file.chunks {
                total_chunks += 1;
                let ok = match handle.as_mut() {
                    Some(file) => {
                        chunk_on_disk_ok(file, chunk.offset, chunk.uncompressed_len, chunk.crc, &target)
                            .unwrap_or(false)
                    }
                    None => false,
                };
                if !ok {
                    bad_chunks += 1;
                }
                done += u64::from(chunk.uncompressed_len);
            }
            progress(DownloadProgress {
                app_id: data.app_id,
                stage: DownloadStage::Verifying,
                done_bytes: done.min(total),
                total_bytes: total,
                current_file: file.path.clone(),
            });
        }
    }

    Ok(VerifyOutcome {
        app_id: data.app_id,
        total_chunks,
        bad_chunks,
    })
}

/// Tries each CDN host in rotation until a chunk downloads and verifies, or attempts are exhausted.
/// The server cursor is shared atomically so concurrent workers spread across the CDN hosts.
fn download_chunk_rotating(
    cdn: &CdnClient,
    servers: &[ContentServer],
    cursor: &AtomicUsize,
    depot_id: u32,
    chunk: &super::manifest::ChunkEntry,
    key: &[u8; 32],
) -> Result<Vec<u8>, DepotDownloadError> {
    let mut last: Option<CdnError> = None;
    for _ in 0..CHUNK_ATTEMPTS.max(servers.len()) {
        let server = &servers[cursor.fetch_add(1, Ordering::Relaxed) % servers.len()];
        match cdn.download_chunk(server, depot_id, chunk, key) {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last = Some(error),
        }
    }
    Err(DepotDownloadError::Cdn(last.unwrap_or(CdnError::NoServers)))
}

/// Reads `len` bytes at `offset` and returns whether their Adler-32 matches `expected_crc`.
fn chunk_on_disk_ok(
    file: &mut std::fs::File,
    offset: u64,
    len: u32,
    expected_crc: u32,
    path: &Path,
) -> Result<bool, DepotDownloadError> {
    let metadata = file.metadata().map_err(|source| io_err(path, source))?;
    if metadata.len() < offset + u64::from(len) {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| io_err(path, source))?;
    let mut buffer = vec![0u8; len as usize];
    if file.read_exact(&mut buffer).is_err() {
        return Ok(false);
    }
    Ok(steam_adler_hash(&buffer) == expected_crc)
}

fn write_at(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &[u8],
    path: &Path,
) -> Result<(), DepotDownloadError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| io_err(path, source))?;
    file.write_all(bytes).map_err(|source| io_err(path, source))
}

/// Opens (creating if needed) `path` and ensures it is exactly `size` bytes long.
fn open_sized(path: &Path, size: u64) -> Result<std::fs::File, DepotDownloadError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| io_err(path, source))?;
    file.set_len(size).map_err(|source| io_err(path, source))?;
    Ok(file)
}

fn create_dir(path: &Path) -> Result<(), DepotDownloadError> {
    std::fs::create_dir_all(path).map_err(|source| io_err(path, source))
}

/// Whether a depot is one of Steam's shared "Common Redistributables" (app 228980: Visual C++,
/// DirectX, .NET, PhysX, …). These are bundled with many games but carry no actual game content, so
/// a package made up solely of them is not a usable download.
fn is_shared_redistributable_depot(depot_id: u32) -> bool {
    (228_980..=229_100).contains(&depot_id)
}

/// Joins a manifest-relative (forward-slash) path onto the install root, dropping every segment
/// that could escape it. See [`crate::is_safe_path_segment`] for why a bare `C:` matters as much as
/// `..` on Windows.
fn joined(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in relative.split(['/', '\\']) {
        if !crate::is_safe_path_segment(segment) {
            continue;
        }
        path.push(segment);
    }
    path
}

fn io_err(path: &Path, source: std::io::Error) -> DepotDownloadError {
    DepotDownloadError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depot::manifest::{ChunkEntry, DepotManifest, FileEntry};

    fn manifest_with(files: Vec<FileEntry>) -> DepotManifest {
        DepotManifest {
            depot_id: 1,
            manifest_gid: 1,
            filenames_encrypted: false,
            files,
        }
    }

    #[test]
    fn total_bytes_sums_real_files_only() {
        let data = DepotData {
            app_id: 730,
            keys: DepotKeys::default(),
            manifests: vec![manifest_with(vec![
                FileEntry {
                    path: "a".into(),
                    size: 100,
                    flags: 0,
                    chunks: vec![],
                },
                FileEntry {
                    path: "dir".into(),
                    size: 0,
                    flags: super::super::manifest::FLAG_DIRECTORY,
                    chunks: vec![],
                },
                FileEntry {
                    path: "b".into(),
                    size: 50,
                    flags: 0,
                    chunks: vec![],
                },
            ])],
            ..DepotData::default()
        };
        assert_eq!(data.total_bytes(), 150);
    }

    fn manifest_for(depot_id: u32) -> DepotManifest {
        let mut manifest = manifest_with(vec![]);
        manifest.depot_id = depot_id;
        manifest
    }

    #[test]
    fn has_no_content_when_only_redistributable_depots_are_packaged() {
        // Only depot 228989 (a shared Common Redistributable) was packaged — no game content. Keys
        // for the real content depots are present but useless without their manifests.
        let mut keys = DepotKeys::default();
        keys.0.insert(228989, [0u8; 32]);
        keys.0.insert(3751260, [1u8; 32]);
        keys.0.insert(3751261, [2u8; 32]);
        let data = DepotData {
            app_id: 3751260,
            keys,
            manifests: vec![manifest_for(228989)],
            ..DepotData::default()
        };
        assert!(data.has_no_content());
        assert_eq!(data.manifest_depots(), vec![228989]);
    }

    #[test]
    fn has_content_when_a_real_depot_is_present_alongside_redists() {
        // A complete game: redist depots plus a real content depot — even though the App ID and some
        // optional depots are keyed without a manifest, this must NOT be flagged as empty.
        let mut keys = DepotKeys::default();
        keys.0.insert(228989, [0u8; 32]);
        keys.0.insert(2358720, [1u8; 32]); // App ID keyed, no manifest — normal.
        keys.0.insert(2358721, [2u8; 32]);
        let data = DepotData {
            app_id: 2358720,
            keys,
            manifests: vec![manifest_for(228989), manifest_for(2358721)],
            ..DepotData::default()
        };
        assert!(!data.has_no_content());
    }

    #[test]
    fn concurrent_positioned_writes_dont_corrupt() {
        // The parallel engine gives each worker its own handle to a shared file and writes at the
        // chunk's byte offset. Verify that concurrent, non-overlapping positioned writes via separate
        // handles produce exactly the expected bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        drop(open_sized(&path, 4096).unwrap());
        std::thread::scope(|scope| {
            for region in 0..4u64 {
                let path = &path;
                scope.spawn(move || {
                    let mut file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .unwrap();
                    let bytes = vec![b'A' + region as u8; 1024];
                    write_at(&mut file, region * 1024, &bytes, path).unwrap();
                });
            }
        });
        let data = std::fs::read(&path).unwrap();
        assert_eq!(data.len(), 4096);
        for region in 0..4usize {
            let slice = &data[region * 1024..(region + 1) * 1024];
            assert!(slice.iter().all(|&byte| byte == b'A' + region as u8));
        }
    }

    #[test]
    fn joined_rejects_traversal() {
        let root = Path::new("/games/app");
        assert_eq!(
            joined(root, "sub/../../etc/passwd"),
            Path::new("/games/app/sub/etc/passwd")
        );
        assert_eq!(
            joined(root, "bin\\game.exe"),
            Path::new("/games/app/bin/game.exe")
        );
    }

    #[test]
    fn verify_reports_missing_file_as_bad() {
        let dir = tempfile::tempdir().unwrap();
        let data = DepotData {
            app_id: 730,
            keys: DepotKeys::default(),
            manifests: vec![manifest_with(vec![FileEntry {
                path: "missing.bin".into(),
                size: 16,
                flags: 0,
                chunks: vec![ChunkEntry {
                    sha: [0; 20],
                    crc: 123,
                    offset: 0,
                    uncompressed_len: 16,
                    compressed_len: 16,
                }],
            }])],
            ..DepotData::default()
        };
        let outcome = verify(&data, dir.path(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(outcome.total_chunks, 1);
        assert_eq!(outcome.bad_chunks, 1);
        assert!(!outcome.is_complete());
    }

    #[test]
    fn verify_accepts_matching_on_disk_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let payload = b"exactly-sixteen!"; // 16 bytes
        std::fs::write(dir.path().join("ok.bin"), payload).unwrap();
        let data = DepotData {
            app_id: 730,
            keys: DepotKeys::default(),
            manifests: vec![manifest_with(vec![FileEntry {
                path: "ok.bin".into(),
                size: 16,
                flags: 0,
                chunks: vec![ChunkEntry {
                    sha: [0; 20],
                    crc: steam_adler_hash(payload),
                    offset: 0,
                    uncompressed_len: 16,
                    compressed_len: 16,
                }],
            }])],
            ..DepotData::default()
        };
        let outcome = verify(&data, dir.path(), &AtomicBool::new(false), |_| {}).unwrap();
        assert_eq!(outcome.bad_chunks, 0);
        assert!(outcome.is_complete());
    }
}
