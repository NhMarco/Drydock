//! Presentation-only Steam artwork choices. Keep the current asset's hash and query intact when
//! requesting its optional double-resolution sibling; the image loader handles missing variants.

/// Steam supplies `_2x` variants alongside its standard header and library artwork. Return no
/// candidate for unknown files or an already-large variant, instead of guessing an unrelated URL.
pub fn high_resolution_url(uri: &str) -> Option<String> {
    if !uri.starts_with("https://") {
        return None;
    }
    let suffix_at = uri.find(['?', '#']).unwrap_or(uri.len());
    let (path, suffix) = uri.split_at(suffix_at);
    let (base, filename) = path.rsplit_once('/')?;
    let (stem, extension) = filename.rsplit_once('.')?;
    if !matches!(extension, "jpg" | "png") || stem.ends_with("_2x") {
        return None;
    }
    if ![
        "header",
        "library_hero",
        "library_600x900",
        "capsule_231x87",
        "capsule_616x353",
    ]
    .iter()
    .any(|name| stem == *name || stem.starts_with(&format!("{name}_alt_assets_")))
    {
        return None;
    }
    Some(format!("{base}/{stem}_2x.{extension}{suffix}"))
}

/// Prefer a sharper sibling, retaining the supplied URL as a fallback for older games.
pub fn push_variants(urls: &mut Vec<String>, uri: &str) {
    if let Some(large) = high_resolution_url(uri)
        && !urls.contains(&large)
    {
        urls.push(large);
    }
    if !uri.is_empty() && !urls.iter().any(|url| url == uri) {
        urls.push(uri.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_hashed_assets_keep_their_version_and_fragment() {
        let uri = "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/4078430/hash/header.jpg?t=123#art";
        assert_eq!(
            high_resolution_url(uri).as_deref(),
            Some(
                "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps/4078430/hash/header_2x.jpg?t=123#art"
            )
        );
        assert_eq!(
            high_resolution_url("https://cdn.example/capsule_231x87_alt_assets_7.jpg").as_deref(),
            Some("https://cdn.example/capsule_231x87_alt_assets_7_2x.jpg")
        );
    }

    #[test]
    fn unrecognised_and_already_large_sources_stay_unchanged() {
        for uri in [
            "file://header.jpg",
            "http://cdn.example/header.jpg",
            "https://cdn.example/header_2x.jpg",
            "https://cdn.example/ss_123.jpg",
            "https://cdn.example/header.webp",
        ] {
            assert!(high_resolution_url(uri).is_none(), "{uri}");
        }
        let mut urls = Vec::new();
        push_variants(&mut urls, "https://cdn.example/header.jpg?t=1");
        push_variants(&mut urls, "https://cdn.example/header.jpg?t=1");
        assert_eq!(urls.len(), 2);
        assert!(urls[0].contains("header_2x.jpg"));
        assert!(urls[1].contains("header.jpg"));
    }
}
