//! Native depot download — Drydock' own DepotDownloader-equivalent.
//!
//! Given a per-app **depot key** and **manifest(s)** relayed by the proxy (`/v1/depot/*`, sourced
//! from an OpenSteamLoader-style API), this module downloads the real game files directly from the
//! Steam content CDN, decrypts and decompresses each chunk, and writes them into the Steam library.
//! Anonymous CDN access only (no Steam account), which is why the key must be supplied out of band.
//!
//! Submodules: [`crypto`] (chunk AES + VZip/deflate + Adler-32), [`keys`] (`.key` parsing),
//! [`manifest`] (protobuf manifest parsing), [`cdn`] (content-server directory + chunk fetch),
//! [`download`] (fetch/parse orchestration, download + verify with progress).

pub mod cdn;
pub mod crypto;
pub mod download;
pub mod keys;
pub mod manifest;

pub use cdn::{CdnClient, CdnError, ContentServer};
pub use download::{
    DepotData, DepotDownloadError, DownloadOutcome, DownloadProgress, DownloadStage, VerifyOutcome,
};
pub use keys::DepotKeys;
pub use manifest::{ChunkEntry, DepotManifest, FileEntry};
