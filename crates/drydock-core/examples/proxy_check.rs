//! End-to-end connectivity check against the configured proxy.
//!
//! Exercises the real signing path — `X-Drydock-*` headers, the canonical signing string, the
//! resolved base URL and secret — against a live proxy, and reports what each endpoint answered.
//! Useful after changing the proxy address, rotating the shared secret, or standing up your own
//! instance: it separates "the proxy is unreachable" from "my credentials do not match".
//!
//! Run: `cargo run -p drydock-core --example proxy_check`

use drydock_core::proxy::{ProxyClient, ProxyError};

fn main() {
    for entry in drydock_core::describe_config() {
        println!(
            "{:<18} {:<44} [{}]",
            entry.name,
            entry.value,
            entry.source.label()
        );
    }
    println!();

    let client = match ProxyClient::new() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("Could not build a proxy client: {error}");
            eprintln!("Set DRYDOCK_PROXY_BASE_URL and DRYDOCK_HMAC_SECRET, or fill them in under");
            eprintln!("Settings > Proxy / Self-hosting. See README > Configuration.");
            std::process::exit(1);
        }
    };

    let mut failures = 0;
    let mut check = |name: &str, result: Result<String, ProxyError>| match result {
        Ok(detail) => println!("  ok    {name:<22} {detail}"),
        Err(error) => {
            failures += 1;
            println!("  FAIL  {name:<22} {error}");
        }
    };

    check(
        "gamelist",
        client.fetch_catalog().map(|apps| format!("{} games", apps.len())),
    );
    check(
        "service/manifest",
        client
            .steam_service_manifest()
            .map(|manifest| format!("version {}, {} file(s)", manifest.version, manifest.files.len())),
    );
    check(
        "repacks",
        client.repacks().map(|list| format!("{} app(s)", list.len())),
    );
    check(
        "denuvo-fixes",
        client.denuvo_fixes().map(|list| format!("{} app(s)", list.len())),
    );
    check(
        "app-schema/730",
        client.app_schema(730).map(|json| format!("{} bytes", json.len())),
    );

    println!();
    if failures == 0 {
        println!("All endpoints answered.");
    } else {
        println!("{failures} endpoint(s) failed.");
        std::process::exit(1);
    }
}
