//! Phase-0 feasibility spike: given a real depot package ZIP (from `/v1/depot/package/<appid>`),
//! parse a manifest + the lua keys, then download, decrypt, decompress and Adler-verify ONE real
//! chunk from the Steam CDN — anonymously. Answers whether the pure-HTTP path works or a Steam
//! session token is needed.
//!
//! Run: `cargo run -p drydock-core --example depot_spike -- path/to/depot_70.zip`

use std::io::Read;

use drydock_core::depot::cdn::CdnClient;
use drydock_core::depot::keys::DepotKeys;
use drydock_core::depot::manifest::DepotManifest;

fn main() {
    let path = std::env::args().nth(1).expect("usage: depot_spike <package.zip>");
    let bytes = std::fs::read(&path).expect("read zip");
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("open zip");

    let mut keys = DepotKeys::default();
    let mut manifests: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).unwrap();
        let name = entry.name().to_ascii_lowercase();
        let mut buffer = Vec::new();
        entry.read_to_end(&mut buffer).unwrap();
        if name.ends_with(".lua") {
            keys.merge_from(DepotKeys::parse_lua(&String::from_utf8_lossy(&buffer)));
        } else if name.ends_with(".key") {
            keys.merge_from(DepotKeys::parse(&String::from_utf8_lossy(&buffer)));
        } else if name.ends_with(".manifest") {
            manifests.push((name, buffer));
        }
    }
    println!(
        "parsed {} manifests, {} depot keys",
        manifests.len(),
        keys.0.len()
    );

    let cdn = CdnClient::new().expect("cdn client");
    let servers = cdn.content_servers(0).expect("content servers");
    println!(
        "resolved {} CDN servers (first: {})",
        servers.len(),
        servers[0].host
    );

    // Pick the manifest with the most chunks (the real game depot) and sample many chunks to find
    // any whose compression header isn't VZip — the ones that hit the failing decompress path.
    let target = manifests
        .iter()
        .filter_map(|(_, raw)| DepotManifest::parse(raw).ok())
        .filter(|m| keys.get(m.depot_id).is_some())
        .max_by_key(|m| m.files.iter().map(|f| f.chunks.len()).sum::<usize>());
    if let Some(mut manifest) = target {
        let key = keys.get(manifest.depot_id).copied().unwrap();
        let _ = manifest.decrypt_filenames(&key);
        let all: Vec<(&str, &_)> = manifest
            .files
            .iter()
            .filter(|f| !f.is_directory())
            .flat_map(|f| f.chunks.iter().map(move |c| (f.path.as_str(), c)))
            .collect();
        println!(
            "sampling depot {} ({} chunks total)",
            manifest.depot_id,
            all.len()
        );
        let step = (all.len() / 30).max(1);
        let mut failures = 0;
        let mut tested = 0;
        for (path, chunk) in all.iter().step_by(step).take(30) {
            tested += 1;
            if let Err(error) = cdn.download_chunk(&servers[0], manifest.depot_id, chunk, &key) {
                failures += 1;
                println!("  ❌ chunk {} of '{}' -> {error}", chunk.id_hex(), path);
            }
        }
        if failures == 0 {
            println!("\nRESULT A: all {tested} sampled chunks decompressed + verified OK.");
        } else {
            println!("\n{failures}/{tested} sampled chunks FAILED.");
        }
        return;
    }
    println!("no manifest had a matching depot key to test");
}
