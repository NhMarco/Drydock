//! Prints the depot manifests an app's package carries, under the names they would take in Steam's
//! `depotcache`. Used to confirm the naming matches what Steam itself writes.
//!
//! Run: `cargo run -p drydock-core --example depot_manifests -- <appid>`

use drydock_core::depot::DepotData;
use drydock_core::proxy::ProxyClient;

fn main() {
    let app_id: u32 = std::env::args()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .expect("usage: depot_manifests <appid>");
    let proxy = ProxyClient::new().expect("proxy client");
    let data = DepotData::fetch(&proxy, app_id).expect("depot package");
    println!("app {app_id}: {} manifest(s)", data.raw_manifests.len());
    for (name, bytes) in &data.raw_manifests {
        println!("  {name}  ({} bytes)", bytes.len());
    }
}
