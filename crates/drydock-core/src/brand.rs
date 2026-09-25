//! Which product this build is.
//!
//! Drydock can be built as a white-label product: the same program under another name, with its
//! own look. Such a build carries a `brand/brand.toml` (see `docs/branding.md`); without one it is
//! Drydock. Almost everything a brand changes lives in the desktop app — colours, navigation,
//! which optional features it offers — but a few things are needed down here, and they all follow
//! from the product's name:
//!
//! * the data directory (`%LOCALAPPDATA%\<Product>`), so each product keeps its own settings;
//! * the executable the self-updater may replace, and the release assets it looks for — a
//!   white-label build must never mistake a Drydock release for its own update.
//!
//! The name comes from `build.rs`, which reads it from the brand file. Identifiers that other builds
//! and the proxy rely on — the request headers, the `DRYDOCK_*` environment variables, the user
//! agent, the activation marker — stay Drydock's in every build.

/// A product this codebase can be built as.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Product {
    /// The name shown to people: window title, headings, messages.
    pub name: &'static str,
    /// The executable's file name on this platform.
    pub executable: &'static str,
    /// The release asset for each platform the release workflow builds.
    pub assets: ReleaseAssets,
}

/// The file names a GitHub release carries for each native target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReleaseAssets {
    pub windows_x64: &'static str,
    pub windows_arm64: &'static str,
    pub linux_x64: &'static str,
    pub linux_arm64: &'static str,
}

/// Drydock itself.
pub const DRYDOCK: Product = Product {
    name: "Drydock",
    executable: if cfg!(windows) { "Drydock.exe" } else { "Drydock" },
    assets: ReleaseAssets {
        windows_x64: "Drydock-windows-x64.exe",
        windows_arm64: "Drydock-windows-arm64.exe",
        linux_x64: "Drydock-linux-x64",
        linux_arm64: "Drydock-linux-arm64",
    },
};

/// The product this binary is: Drydock, or the brand it was built with.
pub const PRODUCT: Product = Product {
    name: env!("DRYDOCK_PRODUCT_NAME"),
    executable: if cfg!(windows) {
        concat!(env!("DRYDOCK_PRODUCT_NAME"), ".exe")
    } else {
        env!("DRYDOCK_PRODUCT_NAME")
    },
    assets: ReleaseAssets {
        windows_x64: concat!(env!("DRYDOCK_PRODUCT_NAME"), "-windows-x64.exe"),
        windows_arm64: concat!(env!("DRYDOCK_PRODUCT_NAME"), "-windows-arm64.exe"),
        linux_x64: concat!(env!("DRYDOCK_PRODUCT_NAME"), "-linux-x64"),
        linux_arm64: concat!(env!("DRYDOCK_PRODUCT_NAME"), "-linux-arm64"),
    },
};

/// Whether this build is a white-label product rather than Drydock.
pub const IS_WHITE_LABEL: bool = option_env!("DRYDOCK_WHITE_LABEL").is_some();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_product_names_its_files_after_itself() {
        // The updater of one product only ever looks for files named after it, so this is what
        // keeps one product from installing another over itself.
        for product in [DRYDOCK, PRODUCT] {
            assert_eq!(product.executable.trim_end_matches(".exe"), product.name);
            for asset in [
                product.assets.windows_x64,
                product.assets.windows_arm64,
                product.assets.linux_x64,
                product.assets.linux_arm64,
            ] {
                assert!(asset.starts_with(&format!("{}-", product.name)), "{asset}");
            }
        }
        assert_eq!(IS_WHITE_LABEL, PRODUCT != DRYDOCK);
    }
}
