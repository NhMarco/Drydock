use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::version::user_agent;

const CACHE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);
/// The live storefront (Top Sellers, New Releases, Specials) shifts through the day, so its cache is
/// short — long enough to keep the Store tab snappy across navigations, short enough to stay "live".
const FEATURED_CACHE_LIFETIME: Duration = Duration::from_secs(60 * 60);
/// Card facts (header art + Denuvo status) change rarely, so they are cached far longer than
/// full details, but not forever: a game that has Denuvo removed refreshes within this window.
const CARD_CACHE_LIFETIME: Duration = Duration::from_secs(14 * 24 * 60 * 60);
const CACHE_FORMAT_VERSION: u8 = 6;
const MAXIMUM_RESPONSE_BYTES: usize = 3 * 1024 * 1024;
/// Steam-request pacing as a token bucket rather than a strict spacing gate. Steam's storefront
/// (`appdetails`) rate-limits by IP at roughly 200 requests per 5-minute window (~0.66/s sustained),
/// then answers `429` for the rest of the window — a limit that falls on the *user's* IP, so we stay
/// safely under it. The bucket allows a burst (the first visible screenful resolves together and in
/// parallel) but the sustained refill is conservative, and every resolved header is cached on disk
/// for two weeks so a game is only ever fetched once. A `429` still trips a global cooldown as a
/// backstop. `burst + refill·300 ≈ 170` stays comfortably below the 200/5-min ceiling.
const REQUEST_BURST: f64 = 20.0;
const REQUEST_REFILL_PER_SEC: f64 = 0.5;
/// How long all Steam requests pause after Steam answers `429`, to let the rate-limit window recover
/// before we risk escalating it.
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(180);
/// The longest a single caller blocks inside the pacing gate before giving up with
/// [`SteamStoreError::RateLimited`].
///
/// Waiting out a full [`RATE_LIMIT_COOLDOWN`] used to happen inline, so clicking a store page during
/// a cooldown looked like a three-minute hang with no explanation. The cooldown itself still stands
/// (the gate keeps refusing until it expires) — callers just find out quickly instead of blocking.
const MAXIMUM_GATE_WAIT: Duration = Duration::from_secs(10);
/// Apps whose `appdetails` lookup failed are remembered for this long so a delisted or region-locked
/// title is not re-requested on every launch. Much shorter than [`CARD_CACHE_LIFETIME`]: a failure
/// can be temporary, a success cannot go stale as fast.
const CARD_FAILURE_CACHE_LIFETIME: Duration = Duration::from_secs(3 * 24 * 60 * 60);
/// Marker written to a header cache file when Steam had no usable header for the app.
const CARD_FAILURE_MARKER: &str = "-";
/// Cached store files older than this are swept on startup. Generous, because a sweep only reclaims
/// disk: the per-entry TTLs above already decide what is *served*.
const CACHE_SWEEP_MAX_AGE: Duration = Duration::from_secs(60 * 24 * 60 * 60);
static REQUEST_GATE: OnceLock<Mutex<RequestGate>> = OnceLock::new();

struct RequestGate {
    tokens: f64,
    last: Instant,
    /// When set and still in the future, every request waits until this instant (set on a `429`).
    cooldown_until: Option<Instant>,
}

fn request_gate() -> &'static Mutex<RequestGate> {
    REQUEST_GATE.get_or_init(|| {
        Mutex::new(RequestGate {
            tokens: REQUEST_BURST,
            last: Instant::now(),
            cooldown_until: None,
        })
    })
}

/// Records that Steam rate-limited us, pausing all storefront requests for [`RATE_LIMIT_COOLDOWN`].
fn note_rate_limited() {
    let gate = request_gate();
    let mut gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.cooldown_until = Some(Instant::now() + RATE_LIMIT_COOLDOWN);
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SteamStoreDetails {
    #[serde(default)]
    pub cache_format_version: u8,
    #[serde(skip)]
    pub stale_cache: bool,
    pub app_id: u32,
    pub name: String,
    pub short_description: String,
    pub about_the_game: String,
    pub header_image_url: String,
    pub developers: Vec<String>,
    pub publishers: Vec<String>,
    pub genres: Vec<String>,
    pub platforms: Vec<String>,
    pub screenshots: Vec<String>,
    pub release_date: String,
    pub price: String,
    pub metacritic_score: Option<u64>,
    /// Steam's third-party DRM notice verbatim (e.g. "Denuvo Anti-Tampering"), empty if none.
    #[serde(default)]
    pub drm_notice: String,
    /// Whether Steam declares Denuvo DRM for this app (the activation-needed signal).
    #[serde(default)]
    pub uses_denuvo: bool,
    #[serde(default)]
    pub reviews: SteamReviewSummary,
    pub requirements: SteamSystemRequirements,
    /// DLC App IDs from the store listing (for the emulator-template generator).
    #[serde(default)]
    pub dlc: Vec<u32>,
    /// Supported languages (lowercased names, e.g. `english`), parsed from the store listing.
    #[serde(default)]
    pub languages: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SteamReviewSummary {
    pub description: String,
    pub total_positive: u64,
    pub total_negative: u64,
    pub total_reviews: u64,
}

impl SteamReviewSummary {
    #[must_use]
    pub fn display_text(&self) -> String {
        if self.total_reviews == 0 {
            return "Not available".into();
        }
        let positive_percent = self.total_positive as f64 * 100.0 / self.total_reviews as f64;
        let description = if self.description.trim().is_empty() {
            "User reviews"
        } else {
            self.description.trim()
        };
        format!(
            "{description} ({positive_percent:.0}% positive, {} reviews)",
            format_count(self.total_reviews)
        )
    }
}

fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            output.push(',');
        }
        output.push(character);
    }
    output
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SteamSystemRequirements {
    pub minimum: String,
    pub recommended: String,
}

/// One game in a live storefront shelf (Top Sellers / New Releases / Specials): enough to draw a
/// capsule card with real Steam art, a price, and — for the Store's discount ribbon — a percentage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoreCapsule {
    pub app_id: u32,
    pub name: String,
    /// The landscape capsule art Steam serves for this item (already a real CDN URL).
    pub header_image_url: String,
    /// Formatted final price (e.g. "$59.99", "Free"), ready to paint.
    pub price: String,
    /// Percentage off, 0 when not discounted.
    pub discount_percent: u32,
}

/// The Steam storefront's live categories, as read from `featuredcategories`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StoreFeatured {
    #[serde(default)]
    pub cache_format_version: u8,
    pub top_sellers: Vec<StoreCapsule>,
    pub new_releases: Vec<StoreCapsule>,
    pub specials: Vec<StoreCapsule>,
    pub coming_soon: Vec<StoreCapsule>,
}

pub struct SteamStoreClient {
    client: Client,
    cache_directory: PathBuf,
}

impl SteamStoreClient {
    pub fn new(cache_directory: impl Into<PathBuf>) -> Result<Self, SteamStoreError> {
        let client = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(18))
            .build()?;
        Ok(Self {
            client,
            cache_directory: cache_directory.into(),
        })
    }

    pub fn details(&self, app_id: u32) -> Result<SteamStoreDetails, SteamStoreError> {
        if app_id == 0 {
            return Err(SteamStoreError::InvalidAppId);
        }
        let cache_path = self.cache_directory.join(format!("{app_id}.json"));
        let cached = read_cache(&cache_path).ok();
        if cache_is_fresh(&cache_path)
            && let Some(cached) = cached.as_ref()
            && cached.cache_format_version == CACHE_FORMAT_VERSION
        {
            return Ok(cached.clone());
        }

        let details_url =
            format!("https://store.steampowered.com/api/appdetails?appids={app_id}&cc=us&l=english");
        let review_url = format!(
            "https://store.steampowered.com/appreviews/{app_id}?json=1&language=all&purchase_type=all&num_per_page=0"
        );
        let (details, reviews) = std::thread::scope(|scope| {
            let review_client = self.client.clone();
            let review = scope.spawn(move || load_review_summary(&review_client, &review_url));
            let details =
                load_json(&self.client, &details_url).and_then(|document| parse_details(app_id, &document));
            let reviews = review.join().ok().and_then(Result::ok);
            (details, reviews)
        });
        let mut details = match details {
            Ok(details) => details,
            Err(error) => {
                if let Some(mut cached) = cached {
                    cached.stale_cache = true;
                    return Ok(cached);
                }
                return Err(error);
            }
        };
        if let Some(reviews) = reviews {
            details.reviews = reviews;
        }
        let _ = write_cache(&cache_path, &details);
        Ok(details)
    }

    /// Resolves just the real header image URL for a card, cached as a small text file.
    ///
    /// Steam migrated many apps to hashed `store_item_assets` URLs, so the legacy
    /// `…/steam/apps/{id}/header.jpg` catalog card URL 404s for them. This fetches the current
    /// URL from a single lightweight `filters=basic` request, paced by the shared limiter.
    /// (Denuvo status comes from the bulk `denuvo` curator list, not per app.)
    pub fn header_image(&self, app_id: u32) -> Result<String, SteamStoreError> {
        if app_id == 0 {
            return Err(SteamStoreError::InvalidAppId);
        }
        let cache_path = self.cache_directory.join("headers").join(format!("{app_id}.txt"));
        // Both outcomes are cached. A *failure* has to be remembered too: without it every launch
        // re-requested the same delisted/region-locked apps, and since only rows whose cheap CDN
        // guesses failed get here, that was a steady drip of pointless `appdetails` traffic against
        // the user's IP — the thing the pacing gate exists to avoid.
        if let Some(age) = cache_age(&cache_path)
            && let Ok(cached) = fs::read_to_string(&cache_path)
        {
            let cached = cached.trim();
            if cached.starts_with("https://") && age <= CARD_CACHE_LIFETIME {
                return Ok(cached.to_owned());
            }
            if cached == CARD_FAILURE_MARKER && age <= CARD_FAILURE_CACHE_LIFETIME {
                return Err(SteamStoreError::Unavailable(app_id));
            }
        }
        let url = format!(
            "https://store.steampowered.com/api/appdetails?appids={app_id}&filters=basic&cc=us&l=english"
        );
        let header = self.fetch_header_image(app_id, &url);
        // Only a definitive "Steam has no header for this app" is worth remembering. A rate limit or
        // a network blip must not be cached as a failure, or a bad minute would blank out artwork for
        // the next three days.
        let write = match &header {
            Ok(url) => Some(url.clone()),
            Err(SteamStoreError::Unavailable(_)) => Some(CARD_FAILURE_MARKER.to_owned()),
            Err(_) => None,
        };
        if let Some(contents) = write {
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&cache_path, contents);
        }
        header
    }

    /// The network half of [`SteamStoreClient::header_image`], split out so the caller can decide what
    /// to cache based on which error came back.
    fn fetch_header_image(&self, app_id: u32, url: &str) -> Result<String, SteamStoreError> {
        let document = load_json(&self.client, url)?;
        let app = document
            .get(app_id.to_string())
            .ok_or(SteamStoreError::Unavailable(app_id))?;
        if app.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(SteamStoreError::Unavailable(app_id));
        }
        let header = app
            .get("data")
            .map(|data| string(data, "header_image"))
            .unwrap_or_default()
            .to_owned();
        if !header.starts_with("https://") {
            return Err(SteamStoreError::Unavailable(app_id));
        }
        Ok(header)
    }

    /// Deletes store cache files that have not been touched in [`CACHE_SWEEP_MAX_AGE`].
    ///
    /// The per-app details and header files accumulate one entry per app the user ever looked at and
    /// were never cleaned up, so the directory only ever grew. Returns the number of files removed.
    /// Best-effort: anything that cannot be read or deleted is skipped.
    pub fn sweep_stale_cache(cache_directory: &Path) -> usize {
        let mut removed = 0;
        let directories = [cache_directory.to_path_buf(), cache_directory.join("headers")];
        for directory in directories {
            let Ok(entries) = fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                    continue;
                }
                // `featured.json` is refreshed hourly and is never stale for long; leave it alone.
                if path.file_name().is_some_and(|name| name == "featured.json") {
                    continue;
                }
                if cache_age(&path).is_some_and(|age| age > CACHE_SWEEP_MAX_AGE)
                    && fs::remove_file(&path).is_ok()
                {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// Fetches Steam's live storefront categories (Top Sellers, New Releases, Specials, Coming Soon)
    /// from the public `featuredcategories` endpoint, cached briefly so the Store tab stays snappy.
    /// A stale cache is returned when the network is down so the Store still renders offline.
    pub fn featured(&self) -> Result<StoreFeatured, SteamStoreError> {
        let cache_path = self.cache_directory.join("featured.json");
        let cached = read_featured(&cache_path).ok();
        if cache_age(&cache_path).is_some_and(|age| age <= FEATURED_CACHE_LIFETIME)
            && let Some(cached) = cached.as_ref()
            && cached.cache_format_version == CACHE_FORMAT_VERSION
        {
            return Ok(cached.clone());
        }

        let url = "https://store.steampowered.com/api/featuredcategories?cc=us&l=english";
        let document = match load_json(&self.client, url) {
            Ok(document) => document,
            Err(error) => return cached.ok_or(error),
        };
        // Top Sellers comes from Steam's ranked top-100 search (so the UI can filter it down to the
        // games actually in our catalog); the small curated `featuredcategories` list is the fallback.
        let mut top_sellers = self.top_sellers_ranked().unwrap_or_default();
        if top_sellers.is_empty() {
            top_sellers = parse_capsules(document.get("top_sellers"));
        }
        let featured = StoreFeatured {
            cache_format_version: CACHE_FORMAT_VERSION,
            top_sellers,
            new_releases: parse_capsules(document.get("new_releases")),
            specials: parse_capsules(document.get("specials")),
            coming_soon: parse_capsules(document.get("coming_soon")),
        };
        if featured.top_sellers.is_empty() && featured.new_releases.is_empty() {
            // A 200 with no usable items (region gate, schema drift): keep any cache we had.
            if let Some(cached) = cached {
                return Ok(cached);
            }
            return Err(SteamStoreError::InvalidResponse("featured categories"));
        }
        let _ = write_featured(&cache_path, &featured);
        Ok(featured)
    }

    /// Steam's ranked Top Sellers (up to the top 100), from the store search endpoint. Returned in
    /// rank order so the caller can intersect it with our catalog and show the best-selling games
    /// that are actually available. Not cached itself — it's folded into the `featured()` cache.
    fn top_sellers_ranked(&self) -> Result<Vec<StoreCapsule>, SteamStoreError> {
        let url = "https://store.steampowered.com/search/results/\
                   ?filter=topsellers&start=0&count=100&cc=us&l=english&infinite=1&json=1&ndl=1";
        let document = load_json(&self.client, url)?;
        let html = document
            .get("results_html")
            .and_then(Value::as_str)
            .ok_or(SteamStoreError::InvalidResponse("top sellers search"))?;
        Ok(parse_topsellers_html(html))
    }
}

/// Parses the `results_html` of Steam's Top Sellers search into capsule cards, in rank order. Each
/// result row carries `data-ds-itemkey="App_<id>"` (only real apps — bundles/packages are keyed
/// `Bundle_`/`Sub_` and thus skipped), a `search_capsule` image, and a `<span class="title">`. The
/// capsule URL is kept verbatim (it always resolves, unlike a guessed `header.jpg`).
fn parse_topsellers_html(html: &str) -> Vec<StoreCapsule> {
    let mut capsules = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in html.split("data-ds-itemkey=\"App_").skip(1) {
        let id: String = row.chars().take_while(char::is_ascii_digit).collect();
        let Ok(app_id) = id.parse::<u32>() else {
            continue;
        };
        if app_id == 0 || !seen.insert(app_id) {
            continue;
        }
        let name = between(row, "<span class=\"title\">", "</span>")
            .map(|title| decode_html_entities(title.trim()))
            .unwrap_or_default();
        if name.is_empty() || name.len() > 200 {
            continue;
        }
        // The capsule image (the `<img>` inside `search_capsule`), kept exactly as Steam serves it.
        // The URL now carries a content-hash folder and a `?t=` version, and some titles ship only a
        // `capsule_231x87_alt_assets_*.jpg` variant, so a naive rewrite to `header.jpg` 404s — and
        // coming-soon games have no `header.jpg` at all, only this capsule. The banner still tries the
        // sharp `library_hero.jpg` first (falling back to this capsule), so real games stay crisp.
        let header_image_url = row
            .find("search_capsule")
            .and_then(|start| between(&row[start..], "src=\"", "\""))
            .filter(|src| src.starts_with("https://"))
            .unwrap_or_default()
            .to_owned();
        capsules.push(StoreCapsule {
            app_id,
            name,
            header_image_url,
            price: String::new(),
            discount_percent: 0,
        });
    }
    capsules
}

/// The substring between `open` and the next `close` after it, if both are present.
fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    let end = rest.find(close)?;
    Some(&rest[..end])
}

/// Decodes the handful of HTML entities Steam's search titles use (`&amp;`, `&#39;`, …).
fn decode_html_entities(text: &str) -> String {
    text.replace("&trade;", "™")
        .replace("&#8482;", "™")
        .replace("&reg;", "®")
        .replace("&#174;", "®")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        // `&amp;` last so it doesn't turn `&amp;trade;` into a real ™, etc.
        .replace("&amp;", "&")
}

/// Parses one `featuredcategories` category node (`{ items: [...] }`) into capsule cards, dropping
/// bundle/package rows (no numeric appid) and anything without real capsule art.
fn parse_capsules(category: Option<&Value>) -> Vec<StoreCapsule> {
    let Some(items) = category
        .and_then(|node| node.get("items"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut capsules = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let Some(app_id) = item.get("id").and_then(Value::as_u64) else {
            continue;
        };
        if app_id == 0 || app_id > u64::from(u32::MAX) {
            continue;
        }
        // Steam repeats some items (regional sub-packages, hardware) across a category; keep the
        // first of each App ID so a title never shows up twice in a shelf.
        if !seen.insert(app_id) {
            continue;
        }
        // Bundles/subs carry their own type ids; only ship rows Steam marks as apps (or unmarked).
        if item
            .get("type")
            .and_then(Value::as_u64)
            .is_some_and(|kind| kind != 0)
        {
            continue;
        }
        let name = string(item, "name").trim().to_owned();
        if name.is_empty() || name.len() > 200 {
            continue;
        }
        // Valve's own hardware (Steam Machine, Steam Deck, Index, …) shows up in the storefront
        // categories but is not a game Drydock can do anything with, so drop it.
        if is_valve_hardware(&name) {
            continue;
        }
        let header = [
            string(item, "header_image"),
            string(item, "large_capsule_image"),
            string(item, "small_capsule_image"),
        ]
        .into_iter()
        .map(str::trim)
        .find(|url| url.starts_with("https://"))
        .unwrap_or_default()
        .to_owned();
        if header.is_empty() {
            continue;
        }
        capsules.push(StoreCapsule {
            app_id: app_id as u32,
            name,
            header_image_url: header,
            price: capsule_price(item),
            discount_percent: item
                .get("discount_percent")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(100) as u32,
        });
    }
    capsules
}

/// Formats a storefront item's price from the integer-cent fields `featuredcategories` returns.
fn capsule_price(item: &Value) -> String {
    if item.get("discounted").and_then(Value::as_bool) == Some(false)
        && item.get("final_price").and_then(Value::as_u64) == Some(0)
        && item.get("original_price").is_none()
    {
        return "Free".into();
    }
    let Some(final_cents) = item.get("final_price").and_then(Value::as_u64) else {
        return String::new();
    };
    if final_cents == 0 {
        return "Free".into();
    }
    let currency = string(item, "currency").trim();
    format_price(final_cents, currency)
}

/// Renders integer cents in a currency as a symbol-prefixed amount, falling back to a trailing code.
fn format_price(cents: u64, currency: &str) -> String {
    let amount = cents as f64 / 100.0;
    match currency.to_ascii_uppercase().as_str() {
        "USD" | "" => format!("${amount:.2}"),
        "EUR" => format!("€{amount:.2}"),
        "GBP" => format!("£{amount:.2}"),
        other => format!("{amount:.2} {other}"),
    }
}

fn read_featured(path: &Path) -> Result<StoreFeatured, SteamStoreError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_featured(path: &Path, featured: &StoreFeatured) -> Result<(), SteamStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| SteamStoreError::InvalidCachePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(featured)?)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

/// Detects whether Steam declares Denuvo DRM for an app, returning its DRM notice verbatim.
///
/// Steam exposes third-party DRM in the `drm_notice` field (occasionally also
/// `ext_user_account_notice`), e.g. "Denuvo Anti-Tampering". This is the authoritative signal:
/// publishers keep it current, so a game that had Denuvo removed no longer lists it. A `false`
/// result means Steam declares no Denuvo for the app.
fn detect_denuvo(data: &Value) -> (String, bool) {
    let drm_notice = clean_html(string(data, "drm_notice"));
    let mentions_denuvo = |text: &str| text.to_ascii_lowercase().contains("denuvo");
    let uses_denuvo =
        mentions_denuvo(&drm_notice) || mentions_denuvo(string(data, "ext_user_account_notice"));
    (drm_notice, uses_denuvo)
}

fn load_json(client: &Client, url: &str) -> Result<Value, SteamStoreError> {
    let mut response = None;
    for attempt in 0..=1 {
        wait_for_request_turn()?;
        let candidate = client.get(url).send()?;
        if candidate.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Steam rate-limited us: pause every storefront request for a cooldown so we don't
            // escalate it against the user's IP, then (on the first attempt) retry once.
            note_rate_limited();
            if attempt == 0 {
                continue;
            }
        }
        response = Some(candidate.error_for_status()?);
        break;
    }
    let response = response.ok_or(SteamStoreError::InvalidResponse("rate-limit response"))?;
    if response
        .content_length()
        .is_some_and(|size| size > MAXIMUM_RESPONSE_BYTES as u64)
    {
        return Err(SteamStoreError::ResponseTooLarge);
    }
    let bytes = response.bytes()?;
    if bytes.len() > MAXIMUM_RESPONSE_BYTES {
        return Err(SteamStoreError::ResponseTooLarge);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn load_review_summary(client: &Client, url: &str) -> Result<SteamReviewSummary, SteamStoreError> {
    let document = load_json(client, url)?;
    let summary = document
        .get("query_summary")
        .ok_or(SteamStoreError::InvalidResponse("review summary"))?;
    Ok(parse_review_summary(summary))
}

fn parse_review_summary(summary: &Value) -> SteamReviewSummary {
    SteamReviewSummary {
        description: string(summary, "review_score_desc").trim().to_owned(),
        total_positive: summary.get("total_positive").and_then(Value::as_u64).unwrap_or(0),
        total_negative: summary.get("total_negative").and_then(Value::as_u64).unwrap_or(0),
        total_reviews: summary.get("total_reviews").and_then(Value::as_u64).unwrap_or(0),
    }
}

/// Waits for this thread's turn to make a Steam request, or gives up with
/// [`SteamStoreError::RateLimited`] once [`MAXIMUM_GATE_WAIT`] has elapsed.
///
/// Token bucket: refill `REQUEST_REFILL_PER_SEC` tokens/second up to `REQUEST_BURST`, spend one per
/// request. The lock is released before any sleep, so concurrent callers can spend the burst in
/// parallel instead of serializing the way a held spacing gate would. A pending `429` cooldown takes
/// precedence over the bucket.
///
/// The bounded wait matters for the UI: a 180-second cooldown used to be slept through inline, so a
/// click on a store page could sit there for three minutes looking broken. Returning an error lets
/// the caller say *why* nothing is happening, and the cooldown still holds off the next request.
fn wait_for_request_turn() -> Result<(), SteamStoreError> {
    let deadline = Instant::now() + MAXIMUM_GATE_WAIT;
    loop {
        let sleep_for = {
            let gate = request_gate();
            let mut gate = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            match gate.cooldown_until {
                // A 429 cooldown is still in force: wait it out before touching the bucket.
                Some(until) if until > now => until - now,
                _ => {
                    gate.cooldown_until = None;
                    let elapsed = now.duration_since(gate.last).as_secs_f64();
                    gate.tokens = (gate.tokens + elapsed * REQUEST_REFILL_PER_SEC).min(REQUEST_BURST);
                    gate.last = now;
                    if gate.tokens >= 1.0 {
                        gate.tokens -= 1.0;
                        return Ok(());
                    }
                    Duration::from_secs_f64((1.0 - gate.tokens) / REQUEST_REFILL_PER_SEC)
                }
            }
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(SteamStoreError::RateLimited {
                retry_after: sleep_for,
            });
        }
        std::thread::sleep(sleep_for.min(deadline - now));
    }
}

fn cache_age(path: &Path) -> Option<Duration> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
}

fn cache_is_fresh(path: &Path) -> bool {
    cache_age(path).is_some_and(|age| age <= CACHE_LIFETIME)
}

fn read_cache(path: &Path) -> Result<SteamStoreDetails, SteamStoreError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn write_cache(path: &Path, details: &SteamStoreDetails) -> Result<(), SteamStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| SteamStoreError::InvalidCachePath(path.to_path_buf()))?;
    fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.new", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(details)?)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn parse_details(app_id: u32, document: &Value) -> Result<SteamStoreDetails, SteamStoreError> {
    let app = document
        .get(app_id.to_string())
        .ok_or(SteamStoreError::Unavailable(app_id))?;
    if app.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(SteamStoreError::Unavailable(app_id));
    }
    let data = app.get("data").ok_or(SteamStoreError::Unavailable(app_id))?;
    let name = string(data, "name").trim().to_owned();
    if name.is_empty() || name.len() > 200 {
        return Err(SteamStoreError::InvalidResponse("name"));
    }

    let mut platforms = Vec::new();
    if let Some(values) = data.get("platforms") {
        for (key, label) in [("windows", "Windows"), ("mac", "macOS"), ("linux", "Linux")] {
            if values.get(key).and_then(Value::as_bool) == Some(true) {
                platforms.push(label.to_owned());
            }
        }
    }

    let requirements = data.get("pc_requirements").unwrap_or(&Value::Null);
    let (drm_notice, uses_denuvo) = detect_denuvo(data);
    Ok(SteamStoreDetails {
        cache_format_version: CACHE_FORMAT_VERSION,
        stale_cache: false,
        app_id,
        name,
        short_description: clean_html(string(data, "short_description")),
        about_the_game: clean_html(string(data, "about_the_game")),
        header_image_url: string(data, "header_image").to_owned(),
        developers: string_array(data, "developers"),
        publishers: string_array(data, "publishers"),
        genres: filter_platform_tags(description_array(data, "genres")),
        platforms,
        screenshots: data
            .get("screenshots")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("path_full").or_else(|| item.get("path_thumbnail")))
            .filter_map(Value::as_str)
            .filter(|url| url.starts_with("https://"))
            .take(12)
            .map(str::to_owned)
            .collect(),
        release_date: data
            .get("release_date")
            .and_then(|value| value.get("date"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        price: price(data),
        metacritic_score: data
            .get("metacritic")
            .and_then(|value| value.get("score"))
            .and_then(Value::as_u64),
        drm_notice,
        uses_denuvo,
        reviews: SteamReviewSummary::default(),
        requirements: SteamSystemRequirements {
            minimum: clean_html(string(requirements, "minimum")),
            recommended: clean_html(string(requirements, "recommended")),
        },
        dlc: data
            .get("dlc")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|id| id as u32)
                    .collect()
            })
            .unwrap_or_default(),
        languages: parse_supported_languages(string(data, "supported_languages")),
    })
}

/// Parses Steam's `supported_languages` HTML blob (e.g. `English<strong>*</strong>, German, …`)
/// into lowercased language names for the emulator's `supported_languages.txt`. The "languages with
/// full audio support" footnote and `*` markers are dropped.
fn parse_supported_languages(raw: &str) -> Vec<String> {
    let text = clean_html(raw);
    // Drop the footnote that Steam appends after the language list.
    let list = text
        .split("languages with full audio support")
        .next()
        .unwrap_or(&text);
    let mut languages = Vec::new();
    for part in list.split(',') {
        let name = part.trim().trim_end_matches('*').trim().to_lowercase();
        if !name.is_empty() && !languages.contains(&name) {
            languages.push(name);
        }
    }
    languages
}

fn string<'a>(value: &'a Value, name: &str) -> &'a str {
    value.get(name).and_then(Value::as_str).unwrap_or_default()
}

fn string_array(value: &Value, name: &str) -> Vec<String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(30)
        .map(str::to_owned)
        .collect()
}

/// Whether a storefront item is Valve first-party hardware rather than a game (Steam Machine,
/// Steam Deck, Steam Controller, Steam Link, Valve Index), so the store shelves can drop it.
#[must_use]
fn is_valve_hardware(name: &str) -> bool {
    const HARDWARE: &[&str] = &[
        "steam machine",
        "steam deck",
        "steam controller",
        "steam link",
        "valve index",
        "steam vr",
        "index controller",
        "index headset",
    ];
    let lower = name.to_lowercase();
    HARDWARE.iter().any(|hardware| lower.contains(hardware)) || lower == "index"
}

/// Drops platform/hardware/feature entries that Steam sometimes mixes into an app's tag list
/// ("Steam Machine", "SteamOS + Linux", VR headsets, controller support, …), keeping only real
/// genres. Matching is case-insensitive: an exact blocklist entry, or any tag mentioning a piece of
/// hardware/platform we never want to surface.
#[must_use]
fn filter_platform_tags(tags: Vec<String>) -> Vec<String> {
    const BLOCKED_SUBSTRINGS: &[&str] = &[
        "steam machine",
        "steamos",
        "steam deck",
        "steamvr",
        "steam controller",
        "valve index",
        "htc vive",
        "oculus",
        "windows mixed reality",
        "vr only",
        "vr support",
        "tracked motion controller",
        "remote play",
        "cross-platform",
        "controller support",
        "captions available",
        "commentary available",
        "downloadable content",
        "level editor",
    ];
    tags.into_iter()
        .filter(|tag| {
            let lower = tag.to_lowercase();
            !BLOCKED_SUBSTRINGS.iter().any(|blocked| lower.contains(blocked))
        })
        .collect()
}

fn description_array(value: &Value, name: &str) -> Vec<String> {
    value
        .get(name)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get("description"))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .take(30)
        .map(str::to_owned)
        .collect()
}

fn price(data: &Value) -> String {
    if data.get("is_free").and_then(Value::as_bool) == Some(true) {
        return "Free to Play".into();
    }
    data.get("price_overview")
        .and_then(|value| value.get("final_formatted"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Unavailable")
        .to_owned()
}

fn clean_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut tag = String::new();
    let mut inside_tag = false;
    for character in input.chars() {
        match character {
            '<' if !inside_tag => {
                inside_tag = true;
                tag.clear();
            }
            '>' if inside_tag => {
                inside_tag = false;
                let name = tag
                    .trim()
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or_default();
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "br" | "p" | "li" | "ul" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                ) && !output.ends_with('\n')
                {
                    output.push('\n');
                }
            }
            value if inside_tag => tag.push(value),
            value => output.push(value),
        }
    }
    html_escape::decode_html_entities(&output)
        .lines()
        .map(str::trim)
        .fold(String::new(), |mut result, line| {
            if line.is_empty() {
                if !result.ends_with("\n\n") && !result.is_empty() {
                    result.push('\n');
                }
            } else {
                if !result.is_empty() && !result.ends_with('\n') {
                    result.push(' ');
                }
                result.push_str(line);
                result.push('\n');
            }
            result
        })
        .trim()
        .to_owned()
}

#[derive(Debug, Error)]
pub enum SteamStoreError {
    #[error("App ID must not be zero")]
    InvalidAppId,
    #[error("Steam Store data is unavailable for App {0}")]
    Unavailable(u32),
    #[error("Steam Store returned an invalid {0}")]
    InvalidResponse(&'static str),
    #[error("Steam Store response exceeded the safe size limit")]
    ResponseTooLarge,
    /// The shared pacing gate could not grant a request slot in time — usually a `429` cooldown.
    /// Surfaced instead of blocking the caller for the full cooldown.
    #[error("Steam is rate-limiting storefront requests — try again in about {}s", retry_after.as_secs().max(1))]
    RateLimited { retry_after: Duration },
    #[error("invalid store cache path {0}")]
    InvalidCachePath(PathBuf),
    #[error(transparent)]
    Network(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_store_details_and_cleans_html() {
        let json = serde_json::json!({
            "42": {
                "success": true,
                "data": {
                    "name": "Example Game",
                    "short_description": "Fast &amp; fun<br>Always",
                    "about_the_game": "<p>About</p><ul><li>One</li></ul>",
                    "header_image": "https://cdn.example/header.jpg",
                    "developers": ["Studio"],
                    "publishers": ["Publisher"],
                    "genres": [{"description": "Action"}],
                    "platforms": {"windows": true, "mac": false, "linux": true},
                    "screenshots": [{"path_full": "https://cdn.example/1.jpg"}],
                    "release_date": {"date": "1 Jan, 2026"},
                    "is_free": false,
                    "price_overview": {"final_formatted": "$9.99"},
                    "metacritic": {"score": 88},
                    "pc_requirements": {"minimum": "<strong>CPU:</strong> Any", "recommended": "Better"}
                }
            }
        });
        let details = parse_details(42, &json).expect("details");
        assert_eq!(details.name, "Example Game");
        assert_eq!(details.cache_format_version, CACHE_FORMAT_VERSION);
        assert_eq!(details.platforms, ["Windows", "Linux"]);
        assert_eq!(details.short_description, "Fast & fun\nAlways");
        assert_eq!(details.requirements.minimum, "CPU: Any");
    }

    #[test]
    fn parses_featured_capsules_and_skips_non_apps() {
        let json = serde_json::json!({
            "items": [
                {
                    "id": 2358720,
                    "type": 0,
                    "name": "Black Myth: Wukong",
                    "discounted": false,
                    "discount_percent": 0,
                    "original_price": 5999,
                    "final_price": 5999,
                    "currency": "USD",
                    "large_capsule_image": "https://cdn.example/wukong_616x353.jpg",
                    "header_image": "https://cdn.example/wukong_header.jpg"
                },
                {
                    "id": 1245620,
                    "type": 0,
                    "name": "ELDEN RING",
                    "discounted": true,
                    "discount_percent": 30,
                    "original_price": 5999,
                    "final_price": 4199,
                    "currency": "USD",
                    "header_image": "https://cdn.example/elden_header.jpg"
                },
                {
                    "id": 12345,
                    "type": 4,
                    "name": "Some Bundle",
                    "header_image": "https://cdn.example/bundle.jpg"
                },
                {
                    "id": 999,
                    "type": 0,
                    "name": "No Art Game",
                    "final_price": 1999,
                    "currency": "USD"
                },
                {
                    "id": 2358720,
                    "type": 0,
                    "name": "Black Myth: Wukong (duplicate)",
                    "final_price": 5999,
                    "currency": "USD",
                    "header_image": "https://cdn.example/wukong_dup.jpg"
                }
            ]
        });
        let capsules = parse_capsules(Some(&json));
        // The two valid, distinct apps — the bundle, the art-less row, and the duplicate are dropped.
        assert_eq!(capsules.len(), 2);
        assert_eq!(capsules[0].app_id, 2_358_720);
        assert_eq!(capsules[0].name, "Black Myth: Wukong");
        assert_eq!(capsules[0].price, "$59.99");
        assert_eq!(capsules[0].discount_percent, 0);
        assert_eq!(capsules[1].app_id, 1_245_620);
        assert_eq!(capsules[1].price, "$41.99");
        assert_eq!(capsules[1].discount_percent, 30);
        assert!(capsules[0].header_image_url.starts_with("https://"));
    }

    #[test]
    fn formats_prices_per_currency() {
        assert_eq!(format_price(5999, "USD"), "$59.99");
        assert_eq!(format_price(5999, "EUR"), "€59.99");
        assert_eq!(format_price(5999, "GBP"), "£59.99");
        assert_eq!(format_price(5999, "PLN"), "59.99 PLN");
        assert_eq!(format_price(5999, ""), "$59.99");
    }

    #[test]
    fn parses_and_formats_review_summary() {
        let summary = parse_review_summary(&serde_json::json!({
            "review_score_desc": "Very Positive",
            "total_positive": 920,
            "total_negative": 80,
            "total_reviews": 1000
        }));
        assert_eq!(
            summary.display_text(),
            "Very Positive (92% positive, 1,000 reviews)"
        );
        assert_eq!(SteamReviewSummary::default().display_text(), "Not available");
    }

    #[test]
    fn parses_topsellers_html_keeping_verbatim_capsule_urls() {
        // Two rows: a normal capsule, and an `_alt_assets` capsule for a coming-soon game with no
        // `header.jpg`. A `Bundle_` row and a title-less row must be dropped. The capsule URL is kept
        // exactly as served (rewriting it to `header.jpg` would 404 for the alt-assets/coming-soon
        // game), and HTML entities in the title are decoded.
        let html = r#"
          <a data-ds-itemkey="App_2075800">
            <div class="search_capsule">
              <img src="https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/2075800/abc123/capsule_231x87.jpg?t=1788256323" >
            </div>
            <span class="title">STAR WARS Zero Company&trade;</span>
          </a>
          <a data-ds-itemkey="Bundle_54321">
            <span class="title">Some Bundle</span>
          </a>
          <a data-ds-itemkey="App_4162040">
            <div class="search_capsule">
              <img src="https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/4162040/def456/capsule_231x87_alt_assets_1.jpg?t=1788905636" >
            </div>
            <span class="title">Zenless Zone Zero</span>
          </a>
          <a data-ds-itemkey="App_99999">
            <div class="search_capsule"><img src="x"></div>
          </a>
        "#;
        let capsules = parse_topsellers_html(html);
        assert_eq!(capsules.len(), 2);
        assert_eq!(capsules[0].app_id, 2_075_800);
        assert_eq!(capsules[0].name, "STAR WARS Zero Company™");
        assert!(
            capsules[0]
                .header_image_url
                .ends_with("capsule_231x87.jpg?t=1788256323")
        );
        assert_eq!(capsules[1].app_id, 4_162_040);
        assert_eq!(capsules[1].name, "Zenless Zone Zero");
        // The alt-assets capsule is preserved verbatim — no rewrite to a non-existent header.
        assert!(
            capsules[1]
                .header_image_url
                .ends_with("capsule_231x87_alt_assets_1.jpg?t=1788905636")
        );
    }

    #[test]
    fn detects_denuvo_from_drm_notice() {
        let denuvo = serde_json::json!({"drm_notice": "Denuvo Anti-Tampering"});
        assert_eq!(detect_denuvo(&denuvo), ("Denuvo Anti-Tampering".to_owned(), true));
        // Games with Denuvo removed report an empty notice, and are correctly treated as clean.
        let removed = serde_json::json!({"drm_notice": ""});
        assert_eq!(detect_denuvo(&removed), (String::new(), false));
        // A non-Denuvo third-party notice must not trip the check.
        let other = serde_json::json!({"drm_notice": "3rd-party EULA"});
        assert_eq!(detect_denuvo(&other), ("3rd-party EULA".to_owned(), false));
        // It can also appear in the external account notice.
        let ext = serde_json::json!({"ext_user_account_notice": "Denuvo Anti-tamper"});
        assert!(detect_denuvo(&ext).1);
    }

    #[test]
    fn rejects_failed_store_response() {
        let json = serde_json::json!({"42": {"success": false}});
        assert!(matches!(
            parse_details(42, &json),
            Err(SteamStoreError::Unavailable(42))
        ));
    }

    #[test]
    fn parses_supported_languages_dropping_markers_and_footnote() {
        let raw = "English<strong>*</strong>, French, German<br><strong>*</strong>languages \
                   with full audio support";
        assert_eq!(
            parse_supported_languages(raw),
            vec!["english".to_owned(), "french".to_owned(), "german".to_owned()]
        );
    }

    #[test]
    fn is_valve_hardware_flags_hardware_not_games() {
        assert!(is_valve_hardware("Steam Machine"));
        assert!(is_valve_hardware("Steam Deck OLED"));
        assert!(is_valve_hardware("Valve Index"));
        assert!(is_valve_hardware("Index"));
        assert!(!is_valve_hardware("Bodycam"));
        assert!(!is_valve_hardware("Team Fortress 2"));
    }

    #[test]
    fn filter_platform_tags_drops_hardware_keeps_genres() {
        let tags = vec![
            "Action".to_owned(),
            "Steam Machine".to_owned(),
            "SteamOS + Linux".to_owned(),
            "Massively Multiplayer".to_owned(),
            "VR Supported".to_owned(),
            "Early Access".to_owned(),
        ];
        assert_eq!(
            filter_platform_tags(tags),
            vec![
                "Action".to_owned(),
                "Massively Multiplayer".to_owned(),
                "Early Access".to_owned(),
            ]
        );
    }
}
