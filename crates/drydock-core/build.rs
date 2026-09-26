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
    // Only for an update repository that is private (a white-label product under test): a
    // read-only token for it. Drydock's releases are public, so its builds embed none.
    embed_secret("DRYDOCK_UPDATE_TOKEN", "update-token.secret");
    let brand = brand_directory();
    let white_label = embed_product_name(brand.as_deref());
    embed_activation_key(brand.as_deref());
    embed_update_repository(white_label);
    // `version.rs` reads this with `option_env!` rather than the build script setting it, so declare
    // the dependency here: without it cargo can reuse a cached `drydock-core` compiled against a
    // different version, and the binary reports a version that was never asked for. The release
    // workflow sets it from the git tag; locally it is how you build a binary that outranks every
    // published release (`DRYDOCK_RELEASE_VERSION=9.9.9`) so the self-updater leaves it alone.
    println!("cargo:rerun-if-env-changed=DRYDOCK_RELEASE_VERSION");
}

/// The product this build is, for `src/brand.rs`: the `name` from a white-label brand file (see
/// `docs/branding.md`), or Drydock. Returns whether the build is white-label.
fn embed_product_name(brand: Option<&Path>) -> bool {
    let name = match brand {
        Some(directory) => brand_name(&directory.join("brand.toml")),
        None => "Drydock".to_owned(),
    };
    println!("cargo:rustc-env=DRYDOCK_PRODUCT_NAME={name}");
    if brand.is_some() {
        println!("cargo:rustc-env=DRYDOCK_WHITE_LABEL=1");
    }
    brand.is_some()
}

/// The activation authority's public key, for a white-label product that activates through a bot
/// of its own: `activation-key.pem` in its brand folder. The app encrypts its requests to this key
/// and checks the responses' signatures against it, so it has to be that bot's key. Without the
/// file the product uses Drydock's bot, whose key `activation.rs` has built in.
fn embed_activation_key(brand: Option<&Path>) {
    let Some(file) = brand
        .map(|directory| directory.join("activation-key.pem"))
        .filter(|file| file.is_file())
    else {
        return;
    };
    let text = fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    assert!(
        !text.contains("PRIVATE KEY"),
        "{} is a PRIVATE key. Delete it from the brand folder at once — it must never be in a \
         repository or a build. The app needs only the public key: \
         `openssl pkey -in private.pem -pubout -out activation-key.pem`",
        file.display()
    );
    let body: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    let usable = text.contains("-----BEGIN PUBLIC KEY-----")
        && !body.is_empty()
        && body.len() <= 4096
        && body
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='));
    assert!(
        usable,
        "{} must be a public key in PEM form (-----BEGIN PUBLIC KEY----- …)",
        file.display()
    );
    println!("cargo:rustc-env=DRYDOCK_ACTIVATION_KEY={body}");
}

/// The white-label brand directory, if this build has one: `DRYDOCK_BRAND_DIR` when set (relative
/// paths from the workspace root), else `brand/` at the workspace root. Drydock itself has neither.
///
/// Only an existing brand folder is watched for changes: telling Cargo to watch a path that does not
/// exist makes it rerun the build script — and rebuild the crate — on every single build. The whole
/// folder is watched, so adding a file to it (`activation-key.pem`) is noticed too.
fn brand_directory() -> Option<std::path::PathBuf> {
    println!("cargo:rerun-if-env-changed=DRYDOCK_BRAND_DIR");
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    // crates/drydock-core → the workspace root.
    let workspace = manifest.ancestors().nth(2).expect("workspace root");
    let directory = match std::env::var("DRYDOCK_BRAND_DIR") {
        Ok(value) if !value.trim().is_empty() => workspace.join(value.trim()),
        _ => workspace.join("brand"),
    };
    let file = directory.join("brand.toml");
    if !file.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", directory.display());
    Some(directory)
}

/// The brand's `name`, which becomes the window title, the executable and the release assets, so it
/// has to be usable as a file name everywhere.
fn brand_name(file: &Path) -> String {
    let text = fs::read_to_string(file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    let value: toml::Value = text
        .parse()
        .unwrap_or_else(|error| panic!("{} is not valid TOML: {error}", file.display()));
    let name = value
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or_else(|| panic!("{} needs a `name`", file.display()))
        .trim()
        .to_owned();
    let usable = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    assert!(
        usable,
        "{}: `name` {name:?} must start with a letter and use only letters, digits, - and _ (it \
         names the executable and the release files)",
        file.display()
    );
    name
}

fn embed_update_repository(white_label: bool) {
    println!("cargo:rerun-if-env-changed=DRYDOCK_UPDATE_REPOSITORY");
    let explicit = std::env::var("DRYDOCK_UPDATE_REPOSITORY")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    // Only Drydock itself defaults to Drydock's releases. A white-label build gets no channel unless
    // its release workflow names one: pointed at Drydock's, it would either fail to find its own
    // assets on every check or — worse, with a matching name — install Drydock over itself.
    let value = explicit.or_else(|| (!white_label).then(|| DEFAULT_UPDATE_REPOSITORY.to_owned()));
    if let Some(value) = value {
        println!("cargo:rustc-env=DRYDOCK_UPDATE_REPOSITORY={value}");
    }
}

fn embed_secret(env_var: &str, secret_file: &str) {
    println!("cargo:rerun-if-env-changed={env_var}");
    println!("cargo:rerun-if-changed={secret_file}");

    // Both sources are trimmed. A value pasted into a CI secret or produced by `echo` easily picks
    // up trailing whitespace, and embedding that verbatim would ship a base URL ending in a newline
    // or a signing secret that no longer matches the proxy's.
    let value = std::env::var(env_var)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
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
