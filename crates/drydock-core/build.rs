//! Injects build-time secrets (never source-code literals) into the binary.
//!
//! Each secret is supplied by (in priority order) an environment variable or a git-ignored
//! `*.secret` file next to this script, then embedded via `cargo:rustc-env` so `option_env!`
//! can read it in the crate source. When neither source is present the value is simply
//! absent and the client falls back to a runtime environment variable or anonymous access.

use std::fs;
use std::path::Path;

/// The public GitHub repository the auto-updater checks for the latest release. Baked in by
/// default so shipped release builds "just work"; override with the `DRYDOCK_UPDATE_REPOSITORY`
/// environment variable (the release workflow sets it explicitly).
const DEFAULT_UPDATE_REPOSITORY: &str = "NhMarco/Drydock";

fn main() {
    // The app now talks only to the self-hosted proxy: no steamtools key or GitHub token ships
    // in the binary, just the proxy address and the shared HMAC signing secret.
    embed_secret("DRYDOCK_PROXY_BASE_URL", "proxy-base-url.secret");
    embed_secret("DRYDOCK_HMAC_SECRET", "hmac-secret.secret");
    embed_update_repository();
    // `version.rs` reads this with `option_env!` rather than the build script setting it, so declare
    // the dependency here: without it cargo can reuse a cached `drydock-core` compiled against a
    // different version, and the binary reports a version that was never asked for. The release
    // workflow sets it from the git tag; locally it is how you build a binary that outranks every
    // published release (`DRYDOCK_RELEASE_VERSION=9.9.9`) so the self-updater leaves it alone.
    println!("cargo:rerun-if-env-changed=DRYDOCK_RELEASE_VERSION");
}

fn embed_update_repository() {
    println!("cargo:rerun-if-env-changed=DRYDOCK_UPDATE_REPOSITORY");
    let value = std::env::var("DRYDOCK_UPDATE_REPOSITORY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_UPDATE_REPOSITORY.to_owned());
    println!("cargo:rustc-env=DRYDOCK_UPDATE_REPOSITORY={value}");
}

fn embed_secret(env_var: &str, secret_file: &str) {
    println!("cargo:rerun-if-env-changed={env_var}");
    println!("cargo:rerun-if-changed={secret_file}");

    let value = std::env::var(env_var)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            fs::read_to_string(Path::new(secret_file))
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        });

    if let Some(value) = value {
        println!("cargo:rustc-env={env_var}={value}");
    }
}
