//! Steam content-CDN client: resolves the list of content servers and downloads + decrypts +
//! decompresses individual depot chunks.
//!
//! The server list comes from the **public** `IContentServerDirectoryService/GetServersForSteamPipe`
//! Web API (no auth), so — for anonymously accessible depots whose key we already hold — no Steam
//! session is required. A chunk is fetched from `<host>/depot/<depot>/chunk/<sha>`, then
//! [`crypto::symmetric_decrypt`] + [`crypto::decompress`] + Adler-32 recover the raw bytes.

use std::time::Duration;

use reqwest::blocking::Client;
use thiserror::Error;

use super::crypto::{self, ChunkError};
use super::manifest::ChunkEntry;
use crate::version::user_agent;

const SERVER_DIRECTORY_URL: &str =
    "https://api.steampowered.com/IContentServerDirectoryService/GetServersForSteamPipe/v1/";
/// A single encrypted chunk is at most 1 MiB compressed; cap the read generously.
const MAXIMUM_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CdnError {
    #[error("no content servers were returned by Steam")]
    NoServers,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("content server returned status {0}")]
    Status(u16),
    #[error("chunk exceeded the size limit")]
    TooLarge,
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
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { client })
    }

    /// Fetches the public content-server directory for a cell and returns usable CDN hosts.
    pub fn content_servers(&self, cell_id: u32) -> Result<Vec<ContentServer>, CdnError> {
        let response = self
            .client
            .get(SERVER_DIRECTORY_URL)
            .query(&[("cell_id", cell_id.to_string())])
            .send()?
            .error_for_status()?;
        let document: ServerDirectoryResponse = response.json()?;
        let servers: Vec<ContentServer> = document
            .response
            .servers
            .into_iter()
            .filter(|server| {
                matches!(
                    server.server_type.as_deref(),
                    Some("SteamCache") | Some("CDN") | None
                )
            })
            .filter_map(|server| {
                let host = server.host.filter(|host| !host.is_empty())?;
                Some(ContentServer {
                    https: server.https_support.as_deref() != Some("none"),
                    host,
                })
            })
            .collect();
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
        let url = server.chunk_url(depot_id, &chunk.id_hex());
        let response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            return Err(CdnError::Status(response.status().as_u16()));
        }
        let encrypted = response.bytes()?;
        Ok(crypto::symmetric_decrypt(&encrypted, depot_key)?)
    }

    /// Downloads one chunk from `server`, decrypts it with the depot key, decompresses it, and
    /// verifies its Adler-32 and length against the manifest. Returns the raw file bytes.
    pub fn download_chunk(
        &self,
        server: &ContentServer,
        depot_id: u32,
        chunk: &ChunkEntry,
        depot_key: &[u8; 32],
    ) -> Result<Vec<u8>, CdnError> {
        let url = server.chunk_url(depot_id, &chunk.id_hex());
        let response = self.client.get(&url).send()?;
        if !response.status().is_success() {
            return Err(CdnError::Status(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAXIMUM_CHUNK_BYTES)
        {
            return Err(CdnError::TooLarge);
        }
        let encrypted = response.bytes()?;
        if encrypted.len() as u64 > MAXIMUM_CHUNK_BYTES {
            return Err(CdnError::TooLarge);
        }

        let decrypted = crypto::symmetric_decrypt(&encrypted, depot_key)?;
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
    }
}
