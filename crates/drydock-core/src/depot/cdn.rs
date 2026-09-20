//! Steam content-CDN client: resolves the list of content servers and downloads + decrypts +
//! decompresses individual depot chunks.
//!
//! The server list comes from the **public** `IContentServerDirectoryService/GetServersForSteamPipe`
//! Web API (no auth), so — for anonymously accessible depots whose key we already hold — no Steam
//! session is required. A chunk is fetched from `<host>/depot/<depot>/chunk/<sha>`, then
//! [`crypto::symmetric_decrypt`] + [`crypto::decompress`] + Adler-32 recover the raw bytes.
//!
//! How fast those servers answer differs a lot, and from moment to moment: a chunk a server has
//! not cached yet takes seconds instead of milliseconds. [`ServerPool`] therefore picks the server
//! for each request by how fast it has been so far, the way the Steam client scores its servers.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use thiserror::Error;

use super::crypto::{self, ChunkError};
use super::manifest::ChunkEntry;
use crate::version::user_agent;

const SERVER_DIRECTORY_URL: &str =
    "https://api.steampowered.com/IContentServerDirectoryService/GetServersForSteamPipe/v1/";
/// A single encrypted chunk is at most 1 MiB compressed; cap the read generously.
const MAXIMUM_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// How often a pick ignores the scores and takes a random server, so one that answered slowly once
/// (typically for a chunk it first had to fetch itself) gets measured again.
const EXPLORE_EVERY: u64 = 16;
/// How long a server that failed is left alone while others are available.
const FAILURE_REST: Duration = Duration::from_secs(30);
/// Weight of the newest measurement in a server's running score.
const SCORE_WEIGHT: f64 = 0.3;
/// A small chunk still costs a whole round trip. Scoring it as at least this many bytes keeps a
/// server that happened to serve small chunks from looking slow.
const MINIMUM_SCORED_BYTES: f64 = 256.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;

#[derive(Debug, Error)]
pub enum CdnError {
    #[error("no content servers were returned by Steam")]
    NoServers,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("network error while reading a chunk: {0}")]
    Body(#[from] std::io::Error),
    #[error("content server returned status {0}")]
    Status(u16),
    #[error("chunk exceeded the size limit")]
    TooLarge,
    #[error("the download was stopped")]
    Cancelled,
    #[error(transparent)]
    Chunk(#[from] ChunkError),
    #[error("decompressed chunk length {actual} did not match the manifest's {expected}")]
    LengthMismatch { expected: u32, actual: usize },
}

/// A resolved content server (host only; scheme chosen by `https`).
#[derive(Clone, Debug)]
pub struct ContentServer {
    pub host: String,
    pub https: bool,
}

impl ContentServer {
    fn chunk_url(&self, depot_id: u32, chunk_id_hex: &str) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://{}/depot/{depot_id}/chunk/{chunk_id_hex}", self.host)
    }
}

/// A blocking CDN client with a pooled connection.
pub struct CdnClient {
    client: Client,
}

impl CdnClient {
    pub fn new() -> Result<Self, CdnError> {
        let client = Client::builder()
            .user_agent(user_agent())
            .connect_timeout(Duration::from_secs(10))
            // One chunk is at most a megabyte. A server that needs longer than this for it is a
            // server to drop rather than to wait for: another one gets the chunk while this one is
            // still thinking, and a pause does not hang on it either.
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { client })
    }

    /// Fetches the public content-server directory for a cell and returns the hosts usable for
    /// `app_id`, least loaded first.
    ///
    /// Like the Steam client and DepotDownloader, only servers Steam currently offers to clients
    /// (`num_entries_in_client_list` above zero) are used; the others are the ones it is steering
    /// traffic away from. Only when it offers none does every listed server count.
    pub fn content_servers(&self, cell_id: u32, app_id: u32) -> Result<Vec<ContentServer>, CdnError> {
        let response = self
            .client
            .get(SERVER_DIRECTORY_URL)
            .query(&[("cell_id", cell_id.to_string())])
            .send()?
            .error_for_status()?;
        let document: ServerDirectoryResponse = response.json()?;
        let servers = usable_servers(document.response.servers, app_id);
        if servers.is_empty() {
            return Err(CdnError::NoServers);
        }
        Ok(servers)
    }

    /// Diagnostic: downloads and decrypts a chunk but returns the still-**compressed** bytes, so a
    /// caller can inspect the compression container header (`VZ`, `PK\x03\x04`, …).
    pub fn fetch_decrypted(
        &self,
        server: &ContentServer,
        depot_id: u32,
        chunk: &ChunkEntry,
        depot_key: &[u8; 32],
    ) -> Result<Vec<u8>, CdnError> {
        let encrypted = self.fetch_chunk(server, depot_id, chunk)?;
        Ok(crypto::symmetric_decrypt(&encrypted, depot_key)?)
    }

    /// Downloads one chunk's encrypted bytes from `server`, as they are stored on the CDN.
    pub fn fetch_chunk(
        &self,
        server: &ContentServer,
        depot_id: u32,
        chunk: &ChunkEntry,
    ) -> Result<Vec<u8>, CdnError> {
        self.fetch_chunk_cancellable(server, depot_id, chunk, &AtomicBool::new(false))
    }

    /// [`Self::fetch_chunk`] that drops the transfer as soon as `cancel` is set.
    ///
    /// A pause is only as fast as the slowest request still in flight: with dozens of connections,
    /// waiting for each one to finish its chunk — or worse, to run into the request timeout on a
    /// server that stopped sending — is what makes "pausing" take far longer than it should.
    pub fn fetch_chunk_cancellable(
        &self,
        server: &ContentServer,
        depot_id: u32,
        chunk: &ChunkEntry,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, CdnError> {
        let url = server.chunk_url(depot_id, &chunk.id_hex());
        let response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            return Err(CdnError::Status(response.status().as_u16()));
        }
        let expected = response.content_length();
        if expected.is_some_and(|len| len > MAXIMUM_CHUNK_BYTES) {
            return Err(CdnError::TooLarge);
        }
        let capacity = expected.unwrap_or_else(|| u64::from(chunk.compressed_len));
        let mut encrypted = Vec::with_capacity(capacity.min(MAXIMUM_CHUNK_BYTES) as usize);
        let mut reader = response.take(MAXIMUM_CHUNK_BYTES + 1);
        let mut buffer = [0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(CdnError::Cancelled);
            }
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => encrypted.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.into()),
            }
        }
        if encrypted.len() as u64 > MAXIMUM_CHUNK_BYTES {
            return Err(CdnError::TooLarge);
        }
        Ok(encrypted)
    }

    /// Downloads one chunk from `server` and returns its raw file bytes (see [`process_chunk`]).
    pub fn download_chunk(
        &self,
        server: &ContentServer,
        depot_id: u32,
        chunk: &ChunkEntry,
        depot_key: &[u8; 32],
    ) -> Result<Vec<u8>, CdnError> {
        process_chunk(&self.fetch_chunk(server, depot_id, chunk)?, chunk, depot_key)
    }
}

/// Decrypts a downloaded chunk with the depot key, decompresses it, and verifies its length and
/// Adler-32 against the manifest. Returns the raw file bytes.
pub fn process_chunk(
    encrypted: &[u8],
    chunk: &ChunkEntry,
    depot_key: &[u8; 32],
) -> Result<Vec<u8>, CdnError> {
    let decrypted = crypto::symmetric_decrypt(encrypted, depot_key)?;
    let raw = crypto::decompress(&decrypted)?;
    if raw.len() != chunk.uncompressed_len as usize {
        return Err(CdnError::LengthMismatch {
            expected: chunk.uncompressed_len,
            actual: raw.len(),
        });
    }
    let actual = crypto::steam_adler_hash(&raw);
    if actual != chunk.crc {
        return Err(CdnError::Chunk(ChunkError::Checksum {
            expected: chunk.crc,
            actual,
        }));
    }
    Ok(raw)
}

/// Picks a content server for each request, favouring the ones that have been fast.
///
/// Every pick compares two random servers and takes the one with the better running score ("power
/// of two choices"), which spreads the load while steering clear of slow servers. A server not yet
/// measured wins such a comparison, so each gets tried early on, and one pick in
/// [`EXPLORE_EVERY`] ignores the scores so a server that was slow once can recover. A server that
/// failed rests for [`FAILURE_REST`] unless no other is left.
pub struct ServerPool {
    servers: Vec<ContentServer>,
    scores: Mutex<Vec<ServerScore>>,
    picks: AtomicU64,
    seed: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ServerScore {
    /// Running average of seconds per MiB received; 0 until the server has answered once.
    seconds_per_mib: f64,
    resting_until: Option<Instant>,
}

impl ServerPool {
    pub fn new(servers: Vec<ContentServer>) -> Result<Self, CdnError> {
        if servers.is_empty() {
            return Err(CdnError::NoServers);
        }
        Ok(Self {
            scores: Mutex::new(vec![ServerScore::default(); servers.len()]),
            servers,
            picks: AtomicU64::new(0),
            seed: RandomState::new().build_hasher().finish(),
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    #[must_use]
    pub fn server(&self, index: usize) -> &ContentServer {
        &self.servers[index]
    }

    /// The index of the server to use next. `avoid` is a server that just failed this request; it
    /// is only picked again when it is the only one.
    #[must_use]
    pub fn pick(&self, avoid: Option<usize>) -> usize {
        let count = self.servers.len();
        let pick = self.picks.fetch_add(1, Ordering::Relaxed);
        let random = mix(self.seed ^ pick);
        let now = Instant::now();
        let scores = self.scores.lock().unwrap_or_else(PoisonError::into_inner);
        let allowed = |index: &usize| count == 1 || Some(*index) != avoid;
        let mut candidates: Vec<usize> = (0..count)
            .filter(allowed)
            .filter(|index| scores[*index].resting_until.is_none_or(|until| until <= now))
            .collect();
        if candidates.is_empty() {
            candidates = (0..count).filter(allowed).collect();
        }
        let length = candidates.len() as u64;
        let first = candidates[(random % length) as usize];
        if pick % EXPLORE_EVERY == EXPLORE_EVERY - 1 {
            return first;
        }
        let second = candidates[((random >> 32) % length) as usize];
        if scores[second].seconds_per_mib < scores[first].seconds_per_mib {
            second
        } else {
            first
        }
    }

    /// Records that `index` delivered `bytes` in `elapsed`.
    pub fn record(&self, index: usize, bytes: usize, elapsed: Duration) {
        let sample = elapsed.as_secs_f64() / (bytes as f64).max(MINIMUM_SCORED_BYTES) * MIB;
        let mut scores = self.scores.lock().unwrap_or_else(PoisonError::into_inner);
        let score = &mut scores[index];
        score.seconds_per_mib = if score.seconds_per_mib == 0.0 {
            sample
        } else {
            score.seconds_per_mib * (1.0 - SCORE_WEIGHT) + sample * SCORE_WEIGHT
        };
        score.resting_until = None;
    }

    /// Records that `index` failed a request or delivered a chunk that did not check out.
    pub fn record_failure(&self, index: usize) {
        let mut scores = self.scores.lock().unwrap_or_else(PoisonError::into_inner);
        let score = &mut scores[index];
        score.seconds_per_mib = (score.seconds_per_mib * 4.0).max(10.0);
        score.resting_until = Some(Instant::now() + FAILURE_REST);
    }
}

/// SplitMix64's finaliser: turns a counter into well-spread pseudo-random bits.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// The directory entries a client may download `app_id` from, least loaded first.
fn usable_servers(entries: Vec<ServerEntry>, app_id: u32) -> Vec<ContentServer> {
    let mut eligible: Vec<ServerEntry> = entries
        .into_iter()
        .filter(|server| {
            matches!(
                server.server_type.as_deref(),
                Some("SteamCache") | Some("CDN") | None
            )
        })
        .filter(|server| server.allowed_app_ids.is_empty() || server.allowed_app_ids.contains(&app_id))
        .filter(|server| server.host.as_deref().is_some_and(|host| !host.is_empty()))
        .collect();
    if eligible.iter().any(|server| server.offered_to_clients()) {
        eligible.retain(ServerEntry::offered_to_clients);
    }
    eligible.sort_by(|a, b| a.weighted_load.total_cmp(&b.weighted_load));
    eligible
        .into_iter()
        .filter_map(|server| {
            Some(ContentServer {
                https: server.https_support.as_deref() != Some("none"),
                host: server.host?,
            })
        })
        .collect()
}

// --- content-server directory JSON ---

#[derive(serde::Deserialize)]
struct ServerDirectoryResponse {
    response: ServerList,
}

#[derive(serde::Deserialize)]
struct ServerList {
    #[serde(default)]
    servers: Vec<ServerEntry>,
}

#[derive(serde::Deserialize)]
struct ServerEntry {
    #[serde(rename = "type")]
    server_type: Option<String>,
    host: Option<String>,
    https_support: Option<String>,
    #[serde(default)]
    weighted_load: f64,
    /// How many slots Steam gives the server in a client's list; 0 while it is shedding load.
    num_entries_in_client_list: Option<u32>,
    #[serde(default)]
    allowed_app_ids: Vec<u32>,
}

impl ServerEntry {
    fn offered_to_clients(&self) -> bool {
        self.num_entries_in_client_list.is_none_or(|entries| entries > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_chunk_url() {
        let server = ContentServer {
            host: "cache1.example.net".into(),
            https: true,
        };
        assert_eq!(
            server.chunk_url(228_990, "abcdef"),
            "https://cache1.example.net/depot/228990/chunk/abcdef"
        );
    }

    #[test]
    fn parses_server_directory_json() {
        let json = serde_json::json!({
            "response": {
                "servers": [
                    { "type": "SteamCache", "host": "c1.example.net", "https_support": "optional" },
                    { "type": "CDN", "host": "c2.example.net", "https_support": "none" },
                    { "type": "OpenCache", "host": "ignored.example.net" }
                ]
            }
        });
        let parsed: ServerDirectoryResponse = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.response.servers.len(), 3);
        let hosts: Vec<String> = usable_servers(parsed.response.servers, 1)
            .into_iter()
            .map(|server| server.host)
            .collect();
        assert_eq!(hosts, ["c1.example.net", "c2.example.net"]);
    }

    #[test]
    fn servers_steam_is_draining_or_reserving_for_other_apps_are_left_out() {
        let json = serde_json::json!({
            "response": {
                "servers": [
                    { "type": "SteamCache", "host": "full.example.net", "weighted_load": 100, "num_entries_in_client_list": 0 },
                    { "type": "SteamCache", "host": "busy.example.net", "weighted_load": 95, "num_entries_in_client_list": 1 },
                    { "type": "SteamCache", "host": "quiet.example.net", "weighted_load": 80, "num_entries_in_client_list": 1 },
                    { "type": "CDN", "host": "partner.example.net", "weighted_load": 10, "num_entries_in_client_list": 1, "allowed_app_ids": [730] }
                ]
            }
        });
        let parsed: ServerDirectoryResponse = serde_json::from_value(json).unwrap();
        let hosts: Vec<String> = usable_servers(parsed.response.servers, 440)
            .into_iter()
            .map(|server| server.host)
            .collect();
        assert_eq!(hosts, ["quiet.example.net", "busy.example.net"]);

        // When Steam offers none of them, a download can still use what is listed.
        let json = serde_json::json!({
            "response": { "servers": [
                { "type": "SteamCache", "host": "full.example.net", "num_entries_in_client_list": 0 }
            ] }
        });
        let parsed: ServerDirectoryResponse = serde_json::from_value(json).unwrap();
        assert_eq!(usable_servers(parsed.response.servers, 440).len(), 1);
    }

    fn pool(count: usize) -> ServerPool {
        ServerPool::new(
            (0..count)
                .map(|index| ContentServer {
                    host: format!("c{index}.example.net"),
                    https: true,
                })
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn the_pool_settles_on_the_faster_servers() {
        let pool = pool(4);
        let megabyte = 1024 * 1024;
        pool.record(0, megabyte, Duration::from_millis(2000));
        pool.record(1, megabyte, Duration::from_millis(50));
        pool.record(2, megabyte, Duration::from_millis(1500));
        pool.record(3, megabyte, Duration::from_millis(60));
        let mut hits = [0usize; 4];
        for _ in 0..4000 {
            hits[pool.pick(None)] += 1;
        }
        assert!(
            hits[1] + hits[3] > 2 * (hits[0] + hits[2]),
            "fast servers should get most requests: {hits:?}"
        );
        assert!(
            hits[0] > 0 && hits[2] > 0,
            "slow servers are still re-measured: {hits:?}"
        );
    }

    #[test]
    fn a_failed_server_rests_and_is_not_retried_right_away() {
        let pool = pool(3);
        pool.record_failure(0);
        for _ in 0..500 {
            assert_ne!(pool.pick(None), 0);
            assert_ne!(pool.pick(Some(1)), 1);
        }
        // With nothing else left, even a resting or just-failed server is used.
        let single = self::pool(1);
        single.record_failure(0);
        assert_eq!(single.pick(Some(0)), 0);
    }

    #[test]
    fn an_empty_server_list_is_an_error() {
        assert!(matches!(ServerPool::new(Vec::new()), Err(CdnError::NoServers)));
    }
}
