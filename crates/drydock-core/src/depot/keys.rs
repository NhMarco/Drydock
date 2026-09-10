//! Parsing of the per-app depot **key file** the proxy relays (`/v1/depot/keys/:appid`).
//!
//! The file is plain text, one depot per line, formatted `"<depotId>;<hexKey>"` where the key is a
//! 32-byte (64 hex character) AES-256 depot key. Blank and malformed lines are skipped, matching
//! OpenSteamLoader's `LoadLocalDepotKey`.

use std::collections::HashMap;

/// A map of depot id → 32-byte AES depot key parsed from a `.key` file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DepotKeys(pub HashMap<u32, [u8; 32]>);

impl DepotKeys {
    /// Parses a depot key file's text. Unparseable lines are ignored rather than failing the whole
    /// file, since a single stray line should not block a valid download.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut keys = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((id_part, hex_part)) = line.split_once(';') else {
                continue;
            };
            let Ok(depot_id) = id_part.trim().parse::<u32>() else {
                continue;
            };
            if let Some(key) = parse_hex_key(hex_part.trim()) {
                keys.insert(depot_id, key);
            }
        }
        Self(keys)
    }

    /// Parses depot keys out of a SteamTools/DepotBox `.lua` unlock file, whose lines look like
    /// `addappid(<depotId>, 1, "<64-hex-key>")`. Entries without a key (`addappid(<id>)`) are
    /// skipped. This is the key source when the depot package ships a `.lua` instead of a `.key`.
    #[must_use]
    pub fn parse_lua(text: &str) -> Self {
        let mut keys = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("addappid(") else {
                continue;
            };
            // Take the argument list up to the closing paren.
            let Some(args) = rest.split(')').next() else {
                continue;
            };
            let mut parts = args.split(',');
            let Some(depot_id) = parts.next().and_then(|value| value.trim().parse::<u32>().ok()) else {
                continue;
            };
            // The key is the third argument, a quoted 64-hex string; absent for no-key entries.
            let Some(hex) = parts.nth(1).map(|value| value.trim().trim_matches('"')) else {
                continue;
            };
            if let Some(key) = parse_hex_key(hex) {
                keys.insert(depot_id, key);
            }
        }
        Self(keys)
    }

    /// Merges keys from `other` into `self`, keeping existing entries on conflict.
    pub fn merge_from(&mut self, other: Self) {
        for (depot_id, key) in other.0 {
            self.0.entry(depot_id).or_insert(key);
        }
    }

    #[must_use]
    pub fn get(&self, depot_id: u32) -> Option<&[u8; 32]> {
        self.0.get(&depot_id)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Decodes exactly 64 hex characters into a 32-byte key, or `None` for any other length/content.
fn parse_hex_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let hi = (bytes[index * 2] as char).to_digit(16)?;
        let lo = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_lines_and_skips_junk() {
        let text = "\
228987;0011223344556677889900aabbccddeeff00112233445566778899aabbccddee

# a comment line without a semicolon
987654;short
228990 ; ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
notanumber;0011223344556677889900aabbccddeeff00112233445566778899aabbccddee
";
        let keys = DepotKeys::parse(text);
        assert_eq!(keys.0.len(), 2);
        assert!(keys.get(228_987).is_some());
        assert_eq!(keys.get(228_990), Some(&[0xffu8; 32]));
        assert!(keys.get(987_654).is_none());
    }

    #[test]
    fn empty_file_yields_no_keys() {
        assert!(DepotKeys::parse("").is_empty());
    }

    #[test]
    fn parses_lua_addappid_keys() {
        let lua = r#"
-- Downloaded using DepotBox
addappid(70, 1, "5a108b5192144017c9614e6ee6dd925d5ef6bd8372bccb5b68f0f1814832dd1b") --Mainappid
addappid(1, 1, "b465d45ab2a7c396f7d1c08a6644e68529ec86b14da77e18588abbbcd2412060")
setManifestid(1, "2667117727132184734", 264641152)
addappid(323130) --Dlcname with no key
"#;
        let keys = DepotKeys::parse_lua(lua);
        assert_eq!(keys.0.len(), 2);
        assert!(keys.get(70).is_some());
        assert_eq!(
            keys.get(1),
            Some(&[
                0xb4, 0x65, 0xd4, 0x5a, 0xb2, 0xa7, 0xc3, 0x96, 0xf7, 0xd1, 0xc0, 0x8a, 0x66, 0x44, 0xe6,
                0x85, 0x29, 0xec, 0x86, 0xb1, 0x4d, 0xa7, 0x7e, 0x18, 0x58, 0x8a, 0xbb, 0xbc, 0xd2, 0x41,
                0x20, 0x60
            ])
        );
        assert!(keys.get(323_130).is_none());
    }
}
