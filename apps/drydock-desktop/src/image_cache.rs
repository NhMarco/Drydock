//! A persistent, disk-backed `egui` bytes loader for the storefront/library CDN images.
//!
//! egui's default image loader keeps decoded textures in memory only, so every artwork is
//! re-downloaded from the Steam CDN on each app launch. This loader adds a disk cache under
//! `cache/images/`: a successfully fetched image is written there and, on any later frame or launch,
//! served straight from disk (instant, no spinner). It must be registered **after**
//! `egui_extras::install_image_loaders` so it wins over the default HTTP loader (egui tries the most
//! recently added bytes loader first). Non-`http(s)` URIs are declined so egui's other loaders
//! (`bytes://`, `file://`, `include_image!`) still work.
//!
//! Two things keep it bounded, because a catalog of 70k+ games means effectively unbounded artwork:
//!
//! * **In memory**, compressed bytes are held in a small LRU (see [`MAXIMUM_MEMORY_BYTES`]). Previously
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

/// How much uploaded artwork may stay resident once it has left the screen.
///
/// With `reduce_texture_memory` on, a texture is the only copy of its image, so evicting one means
/// fetching and decoding it again when it comes back. Below this budget nothing is evicted, so paging
/// back or scrolling a list back is instant; above it the images off screen longest go first.
const TEXTURE_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Owns the HTTP textures so artwork that has left the screen cannot accumulate without bound in
/// egui's default texture cache.
pub struct VisibleTextureLoader {
    inner: eframe::egui::load::DefaultTextureLoader,
    budget: usize,
    usage: Mutex<TextureUsage>,
    retired: Mutex<Vec<String>>,
}

#[derive(Default)]
struct TextureUsage {
    /// The last pass that ended; a URI loaded during the current pass is stamped with it.
    pass: u64,
    /// Per URI: the stamp of the pass it was last loaded in, and whether it has become a texture.
    uris: HashMap<String, (u64, bool)>,
}

impl Default for VisibleTextureLoader {
    fn default() -> Self {
        Self::with_budget(TEXTURE_BUDGET_BYTES)
    }
}

impl VisibleTextureLoader {
    fn with_budget(budget: usize) -> Self {
        Self {
            inner: eframe::egui::load::DefaultTextureLoader::default(),
            budget,
            usage: Mutex::default(),
            retired: Mutex::default(),
        }
    }

    /// Called by the UI before loading images, outside egui's loader-registry locks.
    pub fn forget_retired(&self, ctx: &Context) {
        let retired = std::mem::take(&mut *self.retired.lock().unwrap_or_else(|error| error.into_inner()));
        for uri in retired {
            ctx.forget_image(&uri);
        }
    }
}

impl eframe::egui::load::TextureLoader for VisibleTextureLoader {
    fn id(&self) -> &'static str {
        "drydock-visible-textures"
    }

    fn load(
        &self,
        ctx: &Context,
        uri: &str,
        options: eframe::egui::TextureOptions,
        size: eframe::egui::load::SizeHint,
    ) -> eframe::egui::load::TextureLoadResult {
        if !uri.starts_with("http://") && !uri.starts_with("https://") {
            return Err(LoadError::NotSupported);
        }
        let result = self.inner.load(ctx, uri, options, size);
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        match &result {
            // A failure stays with the loaders that remember it, so a dead URL is not requested again
            // every time its row scrolls back into view.
            Err(_) => {
                usage.uris.remove(uri);
            }
            Ok(poll) => {
                let used = (
                    usage.pass,
                    matches!(poll, eframe::egui::load::TexturePoll::Ready { .. }),
                );
                if let Some(entry) = usage.uris.get_mut(uri) {
                    *entry = used;
                } else {
                    usage.uris.insert(uri.to_owned(), used);
                }
            }
        }
        result
    }

    fn end_pass(&self, pass: u64) {
        self.inner.end_pass(pass);
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        usage.pass = pass;
        let mut retired = self.retired.lock().unwrap_or_else(|error| error.into_inner());
        // A load abandoned before it finished holds no texture. Forgetting it right away cancels the
        // fetch and frees whatever the loaders buffered for an image nobody is looking at.
        usage.uris.retain(|uri, (last, ready)| {
            let abandoned = !*ready && last.saturating_add(1) < pass;
            if abandoned {
                retired.push(uri.clone());
            }
            !abandoned
        });
        if self.inner.byte_size() <= self.budget {
            return;
        }
        let mut off_screen: Vec<(u64, String)> = usage
            .uris
            .iter()
            .filter(|(_, (last, _))| last.saturating_add(1) < pass)
            .map(|(uri, (last, _))| (*last, uri.clone()))
            .collect();
        off_screen.sort_unstable();
        for (_, uri) in off_screen {
            self.inner.forget(&uri);
            usage.uris.remove(&uri);
            retired.push(uri);
            if self.inner.byte_size() <= self.budget {
                break;
            }
        }
    }

    fn forget(&self, uri: &str) {
        self.inner.forget(uri);
        self.usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .uris
            .remove(uri);
    }

    fn forget_all(&self) {
        self.inner.forget_all();
        self.usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .uris
            .clear();
    }

    fn byte_size(&self) -> usize {
        self.inner.byte_size()
    }
}

/// How much compressed image data to keep resident. Steam headers are ~30–60 KB, so this holds several
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
                        && may_be_image(response.content_type())
                        && !response.bytes.is_empty()
                        && response.bytes.len() <= MAXIMUM_IMAGE_BYTES =>
                {
                    // Best-effort disk cache; a write failure just means it reloads next time.
                    let _ = write_atomically(&path, &response.bytes);
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

/// Whether a response may be an image. One that says otherwise — an HTML error or captive-portal page
/// served with status 200 — must not reach the disk cache, which would serve it on every launch.
fn may_be_image(content_type: Option<&str>) -> bool {
    content_type.is_none_or(|kind| {
        let kind = kind.trim_start().to_ascii_lowercase();
        kind.starts_with("image/") || kind.starts_with("application/octet-stream")
    })
}

/// Writes a cache file under a temporary name first, so a crash mid-write never leaves a truncated
/// image behind for later launches to serve. Leftover temporary files age out like any other entry.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_extension(format!("{}-{serial}.tmp", std::process::id()));
    let result = std::fs::write(&temporary, bytes).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_image_responses_are_cached() {
        for kind in [
            None,
            Some("image/jpeg"),
            Some("Image/PNG; charset=binary"),
            Some("application/octet-stream"),
        ] {
            assert!(may_be_image(kind), "{kind:?}");
        }
        for kind in [Some("text/html; charset=utf-8"), Some("application/json")] {
            assert!(!may_be_image(kind), "{kind:?}");
        }
    }

    #[test]
    fn cache_files_are_written_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("0123abcd");
        write_atomically(&path, b"first").unwrap();
        write_atomically(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temporary file is left"
        );
    }

    #[test]
    fn offscreen_textures_are_released_only_over_budget() {
        use eframe::egui::load::{ImageLoadResult, ImageLoader, ImagePoll, SizeHint, TextureLoader};
        use eframe::egui::{Color32, ColorImage, TextureOptions};
        /// Every URI is a 4×4 image, except `failed` ones, which do not load.
        struct Fixture;
        impl ImageLoader for Fixture {
            fn id(&self) -> &str {
                "fixture"
            }
            fn load(&self, _: &Context, uri: &str, _: SizeHint) -> ImageLoadResult {
                if uri.contains("failed") {
                    return Err(LoadError::Loading("404".into()));
                }
                Ok(ImagePoll::Ready {
                    image: Arc::new(ColorImage::filled([4, 4], Color32::WHITE)),
                })
            }
            fn forget(&self, _: &str) {}
            fn forget_all(&self) {}
            fn byte_size(&self) -> usize {
                0
            }
        }
        let ctx = Context::default();
        ctx.add_image_loader(Arc::new(Fixture));
        let load = |loader: &VisibleTextureLoader, name: &str| {
            loader.load(
                &ctx,
                &format!("https://fixture/{name}.png"),
                TextureOptions::default(),
                SizeHint::default(),
            )
        };

        // Within budget nothing is evicted, so artwork scrolled away and back needs no refetch.
        let roomy = VisibleTextureLoader::with_budget(usize::MAX);
        for index in 0..100 {
            load(&roomy, &index.to_string()).unwrap();
        }
        roomy.end_pass(1);
        roomy.end_pass(2);
        assert_eq!(roomy.byte_size(), 100 * 4 * 4 * 4);

        // Over budget the images off screen go, while the one still drawn stays; a failure is never
        // retired, so its loaders keep remembering it.
        let tight = VisibleTextureLoader::with_budget(0);
        for index in 0..100 {
            load(&tight, &index.to_string()).unwrap();
        }
        assert!(load(&tight, "failed").is_err());
        tight.end_pass(1);
        load(&tight, "0").unwrap();
        tight.end_pass(2);
        assert_eq!(tight.byte_size(), 4 * 4 * 4);
        let retired = tight.retired.lock().unwrap();
        assert_eq!(retired.len(), 99);
        assert!(!retired.iter().any(|uri| uri.contains("failed")));
    }

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
