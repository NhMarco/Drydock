//! A persistent, disk-backed `egui` bytes loader for the storefront/library CDN images.
//!
//! egui's default image loader keeps decoded textures in memory only, so every artwork is
//! re-downloaded from the Steam CDN on each app launch. This loader adds a disk cache under
//! `cache/images/`: a successfully fetched image is written there and, on any later frame or launch,
//! served straight from disk (instant, no spinner). It must be registered **before**
//! `egui_extras::install_image_loaders` so it wins over the default HTTP loader (egui tries bytes
//! loaders in registration order). Non-`http(s)` URIs are declined so egui's other loaders
//! (`bytes://`, `file://`, `include_image!`) still work.
//!
//! Two things keep it bounded, because a catalog of 70k+ games means effectively unbounded artwork:
//!
//! * **In memory**, decoded bytes are held in a small LRU (see [`MAXIMUM_MEMORY_BYTES`]). Previously
//!   every image fetched in a session was pinned for the life of the process, so a long scroll
//!   through the Denuvo or Repacks lists grew RSS monotonically.
//! * **On disk**, entries are keyed by a *stable* SHA-256 of the URI and swept by age on startup.
//!   The key used to come from `DefaultHasher`, whose output is explicitly not stable across Rust
//!   releases — so every rebuild with a new toolchain orphaned the entire cache while leaving the
//!   files behind forever.
//!
//! Failures are remembered in memory only (so they are not pinned across launches) and expire after
//! [`FAILURE_RETRY`], so one bad minute on the network does not blank a row until restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::Context;
use eframe::egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};

/// How much decoded image data to keep resident. Steam headers are ~30–60 KB, so this holds several
/// hundred of them — far more than any screenful — while staying a bounded, predictable cost.
const MAXIMUM_MEMORY_BYTES: usize = 48 * 1024 * 1024;

/// How long a failed fetch is remembered before it may be retried.
const FAILURE_RETRY: Duration = Duration::from_secs(60);

/// Disk entries untouched for this long are deleted by [`DiskImageCache::sweep`].
const DISK_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Hard cap on a single cached image, so a hostile or broken response cannot fill memory or disk.
const MAXIMUM_IMAGE_BYTES: usize = 16 * 1024 * 1024;

enum Entry {
    Pending,
    Ready {
        bytes: Arc<[u8]>,
        /// Bumped on every hit; the lowest value is evicted first.
        touched: u64,
    },
    Failed {
        at: Instant,
    },
}

#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    /// Monotonic tick used as the LRU clock.
    clock: u64,
    /// Running total of `Ready` payload sizes, so eviction does not have to re-sum the map.
    resident_bytes: usize,
}

impl State {
    /// Records a hit on `uri` and returns its bytes, if resident.
    fn touch(&mut self, uri: &str) -> Option<Arc<[u8]>> {
        self.clock += 1;
        let clock = self.clock;
        match self.entries.get_mut(uri) {
            Some(Entry::Ready { bytes, touched }) => {
                *touched = clock;
                Some(bytes.clone())
            }
            _ => None,
        }
    }

    fn insert_ready(&mut self, uri: String, bytes: Arc<[u8]>) {
        self.clock += 1;
        self.remove(&uri);
        self.resident_bytes += bytes.len();
        self.entries.insert(
            uri,
            Entry::Ready {
                bytes,
                touched: self.clock,
            },
        );
        self.evict_to_budget();
    }

    fn remove(&mut self, uri: &str) {
        if let Some(Entry::Ready { bytes, .. }) = self.entries.remove(uri) {
            self.resident_bytes = self.resident_bytes.saturating_sub(bytes.len());
        }
    }

    /// Drops least-recently-used `Ready` entries until the budget is met. `Pending` entries are never
    /// evicted — an in-flight fetch must be able to find its slot when it completes.
    fn evict_to_budget(&mut self) {
        while self.resident_bytes > MAXIMUM_MEMORY_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter_map(|(uri, entry)| match entry {
                    Entry::Ready { touched, .. } => Some((*touched, uri.clone())),
                    _ => None,
                })
                .min_by_key(|(touched, _)| *touched)
                .map(|(_, uri)| uri);
            match oldest {
                Some(uri) => self.remove(&uri),
                None => break, // nothing evictable left
            }
        }
    }
}

pub struct DiskImageCache {
    dir: PathBuf,
    state: Arc<Mutex<State>>,
}

impl DiskImageCache {
    /// Creates the cache rooted at `<cache_root>/images` (created if missing).
    pub fn new(cache_root: &Path) -> Self {
        let dir = cache_root.join("images");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    /// The on-disk file for a URI: a SHA-256 prefix of the URI under the cache dir.
    ///
    /// Deliberately **not** `DefaultHasher`: its output is unspecified and changes between Rust
    /// releases, which silently orphaned every cached image on each toolchain bump.
    fn path_for(&self, uri: &str) -> PathBuf {
        use sha2::{Digest as _, Sha256};
        let digest = Sha256::digest(uri.as_bytes());
        let mut name = String::with_capacity(32);
        for byte in &digest[..16] {
            use std::fmt::Write as _;
            let _ = write!(name, "{byte:02x}");
        }
        self.dir.join(name)
    }

    /// Deletes cached image files untouched for [`DISK_MAX_AGE`], returning how many were removed.
    ///
    /// Called once at startup. Without it the directory only ever grew — one file per artwork the user
    /// ever saw, plus everything orphaned by the old unstable hash.
    pub fn sweep(cache_root: &Path) -> usize {
        let dir = cache_root.join("images");
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return 0;
        };
        let mut removed = 0;
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > DISK_MAX_AGE);
            if stale && std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Locks the shared state, recovering from a poisoned mutex rather than panicking.
    ///
    /// This runs on the render thread. Propagating a poisoned lock would take the whole window down
    /// because one background fetch callback panicked — the cache is regenerable, so carrying on with
    /// whatever state is there is strictly better.
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl BytesLoader for DiskImageCache {
    fn id(&self) -> &str {
        concat!(module_path!(), "::DiskImageCache")
    }

    fn load(&self, ctx: &Context, uri: &str) -> BytesLoadResult {
        // Only take over remote images; everything else falls through to egui's own loaders.
        if !(uri.starts_with("http://") || uri.starts_with("https://")) {
            return Err(LoadError::NotSupported);
        }

        let mut state = self.state();
        if let Some(bytes) = state.touch(uri) {
            return Ok(BytesPoll::Ready {
                size: None,
                bytes: Bytes::Shared(bytes),
                mime: None,
            });
        }
        match state.entries.get(uri) {
            Some(Entry::Pending) => return Ok(BytesPoll::Pending { size: None }),
            // A failure is held only briefly: transient CDN/network errors used to pin a blank tile
            // for the rest of the session.
            Some(Entry::Failed { at }) if at.elapsed() < FAILURE_RETRY => {
                return Err(LoadError::Loading("image download failed".to_owned()));
            }
            _ => {}
        }

        // Not resident: serve from disk instantly if we cached it before, else fetch.
        let path = self.path_for(uri);
        if let Ok(bytes) = std::fs::read(&path)
            && !bytes.is_empty()
            && bytes.len() <= MAXIMUM_IMAGE_BYTES
        {
            let shared: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
            state.insert_ready(uri.to_owned(), shared.clone());
            return Ok(BytesPoll::Ready {
                size: None,
                bytes: Bytes::Shared(shared),
                mime: None,
            });
        }

        state.entries.insert(uri.to_owned(), Entry::Pending);
        drop(state);

        let shared_state = Arc::clone(&self.state);
        let ctx = ctx.clone();
        let uri_key = uri.to_owned();
        ehttp::fetch(ehttp::Request::get(uri), move |result| {
            let mut state = shared_state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match result {
                Ok(response)
                    if response.ok
                        && !response.bytes.is_empty()
                        && response.bytes.len() <= MAXIMUM_IMAGE_BYTES =>
                {
                    // Best-effort disk cache; a write failure just means it reloads next time.
                    let _ = std::fs::write(&path, &response.bytes);
                    state.insert_ready(uri_key, Arc::from(response.bytes.into_boxed_slice()));
                }
                _ => {
                    state
                        .entries
                        .insert(uri_key, Entry::Failed { at: Instant::now() });
                }
            }
            drop(state);
            ctx.request_repaint();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.state().remove(uri);
    }

    fn forget_all(&self) {
        let mut state = self.state();
        state.entries.clear();
        state.resident_bytes = 0;
    }

    fn byte_size(&self) -> usize {
        self.state().resident_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declines_non_http_uris() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskImageCache::new(dir.path());
        let ctx = Context::default();
        assert!(matches!(
            cache.load(&ctx, "bytes://logo.png"),
            Err(LoadError::NotSupported)
        ));
        assert!(matches!(
            cache.load(&ctx, "file:///tmp/x.png"),
            Err(LoadError::NotSupported)
        ));
    }

    /// The disk key must be reproducible across processes and toolchains, or the cache orphans itself
    /// on every rebuild.
    #[test]
    fn disk_keys_are_stable_and_uri_specific() {
        let dir = tempfile::tempdir().unwrap();
        let first = DiskImageCache::new(dir.path());
        let second = DiskImageCache::new(dir.path());
        let url = "https://cdn.example/steam/apps/730/header.jpg";
        assert_eq!(first.path_for(url), second.path_for(url));
        assert_ne!(first.path_for(url), first.path_for(&format!("{url}?t=2")));
        let name = first
            .path_for(url)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert_eq!(name.len(), 32, "expected a 16-byte hex digest, got {name}");
    }

    #[test]
    fn serves_a_pre_existing_disk_entry_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskImageCache::new(dir.path());
        let url = "https://cdn.example/art.jpg";
        std::fs::write(cache.path_for(url), b"fake-jpeg-bytes").unwrap();

        let poll = cache.load(&Context::default(), url).expect("disk hit");
        let BytesPoll::Ready { bytes, .. } = poll else {
            panic!("expected an immediate disk hit");
        };
        assert_eq!(&*bytes, b"fake-jpeg-bytes");
        assert_eq!(cache.byte_size(), 15);
    }

    #[test]
    fn evicts_least_recently_used_entries_past_the_memory_budget() {
        let mut state = State::default();
        // Each entry is just over a third of the budget, so two fit and the third forces an eviction.
        let chunk = MAXIMUM_MEMORY_BYTES / 3 + 1;
        let payload = || Arc::from(vec![0u8; chunk].into_boxed_slice());
        state.insert_ready("a".into(), payload());
        state.insert_ready("b".into(), payload());
        // Re-reading `a` makes `b` the coldest entry even though it was inserted later.
        assert!(state.touch("a").is_some());
        state.insert_ready("c".into(), payload());

        assert!(state.resident_bytes <= MAXIMUM_MEMORY_BYTES);
        assert!(state.touch("a").is_some(), "the re-read entry must survive");
        assert!(state.touch("c").is_some(), "the newest entry must survive");
        assert!(state.touch("b").is_none(), "the coldest entry is the one evicted");
    }

    /// A plain insert counts as a use, so without any re-reads the oldest insert is evicted first.
    #[test]
    fn eviction_without_re_reads_drops_the_oldest_insert() {
        let mut state = State::default();
        let chunk = MAXIMUM_MEMORY_BYTES / 3 + 1;
        let payload = || Arc::from(vec![0u8; chunk].into_boxed_slice());
        state.insert_ready("first".into(), payload());
        state.insert_ready("second".into(), payload());
        state.insert_ready("third".into(), payload());

        assert!(state.touch("first").is_none());
        assert!(state.touch("second").is_some());
        assert!(state.touch("third").is_some());
    }

    #[test]
    fn a_pending_entry_is_never_evicted() {
        let mut state = State::default();
        state.entries.insert("pending".into(), Entry::Pending);
        state.insert_ready(
            "huge".into(),
            Arc::from(vec![0u8; MAXIMUM_MEMORY_BYTES + 1].into_boxed_slice()),
        );
        assert!(
            matches!(state.entries.get("pending"), Some(Entry::Pending)),
            "an in-flight fetch must still find its slot"
        );
    }

    #[test]
    fn sweep_removes_only_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskImageCache::new(dir.path());
        let fresh = cache.path_for("https://cdn.example/fresh.jpg");
        std::fs::write(&fresh, b"x").unwrap();
        assert_eq!(DiskImageCache::sweep(dir.path()), 0);
        assert!(fresh.is_file(), "a fresh entry must survive the sweep");
    }

    #[test]
    #[ignore = "hits the Steam CDN over the network"]
    fn caches_a_real_image_to_disk_and_serves_it_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskImageCache::new(dir.path());
        let ctx = Context::default();
        let url = "https://cdn.cloudflare.steamstatic.com/steam/apps/730/header.jpg";

        // First touch: pending, triggers the background fetch.
        assert!(matches!(cache.load(&ctx, url), Ok(BytesPoll::Pending { .. })));
        let mut ready = false;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if matches!(cache.load(&ctx, url), Ok(BytesPoll::Ready { .. })) {
                ready = true;
                break;
            }
        }
        assert!(ready, "image never became ready");
        let path = cache.path_for(url);
        assert!(path.is_file() && std::fs::metadata(&path).unwrap().len() > 0);

        // A fresh cache over the same dir serves it straight from disk (Ready, no fetch/spinner).
        let fresh = DiskImageCache::new(dir.path());
        assert!(matches!(fresh.load(&ctx, url), Ok(BytesPoll::Ready { .. })));
    }
}
