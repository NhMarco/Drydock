//! Fetch + parse orchestration and the download/verify engine.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use thiserror::Error;
use zip::ZipArchive;

use super::cdn::{CdnClient, CdnError, ContentServer, ServerPool, process_chunk};
use super::crypto::steam_adler_hash;
use super::keys::DepotKeys;
use super::manifest::{DepotManifest, ManifestError};
use crate::proxy::{ProxyClient, ProxyError};

/// How many times a chunk may be fetched before the download gives up: a failed request moves on to
/// another server (and gets at least one try per server), and so does a response that does not
/// decode and check out.
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
    /// The unlock Lua the package ships, as `(file name, bytes)`, when it has one.
    ///
    /// Preferred over the separate Lua API: this copy is cut from the same package as the manifests
    /// beside it, so its `setManifestid` lines are active and pin exactly the manifest GIDs shipped
    /// here. The Lua API returns the same app's unlock built against the *current* build instead —
    /// with the pins commented out and, for at least one observed title, fewer DLC ownership lines.
    /// Installing that leaves Steam unpinned and the cached manifests unused.
    pub lua: Option<(String, Vec<u8>)>,
}

impl DepotData {
    /// Fetches the single depot **package** ZIP for an app via the proxy and parses it: every
    /// `.manifest` file becomes a [`DepotManifest`], and the depot keys are read from the bundled
    /// `.lua` (`addappid(<depot>, 1, "<hex>")`) and/or any `.key` file. Encrypted filenames are
    /// decrypted with the matching depot key.
    pub fn fetch(proxy: &ProxyClient, app_id: u32) -> Result<Self, DepotDownloadError> {
        Self::parse_package(app_id, proxy.depot_package(app_id)?)
    }

    /// The parsing half of [`Self::fetch`], split out so it can be exercised without a network.
    pub fn parse_package(app_id: u32, zip_bytes: Vec<u8>) -> Result<Self, DepotDownloadError> {
        let mut archive = ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|error| DepotDownloadError::Archive(error.to_string()))?;

        // First pass: collect keys (from `.lua`/`.key`) and the raw manifest blobs.
        let mut keys = DepotKeys::default();
        let mut raw_manifests: Vec<Vec<u8>> = Vec::new();
        let mut lua: Option<(String, Vec<u8>)> = None;
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
                // Keep the first one only: a package carries the app's unlock, and a second `.lua`
                // would be something else. The name is taken from the archive but never used as a
                // path — the caller writes it as `<app id>.lua`.
                if lua.is_none() {
                    lua = Some((format!("{app_id}.lua"), bytes));
                }
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
            lua,
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
    pub install_root: PathBuf,
    pub files_written: u64,
    pub bytes_written: u64,
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
    progress: impl FnMut(DownloadProgress),
) -> Result<DownloadOutcome, DepotDownloadError> {
    let servers = cdn.content_servers(0, data.app_id)?;
    download_from(
        data,
        install_root,
        cdn,
        servers,
        cancel,
        connections,
        max_bps,
        progress,
    )
}

/// [`download`] from the given content servers.
///
/// `connections` threads fetch chunks while a second set of threads, one per CPU core, decrypts,
/// decompresses and writes them. Decoding is the slow part — most chunks are LZMA, which manages
/// only a few dozen MB per second per core — so doing it on the fetching threads would leave
/// connections idle while their chunk is being unpacked.
#[allow(clippy::too_many_arguments)]
fn download_from(
    data: &DepotData,
    install_root: &Path,
    cdn: &CdnClient,
    servers: Vec<ContentServer>,
    cancel: &AtomicBool,
    connections: usize,
    max_bps: Option<u64>,
    mut progress: impl FnMut(DownloadProgress),
) -> Result<DownloadOutcome, DepotDownloadError> {
    let total = data.total_bytes();
    let app_id = data.app_id;
    let pool = ServerPool::new(servers)?;

    // Refuse before writing anything if the volume plainly cannot hold the download. Sizing every
    // file up front means a full disk would otherwise fail somewhere in the middle, leaving a
    // half-written install behind and an I/O error the user has to interpret.
    let additional = additional_space(data, install_root);
    check_free_space(install_root, additional)?;
    let scope =
        crate::safe_path::WriteRoot::new(install_root).map_err(|error| io_err(install_root, error))?;

    // Create every directory + size every file up front, then list every distinct chunk once, with
    // each place it belongs.
    let mut files: Vec<FileSlot> = Vec::new();
    let mut chunks: Vec<ChunkTask> = Vec::new();
    let mut by_content: HashMap<(u32, [u8; 20], u32, u32), usize> = HashMap::new();
    // Folders already created by this call. Thousands of files share a handful of folders, and each
    // creation also checks the path for links, so it is done once per folder.
    let mut created: HashSet<PathBuf> = HashSet::new();
    for manifest in &data.manifests {
        let depot_id = manifest.depot_id;
        if data.keys.get(depot_id).is_none() {
            return Err(DepotDownloadError::MissingKey(depot_id));
        }
        for file in &manifest.files {
            let target = joined(install_root, &file.path);
            if file.is_directory() {
                if !created.contains(&target) {
                    scope
                        .create_dir_all(&target)
                        .map_err(|error| io_err(&target, error))?;
                    created.insert(target);
                }
                continue;
            }
            if let Some(parent) = target.parent()
                && !created.contains(parent)
            {
                scope
                    .create_dir_all(parent)
                    .map_err(|error| io_err(parent, error))?;
                created.insert(parent.to_owned());
            }
            // A file we just created holds nothing but zeroes, so checksumming its chunks before
            // downloading them is pure waste — on a fresh install that meant reading back (and
            // Adler-32-ing) the entire game before fetching a single byte of it.
            let existed = target.exists();
            open_sized(&scope, &target, file.size)?;
            let index = files.len();
            files.push(FileSlot {
                path: target,
                relative: file.path.clone(),
                may_resume: existed,
            });
            for chunk in &file.chunks {
                // Games often repeat content. Like Steam, fetch each distinct chunk once and write it
                // everywhere it belongs.
                let identity = (depot_id, chunk.sha, chunk.crc, chunk.uncompressed_len);
                match by_content.entry(identity) {
                    Entry::Occupied(entry) => chunks[*entry.get()].copies.push((index, chunk.offset)),
                    Entry::Vacant(entry) => {
                        entry.insert(chunks.len());
                        chunks.push(ChunkTask {
                            depot_id,
                            chunk,
                            copies: vec![(index, chunk.offset)],
                        });
                    }
                }
            }
        }
    }
    drop(by_content);
    let files_written = files.len() as u64;

    // One core is left to the rest of the app, so the window stays responsive on a small CPU.
    let decoders = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .saturating_sub(1)
        .clamp(1, MAXIMUM_DECODERS);
    let engine = Engine {
        data,
        cdn,
        pool,
        scope,
        files,
        chunks,
        cancel,
        limiter: max_bps.filter(|bps| *bps > 0).map(RateLimiter::new),
        next: AtomicUsize::new(0),
        retries: Mutex::new(Vec::new()),
        completed: AtomicUsize::new(0),
        done: AtomicU64::new(0),
        bytes_written: AtomicU64::new(0),
        current_file: Mutex::new(String::new()),
        failure: Mutex::new(None),
    };

    // A bounded hand-over: when decoding falls behind, the fetchers wait instead of piling up
    // megabytes of undecoded chunks.
    let (sender, receiver) = mpsc::sync_channel::<Fetched>(decoders);
    let receiver = Mutex::new(receiver);
    std::thread::scope(|threads| {
        for _ in 0..connections.clamp(1, 32) {
            let sender = sender.clone();
            threads.spawn(|| engine.fetch_chunks(sender));
        }
        // The decoders stop once every fetcher has dropped its sender.
        drop(sender);
        for _ in 0..decoders {
            threads.spawn(|| engine.store_chunks(&receiver));
        }

        // Report aggregate progress ~5×/s until every chunk is accounted for (or cancel/error).
        loop {
            let name = lock(&engine.current_file).clone();
            progress(DownloadProgress {
                app_id,
                stage: DownloadStage::Downloading,
                done_bytes: engine.done.load(Ordering::Relaxed).min(total),
                total_bytes: total,
                current_file: name,
            });
            if engine.finished() || engine.stopped() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });

    let bytes_written = engine.bytes_written.load(Ordering::Relaxed);
    if let Some(error) = engine
        .failure
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
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
        install_root: install_root.to_owned(),
        files_written,
        bytes_written,
    })
}

/// How many threads decode chunks at most, however many cores there are.
const MAXIMUM_DECODERS: usize = 16;
/// How long a fetcher with nothing to do waits before looking for a chunk sent back for another try.
const IDLE_WAIT: Duration = Duration::from_millis(20);

/// One target file in the download.
struct FileSlot {
    path: PathBuf,
    /// Manifest-relative path, shown as the "current file" in progress ticks.
    relative: String,
    /// Whether the file already existed before this call. Only then can its on-disk bytes hold
    /// anything worth verifying; a file `open_sized` just created is all zeroes, so checksumming it
    /// before downloading would read the whole install back for nothing.
    may_resume: bool,
}

/// One distinct chunk and every place in the install that holds a copy of it.
struct ChunkTask<'a> {
    depot_id: u32,
    chunk: &'a super::manifest::ChunkEntry,
    /// `(index into the file list, byte offset)` of each copy.
    copies: Vec<(usize, u64)>,
}

/// A fetched chunk on its way to a decoder.
struct Fetched {
    task: usize,
    /// How many earlier fetches of this chunk arrived unusable.
    attempt: usize,
    /// The server it came from, blamed if it does not check out.
    server: usize,
    /// The copies still to write; ones already intact on disk are left out.
    copies: Vec<(usize, u64)>,
    body: Vec<u8>,
}

/// A chunk to fetch again because what arrived did not check out.
struct Retry {
    task: usize,
    attempt: usize,
    copies: Vec<(usize, u64)>,
}

/// Everything the fetching and decoding threads of one download share.
struct Engine<'a> {
    data: &'a DepotData,
    cdn: &'a CdnClient,
    pool: ServerPool,
    scope: crate::safe_path::WriteRoot,
    files: Vec<FileSlot>,
    chunks: Vec<ChunkTask<'a>>,
    cancel: &'a AtomicBool,
    limiter: Option<RateLimiter>,
    /// The next chunk no fetcher has taken yet.
    next: AtomicUsize,
    retries: Mutex<Vec<Retry>>,
    /// Chunks fully written (or found intact).
    completed: AtomicUsize,
    done: AtomicU64,
    bytes_written: AtomicU64,
    current_file: Mutex<String>,
    /// The first error; it stops every thread.
    failure: Mutex<Option<DepotDownloadError>>,
}

impl Engine<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || lock(&self.failure).is_some()
    }

    fn finished(&self) -> bool {
        self.completed.load(Ordering::Relaxed) >= self.chunks.len()
    }

    fn fail(&self, error: DepotDownloadError) {
        lock(&self.failure).get_or_insert(error);
    }

    /// A fetching thread: takes chunks, fetches them and hands them to the decoders, until every
    /// chunk is written or the download stops. It keeps waiting once all chunks are handed out,
    /// because a decoder may still send one back for another try.
    fn fetch_chunks(&self, sender: mpsc::SyncSender<Fetched>) {
        let mut handles = FileHandles::default();
        while !self.stopped() && !self.finished() {
            let retry = lock(&self.retries).pop();
            let (index, attempt, copies) = match retry {
                Some(retry) => (retry.task, retry.attempt, retry.copies),
                None => {
                    let index = if self.next.load(Ordering::Relaxed) < self.chunks.len() {
                        self.next.fetch_add(1, Ordering::Relaxed)
                    } else {
                        usize::MAX
                    };
                    if index >= self.chunks.len() {
                        std::thread::sleep(IDLE_WAIT);
                        continue;
                    }
                    match self.copies_to_write(index, &mut handles) {
                        Ok(copies) => (index, 0, copies),
                        Err(error) => {
                            self.fail(error);
                            break;
                        }
                    }
                }
            };
            if copies.is_empty() {
                self.completed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let task = &self.chunks[index];
            *lock(&self.current_file) = self.files[copies[0].0].relative.clone();
            if let Some(limiter) = &self.limiter
                && !limiter.take(u64::from(task.chunk.compressed_len.max(1)), self.cancel)
            {
                break;
            }
            let (server, body) = match self.fetch(task) {
                Ok(fetched) => fetched,
                Err(error) => {
                    self.fail(error);
                    break;
                }
            };
            let fetched = Fetched {
                task: index,
                attempt,
                server,
                copies,
                body,
            };
            if sender.send(fetched).is_err() {
                break;
            }
        }
    }

    /// The copies of chunk `index` that still need writing. In a file that existed before this
    /// download, a copy whose bytes already check out is skipped (resume and repair) and counts as
    /// done right away.
    fn copies_to_write(
        &self,
        index: usize,
        handles: &mut FileHandles,
    ) -> Result<Vec<(usize, u64)>, DepotDownloadError> {
        let task = &self.chunks[index];
        let chunk = task.chunk;
        let mut needed = Vec::with_capacity(task.copies.len());
        for &(file, offset) in &task.copies {
            let slot = &self.files[file];
            if slot.may_resume {
                let handle = handles.get(&self.scope, file, &slot.path)?;
                if chunk_on_disk_ok(handle, offset, chunk.uncompressed_len, chunk.crc, &slot.path)
                    .unwrap_or(false)
                {
                    self.done
                        .fetch_add(u64::from(chunk.uncompressed_len), Ordering::Relaxed);
                    continue;
                }
            }
            needed.push((file, offset));
        }
        Ok(needed)
    }

    /// Fetches a chunk's encrypted bytes, moving on to another server whenever one fails.
    fn fetch(&self, task: &ChunkTask) -> Result<(usize, Vec<u8>), DepotDownloadError> {
        let mut avoid = None;
        let mut last = None;
        for _ in 0..CHUNK_ATTEMPTS.max(self.pool.len()) {
            let server = self.pool.pick(avoid);
            let started = Instant::now();
            match self
                .cdn
                .fetch_chunk(self.pool.server(server), task.depot_id, task.chunk)
            {
                Ok(body) => {
                    self.pool.record(server, body.len(), started.elapsed());
                    return Ok((server, body));
                }
                Err(error) => {
                    self.pool.record_failure(server);
                    avoid = Some(server);
                    last = Some(error);
                }
            }
        }
        Err(DepotDownloadError::Cdn(last.unwrap_or(CdnError::NoServers)))
    }

    /// A decoding thread: decrypts, decompresses and checks fetched chunks and writes every copy,
    /// until the fetchers are gone. After a failure or cancel it only empties the queue, so no
    /// fetcher is left waiting on a full one.
    fn store_chunks(&self, receiver: &Mutex<mpsc::Receiver<Fetched>>) {
        let mut handles = FileHandles::default();
        loop {
            let fetched = lock(receiver).recv();
            let Ok(fetched) = fetched else {
                break;
            };
            if self.stopped() {
                continue;
            }
            let task = &self.chunks[fetched.task];
            let Some(key) = self.data.keys.get(task.depot_id) else {
                // Unreachable given the up-front key check, but a silent skip would leave the
                // download short of finishing with nothing to show for it.
                self.fail(DepotDownloadError::MissingKey(task.depot_id));
                continue;
            };
            match process_chunk(&fetched.body, task.chunk, key) {
                Ok(raw) => {
                    for &(file, offset) in &fetched.copies {
                        let slot = &self.files[file];
                        let written = handles
                            .get(&self.scope, file, &slot.path)
                            .and_then(|handle| write_at(handle, offset, &raw, &slot.path));
                        if let Err(error) = written {
                            self.fail(error);
                            break;
                        }
                        self.done.fetch_add(raw.len() as u64, Ordering::Relaxed);
                        self.bytes_written.fetch_add(raw.len() as u64, Ordering::Relaxed);
                    }
                    self.completed.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => {
                    // A cut-off or corrupted response: another server gets to try.
                    self.pool.record_failure(fetched.server);
                    if fetched.attempt + 1 >= CHUNK_ATTEMPTS {
                        self.fail(error.into());
                    } else {
                        lock(&self.retries).push(Retry {
                            task: fetched.task,
                            attempt: fetched.attempt + 1,
                            copies: fetched.copies,
                        });
                    }
                }
            }
        }
    }
}

/// A thread's open handles to the files it touches, reused across its chunks.
#[derive(Default)]
struct FileHandles(HashMap<usize, File>);

impl FileHandles {
    /// At most this many stay open per thread (512 across all of them at the maximum).
    const LIMIT: usize = 16;

    fn get(
        &mut self,
        scope: &crate::safe_path::WriteRoot,
        index: usize,
        path: &Path,
    ) -> Result<&mut File, DepotDownloadError> {
        if self.0.len() >= Self::LIMIT && !self.0.contains_key(&index) {
            self.0.clear();
        }
        match self.0.entry(index) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let file = scope.open(path, false).map_err(|source| io_err(path, source))?;
                Ok(entry.insert(file))
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many more bytes the download needs on disk: each file's size beyond what is already there, so
/// resuming or repairing an install does not ask for room for the whole game again.
pub(crate) fn additional_space(data: &DepotData, install_root: &Path) -> u64 {
    data.manifests
        .iter()
        .flat_map(|manifest| &manifest.files)
        .filter(|file| !file.is_directory())
        .map(|file| {
            file.size.saturating_sub(
                std::fs::metadata(joined(install_root, &file.path)).map_or(0, |meta| meta.len()),
            )
        })
        .fold(0_u64, u64::saturating_add)
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

    fn take(&self, bytes: u64, cancel: &AtomicBool) -> bool {
        let mut remaining = bytes as f64;
        while remaining > 0.0 {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let wait = {
                let mut state = self.state.lock().unwrap();
                let now = Instant::now();
                let elapsed = now.duration_since(state.0).as_secs_f64();
                let tokens = (state.1 + elapsed * self.max_bps).min(self.max_bps);
                state.0 = now;
                let consumed = tokens.min(remaining);
                remaining -= consumed;
                state.1 = tokens - consumed;
                remaining / self.max_bps
            };
            std::thread::sleep(Duration::from_secs_f64(wait.min(0.5)));
        }
        true
    }
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

/// Opens (creating if needed) `path`, whose folder must exist, and ensures it is exactly `size` bytes
/// long.
fn open_sized(
    scope: &crate::safe_path::WriteRoot,
    path: &Path,
    size: u64,
) -> Result<std::fs::File, DepotDownloadError> {
    let file = scope.open(path, true).map_err(|source| io_err(path, source))?;
    file.set_len(size).map_err(|source| io_err(path, source))?;
    Ok(file)
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
pub(super) fn joined(root: &Path, relative: &str) -> PathBuf {
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

    /// Runs `take` on its own thread. Tests wait for the result with a deadline, so a limiter that
    /// blocks forever fails the test instead of hanging the whole run.
    fn spawn_take(
        max_bps: u64,
        bytes: u64,
        cancel: &std::sync::Arc<AtomicBool>,
    ) -> std::sync::mpsc::Receiver<bool> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::clone(cancel);
        std::thread::spawn(move || {
            let _ = sender.send(RateLimiter::new(max_bps).take(bytes, &cancel));
        });
        receiver
    }

    const LIMITER_DEADLINE: Duration = Duration::from_secs(10);

    #[test]
    fn a_request_larger_than_the_limiter_capacity_completes() {
        let taken = spawn_take(1000, 1001, &std::sync::Arc::default());
        assert_eq!(taken.recv_timeout(LIMITER_DEADLINE).ok(), Some(true));
    }

    #[test]
    fn cancelling_interrupts_a_long_limiter_wait() {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let taken = spawn_take(1, 1_000_000, &cancel);
        std::thread::sleep(Duration::from_millis(20));
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(taken.recv_timeout(LIMITER_DEADLINE).ok(), Some(false));
    }

    fn manifest_with(files: Vec<FileEntry>) -> DepotManifest {
        DepotManifest {
            depot_id: 1,
            manifest_gid: 1,
            filenames_encrypted: false,
            files,
        }
    }

    /// Packs `(name, bytes)` into a ZIP shaped like an upstream depot package.
    fn package_zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut buffer = std::io::Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).expect("entry");
            writer.write_all(bytes).expect("write");
        }
        writer.finish().expect("finish");
        buffer.into_inner()
    }

    fn one_manifest(depot_id: u32, gid: u64) -> Vec<u8> {
        crate::depot::manifest::tests::build_manifest(
            depot_id,
            gid,
            false,
            &[FileEntry {
                path: "game.exe".into(),
                size: 10,
                flags: 0,
                chunks: vec![],
            }],
        )
    }

    #[test]
    fn the_packages_own_lua_is_taken_and_named_after_the_app() {
        // The point of preferring it: this copy pins the very manifest shipped beside it, whereas
        // the separate Lua API answers for the current build with its `setManifestid` commented out.
        let lua = b"addappid(5)\nsetManifestid(9,\"123\")\n".to_vec();
        let zip = package_zip(&[
            ("9_123.manifest", one_manifest(9, 123)),
            ("whatever-they-called-it.lua", lua.clone()),
        ]);

        let data = DepotData::parse_package(4242, zip).expect("package parses");
        let (name, bytes) = data.lua.expect("the package's Lua is picked up");
        assert_eq!(name, "4242.lua", "it is installed under the app's own name");
        assert_eq!(bytes, lua, "and byte-for-byte as shipped");
        assert!(data.raw_manifests.contains_key("9_123.manifest"));
    }

    #[test]
    fn a_package_without_a_lua_reports_none() {
        // The caller then falls back to the Lua API rather than leaving the app unlocked.
        let zip = package_zip(&[("9_123.manifest", one_manifest(9, 123))]);
        assert!(DepotData::parse_package(1, zip).expect("parses").lua.is_none());
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
        drop(
            open_sized(
                &crate::safe_path::WriteRoot::new(path.parent().unwrap()).unwrap(),
                &path,
                4096,
            )
            .unwrap(),
        );
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

    /// A stand-in content server: answers `GET …/chunk/<id>` over plain HTTP with keep-alive and
    /// counts the requests per chunk. The first request for `garbled` gets bytes that do not decode.
    struct ChunkServer {
        host: String,
        requests: std::sync::Arc<Mutex<HashMap<String, usize>>>,
    }

    impl ChunkServer {
        fn start(bodies: HashMap<String, Vec<u8>>, garbled: Option<String>) -> Self {
            use std::io::{BufRead, BufReader};
            use std::sync::Arc;

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let host = listener.local_addr().unwrap().to_string();
            let requests: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
            let bodies = Arc::new(bodies);
            let counts = Arc::clone(&requests);
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (bodies, counts, garbled) =
                        (Arc::clone(&bodies), Arc::clone(&counts), garbled.clone());
                    std::thread::spawn(move || {
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        let mut writer = stream;
                        loop {
                            let mut request = String::new();
                            if reader.read_line(&mut request).unwrap_or(0) == 0 {
                                return;
                            }
                            loop {
                                let mut header = String::new();
                                if reader.read_line(&mut header).unwrap_or(0) == 0 {
                                    return;
                                }
                                if header == "\r\n" {
                                    break;
                                }
                            }
                            let id = request
                                .split_whitespace()
                                .nth(1)
                                .and_then(|path| path.rsplit('/').next())
                                .unwrap_or_default()
                                .to_owned();
                            let seen = {
                                let mut counts = counts.lock().unwrap();
                                let count = counts.entry(id.clone()).or_insert(0);
                                *count += 1;
                                *count
                            };
                            let (status, body) = match bodies.get(&id) {
                                Some(_) if garbled.as_deref() == Some(id.as_str()) && seen == 1 => {
                                    ("200 OK", vec![0x5A; 64])
                                }
                                Some(body) => ("200 OK", body.clone()),
                                None => ("404 Not Found", Vec::new()),
                            };
                            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n", body.len());
                            if writer.write_all(head.as_bytes()).is_err() || writer.write_all(&body).is_err()
                            {
                                return;
                            }
                        }
                    });
                }
            });
            Self { host, requests }
        }

        fn requests_for(&self, id: &str) -> usize {
            self.requests.lock().unwrap().get(id).copied().unwrap_or(0)
        }

        fn total_requests(&self) -> usize {
            self.requests.lock().unwrap().values().sum()
        }
    }

    /// Encrypts `raw` into a zstd (VSZ) chunk the way the CDN stores it, and describes it.
    fn cdn_chunk(id: u8, raw: &[u8], offset: u64, key: &[u8; 32]) -> (ChunkEntry, Vec<u8>) {
        let frame = zstd::stream::encode_all(raw, 3).unwrap();
        let mut container = b"VSZa".to_vec();
        container.extend_from_slice(&0u32.to_le_bytes());
        container.extend_from_slice(&frame);
        container.extend_from_slice(&0u32.to_le_bytes());
        container.extend_from_slice(&(raw.len() as u64).to_le_bytes());
        container.extend_from_slice(b"zsv");
        let body = crate::depot::crypto::symmetric_encrypt(&container, key, &[id; 16]);
        let entry = ChunkEntry {
            sha: [id; 20],
            crc: steam_adler_hash(raw),
            offset,
            uncompressed_len: raw.len() as u32,
            compressed_len: body.len() as u32,
        };
        (entry, body)
    }

    #[test]
    fn each_distinct_chunk_is_fetched_once_and_a_garbled_one_again() {
        let key = [9u8; 32];
        let first = vec![1u8; 3000];
        let second: Vec<u8> = (0..2000u32).map(|value| (value % 251) as u8).collect();
        let third = b"a chunk the server garbles once".repeat(20);
        let (a, a_body) = cdn_chunk(1, &first, 0, &key);
        let (b, b_body) = cdn_chunk(2, &second, first.len() as u64, &key);
        let (c, c_body) = cdn_chunk(3, &third, 0, &key);
        let server = ChunkServer::start(
            HashMap::from([(a.id_hex(), a_body), (b.id_hex(), b_body), (c.id_hex(), c_body)]),
            Some(c.id_hex()),
        );

        let mut keys = DepotKeys::default();
        keys.0.insert(1, key);
        let file = |path: &str, size: usize, chunks: Vec<ChunkEntry>| FileEntry {
            path: path.into(),
            size: size as u64,
            flags: 0,
            chunks,
        };
        let data = DepotData {
            app_id: 1,
            keys,
            manifests: vec![manifest_with(vec![
                file(
                    "game/data.bin",
                    first.len() + second.len(),
                    vec![a.clone(), b.clone()],
                ),
                // The same content again, in another file.
                file("game/copy.bin", first.len(), vec![a.clone()]),
                file("game/sub/other.bin", third.len(), vec![c.clone()]),
            ])],
            ..DepotData::default()
        };

        let root = tempfile::tempdir().unwrap();
        let servers = vec![ContentServer {
            host: server.host.clone(),
            https: false,
        }];
        let cdn = CdnClient::new().unwrap();
        let outcome = download_from(
            &data,
            root.path(),
            &cdn,
            servers.clone(),
            &AtomicBool::new(false),
            4,
            None,
            |_| {},
        )
        .unwrap();

        assert_eq!(
            std::fs::read(root.path().join("game/data.bin")).unwrap(),
            [first.as_slice(), second.as_slice()].concat()
        );
        assert_eq!(std::fs::read(root.path().join("game/copy.bin")).unwrap(), first);
        assert_eq!(
            std::fs::read(root.path().join("game/sub/other.bin")).unwrap(),
            third
        );
        assert_eq!(
            server.requests_for(&a.id_hex()),
            1,
            "shared content is fetched once"
        );
        assert_eq!(server.requests_for(&b.id_hex()), 1);
        assert_eq!(
            server.requests_for(&c.id_hex()),
            2,
            "a garbled chunk is fetched again"
        );
        assert_eq!(outcome.files_written, 3);
        assert_eq!(outcome.bytes_written, data.total_bytes());

        // Everything is in place now, so running again fetches nothing and writes nothing.
        let before = server.total_requests();
        let again = download_from(
            &data,
            root.path(),
            &cdn,
            servers,
            &AtomicBool::new(false),
            4,
            None,
            |_| {},
        )
        .unwrap();
        assert_eq!(server.total_requests(), before);
        assert_eq!(again.bytes_written, 0);
    }

    #[test]
    fn a_chunk_no_server_has_fails_the_download_instead_of_hanging() {
        let key = [4u8; 32];
        let (missing, _) = cdn_chunk(7, b"nobody serves this", 0, &key);
        let server = ChunkServer::start(HashMap::new(), None);
        let mut keys = DepotKeys::default();
        keys.0.insert(1, key);
        let data = DepotData {
            app_id: 1,
            keys,
            manifests: vec![manifest_with(vec![FileEntry {
                path: "lost.bin".into(),
                size: 18,
                flags: 0,
                chunks: vec![missing],
            }])],
            ..DepotData::default()
        };
        let root = tempfile::tempdir().unwrap();
        let result = download_from(
            &data,
            root.path(),
            &CdnClient::new().unwrap(),
            vec![ContentServer {
                host: server.host.clone(),
                https: false,
            }],
            &AtomicBool::new(false),
            2,
            None,
            |_| {},
        );
        assert!(matches!(
            result,
            Err(DepotDownloadError::Cdn(CdnError::Status(404)))
        ));
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
}
