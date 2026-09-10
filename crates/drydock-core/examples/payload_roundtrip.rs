//! Exercises the local payload store against a real app: fetch the depot package, save the Lua and
//! manifests, read them back, and report what a later install would restore.
//!
//! Run: `cargo run -p drydock-core --example payload_roundtrip -- <appid>`

use drydock_core::app_payloads::AppPayloadStore;
use drydock_core::depot::DepotData;
use drydock_core::paths::PortablePaths;
use drydock_core::proxy::ProxyClient;

fn main() {
    let app_id: u32 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .expect("usage: payload_roundtrip <appid>");

    let paths = PortablePaths::discover().expect("data directory");
    let store = AppPayloadStore::new(&paths.settings_dir());
    let proxy = ProxyClient::new().expect("proxy client");

    let lua = proxy.download_lua(app_id).expect("lua");
    let data = DepotData::fetch(&proxy, app_id).expect("depot package");
    let name = format!("{app_id}.lua");
    store
        .save(app_id, Some((&name, &lua)), &data.raw_manifests)
        .expect("save");

    let loaded = store.load(app_id);
    println!("stored at {}", store.app_directory(app_id).display());
    match &loaded.lua {
        Some((name, bytes)) => println!("  lua       {name} ({} bytes)", bytes.len()),
        None => println!("  lua       (none)"),
    }
    for (name, bytes) in &loaded.manifests {
        println!("  manifest  {name} ({} bytes)", bytes.len());
    }
    println!("total on disk: {} bytes", store.size_bytes());
    assert_eq!(
        loaded.manifests, data.raw_manifests,
        "round trip must be byte-exact"
    );
    println!("round trip verified.");
}
