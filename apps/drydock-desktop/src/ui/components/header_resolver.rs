use drydock_core::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub enum HeaderSlot {
    /// A resolution is in flight (or queued) — don't enqueue it again.
    Pending,
    /// The real `header_image` URL from Steam's `appdetails`.
    Resolved(String),
    /// `appdetails` had no usable header (unlikely, e.g. a delisted app).
    Failed,
}

/// How many resolver threads run in parallel. Each pulls App IDs off the shared queue and calls
/// `appdetails`; the store's token-bucket limiter still caps the aggregate request rate, so this only

/// parallelises latency, not store throughput.
pub const HEADER_RESOLVER_WORKERS: usize = 4;

/// Upper bound on queued-but-unstarted header resolutions.
///
/// Scrolling a long list fast used to enqueue every row whose CDN guess failed — hundreds of App IDs,
/// which at the store's sustained refill rate meant ten-plus minutes of `appdetails` traffic that kept
/// running long after the user had left the page. The queue is now a bounded *most-recently-requested*
/// window: a push past the cap drops the oldest entry, because the oldest request is the one least
/// likely to still be on screen.
pub const HEADER_RESOLVER_QUEUE_LIMIT: usize = 48;

/// Resolves a catalog row's real header art on demand.
///
/// The App-ID-derived CDN URLs ([`steam_artwork_urls`]) 404 for titles Steam migrated to hashed
/// `store_item_assets` paths, so those rows would otherwise show a blank tile. This asks Steam's
/// `appdetails` for the current `header_image` — one lightweight request per app, cached on disk for
/// two weeks (see [`SteamStoreClient::header_image`]). Resolution is only ever requested for rows
/// whose cheap CDN guesses failed, runs on a small pool of background workers so several resolve in
/// parallel, and never blocks the render thread, which only reads the cached result.

#[derive(Clone)]
pub struct HeaderResolver {
    pub slots: Arc<Mutex<HashMap<u32, HeaderSlot>>>,
    pub queue: Arc<(Mutex<std::collections::VecDeque<u32>>, std::sync::Condvar)>,
}

impl HeaderResolver {
    pub fn new(cache_directory: PathBuf, ctx: egui::Context) -> Self {
        let slots: Arc<Mutex<HashMap<u32, HeaderSlot>>> = Arc::new(Mutex::new(HashMap::new()));
        let queue: Arc<(Mutex<std::collections::VecDeque<u32>>, std::sync::Condvar)> = Arc::new((
            Mutex::new(std::collections::VecDeque::new()),
            std::sync::Condvar::new(),
        ));
        for _ in 0..HEADER_RESOLVER_WORKERS {
            let worker_slots = Arc::clone(&slots);
            let worker_queue = Arc::clone(&queue);
            let cache_directory = cache_directory.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let Ok(client) = SteamStoreClient::new(&cache_directory) else {
                    return;
                };
                loop {
                    // Take the next queued App ID, waiting on the condvar while the queue is empty.
                    let app_id = {
                        let (lock, cvar) = &*worker_queue;
                        let mut queue = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        while queue.is_empty() {
                            queue = cvar
                                .wait(queue)
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                        }
                        queue.pop_front().expect("queue non-empty after wait")
                    };
                    // The network call happens outside every lock, so the workers actually overlap
                    // (bounded only by the store's shared request limiter).
                    let slot = match client.header_image(app_id) {
                        Ok(url) => HeaderSlot::Resolved(url),
                        Err(_) => HeaderSlot::Failed,
                    };
                    if let Ok(mut map) = worker_slots.lock() {
                        map.insert(app_id, slot);
                    }
                    // Wake the UI so freshly-resolved art paints even when the window is unfocused.
                    ctx.request_repaint();
                }
            });
        }
        Self { slots, queue }
    }

    /// The resolved header URL if one is already known — never triggers a request. Returns `None`
    /// while unknown, pending, or failed.
    pub fn get(&self, app_id: u32) -> Option<String> {
        let map = self.slots.lock().ok()?;
        match map.get(&app_id) {
            Some(HeaderSlot::Resolved(url)) => Some(url.clone()),
            _ => None,
        }
    }

    /// Enqueues a one-off background resolution the first time an app is requested. A no-op once the
    /// app is pending, resolved, or known-failed, so it is safe to call every frame. Only rows whose
    /// cheap CDN guesses fail ever call this, keeping `appdetails` traffic minimal (and well under
    /// Steam's rate limit).
    ///
    /// The queue is capped at [`HEADER_RESOLVER_QUEUE_LIMIT`]; a request that overflows it evicts the
    /// oldest pending App ID (and its `Pending` marker, so it can be asked for again if still
    /// visible). Scrolling therefore keeps the work focused on what the user is actually looking at
    /// instead of working through a long backlog of rows that have scrolled away.
    pub fn request(&self, app_id: u32) {
        let Ok(mut map) = self.slots.lock() else {
            return;
        };
        if map.contains_key(&app_id) {
            return;
        }
        map.insert(app_id, HeaderSlot::Pending);
        drop(map);
        let (lock, cvar) = &*self.queue;
        if let Ok(mut queue) = lock.lock() {
            queue.push_back(app_id);
            while queue.len() > HEADER_RESOLVER_QUEUE_LIMIT {
                if let Some(dropped) = queue.pop_front()
                    && let Ok(mut map) = self.slots.lock()
                {
                    // Forget the marker too: the row may come back into view and should then be
                    // re-requested rather than staying `Pending` forever.
                    map.remove(&dropped);
                }
            }
            cvar.notify_one();
        }
    }

    /// Drops everything still waiting to be resolved, e.g. when the user leaves a long list.
    ///
    /// Work already in flight finishes (it is one cheap request), but the backlog does not outlive the
    /// page that created it.
    pub fn cancel_pending(&self) {
        let (lock, _) = &*self.queue;
        let Ok(mut queue) = lock.lock() else {
            return;
        };
        let dropped: Vec<u32> = queue.drain(..).collect();
        drop(queue);
        if let Ok(mut map) = self.slots.lock() {
            for app_id in dropped {
                map.remove(&app_id);
            }
        }
    }
}
