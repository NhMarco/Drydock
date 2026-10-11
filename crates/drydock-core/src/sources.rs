//! Where an app's unlock and depot package come from, when the user picks it: one of the proxy's
//! providers, or Hubcap Manifest with their own key. Without a pick (`None`) the proxy chooses, in the
//! order it is configured with.

use std::sync::atomic::AtomicBool;

use serde::Deserialize;

use crate::depot::{DepotData, DepotDownloadError};
use crate::hubcap::{HubcapClient, HubcapError};
use crate::lua_clean::clean_lua;
use crate::proxy::{ProxyClient, ProxyError};

/// A provider the user can pick.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DepotSource {
    DepotBox,
    SteamTools,
    Ryu,
    /// Hubcap Manifest, with the user's own key.
    Hubcap,
}

impl DepotSource {
    pub const ALL: [Self; 4] = [Self::DepotBox, Self::SteamTools, Self::Ryu, Self::Hubcap];

    /// The name used on the wire and in the settings.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::DepotBox => "depotbox",
            Self::SteamTools => "steamtools",
            Self::Ryu => "ryu",
            Self::Hubcap => "hubcap",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::DepotBox => "DEPOTBOX",
            Self::SteamTools => "STEAMTOOLS",
            Self::Ryu => "RYU",
            Self::Hubcap => "HUBCAPMANIFEST",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.key() == key)
    }
}

/// A date for the source picker, `05 Oct 2026`, from Unix seconds (UTC).
#[must_use]
pub fn day_label(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok())
        .map_or_else(String::new, |time| {
            time.to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%d %b %Y")
                .to_string()
        })
}

/// What the proxy says about one of its providers for an app (`GET /v1/sources/:appid`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct ProxySource {
    pub name: String,
    /// Whether the provider lists the app; `None` when its list cannot tell.
    pub listed: Option<bool>,
    /// When it last updated the app's unlock, in Unix seconds, where its list says so.
    pub updated: Option<u64>,
}

fn from_hubcap(error: HubcapError) -> DepotDownloadError {
    match error {
        HubcapError::Cancelled => DepotDownloadError::Cancelled,
        other => DepotDownloadError::Source(other.to_string()),
    }
}

/// The app's depot package from `source` (the proxy's choice when `None`), parsed and ready to
/// download, verify or add to Steam. Hubcap's Lua is cleaned here, as the proxy cleans the others'.
pub fn depot_data(
    proxy: &ProxyClient,
    hubcap: Option<&HubcapClient>,
    app_id: u32,
    source: Option<DepotSource>,
    cancel: &AtomicBool,
) -> Result<DepotData, DepotDownloadError> {
    match source {
        Some(DepotSource::Hubcap) => {
            let hubcap = hubcap.ok_or_else(|| {
                DepotDownloadError::Source("add your Hubcap Manifest API key in Settings first".to_owned())
            })?;
            let package = hubcap.package(app_id, cancel).map_err(from_hubcap)?;
            let mut data = DepotData::parse_package(app_id, package)?;
            if let Some((_, lua)) = data.lua.as_mut() {
                *lua = clean_lua(&String::from_utf8_lossy(lua), Some(app_id)).into_bytes();
            }
            Ok(data)
        }
        other => {
            let package = proxy
                .depot_package_from(app_id, other.map(DepotSource::key), cancel)
                .map_err(|error| match error {
                    ProxyError::Cancelled => DepotDownloadError::Cancelled,
                    other => DepotDownloadError::Proxy(other),
                })?;
            DepotData::parse_package(app_id, package)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_round_trips_its_key() {
        for source in DepotSource::ALL {
            assert_eq!(DepotSource::from_key(source.key()), Some(source));
        }
        assert_eq!(DepotSource::from_key(""), None);
        assert_eq!(DepotSource::from_key("automatic"), None);
    }

    #[test]
    fn dates_read_as_days() {
        assert_eq!(day_label(1_791_486_413), "08 Oct 2026");
    }
}
