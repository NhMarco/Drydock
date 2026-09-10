/// Version displayed by the application and used by the self-updater.
///
/// Development builds use the Cargo package version. Official release builds
/// inject the version derived from the immutable Git tag.
pub const APP_VERSION: &str = match option_env!("DRYDOCK_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

pub fn user_agent() -> String {
    format!("Drydock/{APP_VERSION}")
}
