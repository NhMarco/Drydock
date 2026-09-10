//! Runtime configuration: one place that resolves every value a Drydock deployment can change.
//!
//! Drydock is meant to be self-hostable — the proxy is open source and anyone can run their own —
//! so none of these values may be baked in as source literals. Each is resolved from three layers,
//! **highest priority first**:
//!
//! 1. **Environment variable** — for scripting, CI and development. Always wins, never persisted.
//! 2. **User override** — what the user typed into Settings, persisted in `settings.json` and pushed
//!    here via [`set_user_overrides`] at startup and on every change. This is how someone points a
//!    stock build at their own proxy **without rebuilding**.
//! 3. **Build-time default** — embedded by `build.rs` from an env var or a git-ignored `*.secret`
//!    file. This is what official release builds ship with; a clone of the repo has none, which is
//!    fine: the app starts, says the proxy is unconfigured, and the local-only features still work.
//!
//! When no layer supplies a value the caller gets `None` and must degrade gracefully rather than
//! panicking — a fresh `git clone && cargo run` has to reach a usable window.
//!
//! [`describe`] renders the resolved state (with secrets redacted) for `--config`, so "why is it
//! talking to the wrong proxy" is answerable without a debugger.

use std::sync::{OnceLock, RwLock};

/// Values a user can override from the Settings UI. Empty strings mean "not set", which is what a
/// blank field in the UI produces — it falls through to the build-time default rather than
/// overriding it with nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserOverrides {
    pub proxy_base_url: String,
    pub hmac_secret: String,
    pub update_repository: String,
}

fn overrides() -> &'static RwLock<UserOverrides> {
    static OVERRIDES: OnceLock<RwLock<UserOverrides>> = OnceLock::new();
    OVERRIDES.get_or_init(|| RwLock::new(UserOverrides::default()))
}

/// Installs the user's Settings-supplied overrides. Call at startup and whenever they change; the
/// next resolution picks them up, so no restart is needed after editing the proxy address.
pub fn set_user_overrides(values: UserOverrides) {
    if let Ok(mut guard) = overrides().write() {
        *guard = values;
    }
}

/// The currently installed user overrides.
#[must_use]
pub fn user_overrides() -> UserOverrides {
    overrides().read().map(|guard| guard.clone()).unwrap_or_default()
}

/// Which layer a resolved value came from — shown by `--config` and the Settings diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Environment,
    UserSettings,
    BuildDefault,
    Unset,
}

impl Source {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Environment => "environment variable",
            Self::UserSettings => "settings",
            Self::BuildDefault => "built in",
            Self::Unset => "not configured",
        }
    }
}

/// Resolves one setting through the three layers, trimming and ignoring blanks at every level.
fn resolve(env_var: &str, user_value: &str, build_default: Option<&'static str>) -> (Option<String>, Source) {
    if let Ok(value) = std::env::var(env_var) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return (Some(trimmed.to_owned()), Source::Environment);
        }
    }
    let trimmed = user_value.trim();
    if !trimmed.is_empty() {
        return (Some(trimmed.to_owned()), Source::UserSettings);
    }
    match build_default.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => (Some(value.to_owned()), Source::BuildDefault),
        None => (None, Source::Unset),
    }
}

/// Base URL of the Drydock proxy, origin only (`https://proxy.example`), trailing slash stripped.
#[must_use]
pub fn proxy_base_url() -> Option<String> {
    proxy_base_url_with_source().0.map(|value| normalize_base(&value))
}

#[must_use]
pub fn proxy_base_url_with_source() -> (Option<String>, Source) {
    resolve(
        "DRYDOCK_PROXY_BASE_URL",
        &user_overrides().proxy_base_url,
        option_env!("DRYDOCK_PROXY_BASE_URL"),
    )
}

/// Shared HMAC secret used to sign proxy requests. Must match one of the proxy's
/// `DRYDOCK_HMAC_SECRET` values.
#[must_use]
pub fn hmac_secret() -> Option<String> {
    hmac_secret_with_source().0
}

#[must_use]
pub fn hmac_secret_with_source() -> (Option<String>, Source) {
    resolve(
        "DRYDOCK_HMAC_SECRET",
        &user_overrides().hmac_secret,
        option_env!("DRYDOCK_HMAC_SECRET"),
    )
}

/// `owner/repo` the self-updater checks for new releases.
///
/// Runtime-overridable so a fork can ship its own update channel without patching `build.rs`. That
/// is not a new attack surface: anyone who can set this process's environment or write its settings
/// file can already replace the executable outright.
#[must_use]
pub fn update_repository() -> Option<String> {
    update_repository_with_source()
        .0
        .filter(|value| is_valid_repository(value))
}

#[must_use]
pub fn update_repository_with_source() -> (Option<String>, Source) {
    resolve(
        "DRYDOCK_UPDATE_REPOSITORY",
        &user_overrides().update_repository,
        option_env!("DRYDOCK_UPDATE_REPOSITORY"),
    )
}

/// Optional GitHub token for the catalog/updater calls, raising the anonymous API rate limit.
/// Environment-only on purpose: a token is a credential and does not belong in `settings.json`.
#[must_use]
pub fn github_token() -> Option<String> {
    std::env::var("DRYDOCK_GITHUB_TOKEN")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// `owner/repo`, conservatively validated so a typo cannot turn into a request somewhere unexpected.
#[must_use]
pub fn is_valid_repository(value: &str) -> bool {
    let Some((owner, repo)) = value.split_once('/') else {
        return false;
    };
    let ok = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    ok(owner) && ok(repo)
}

fn normalize_base(value: &str) -> String {
    value.trim().trim_end_matches('/').to_owned()
}

/// One line of the resolved configuration, for `--config`.
#[derive(Clone, Debug)]
pub struct ConfigEntry {
    pub name: &'static str,
    pub env_var: &'static str,
    /// Already redacted when the value is a secret.
    pub value: String,
    pub source: Source,
}

/// The full resolved configuration, with secrets redacted, for the `--config` diagnostic.
#[must_use]
pub fn describe() -> Vec<ConfigEntry> {
    let (proxy, proxy_source) = proxy_base_url_with_source();
    let (secret, secret_source) = hmac_secret_with_source();
    let (repository, repository_source) = update_repository_with_source();
    vec![
        ConfigEntry {
            name: "Proxy base URL",
            env_var: "DRYDOCK_PROXY_BASE_URL",
            value: proxy.map_or_else(|| "(none)".to_owned(), |value| normalize_base(&value)),
            source: proxy_source,
        },
        ConfigEntry {
            name: "Proxy HMAC secret",
            env_var: "DRYDOCK_HMAC_SECRET",
            value: secret.as_deref().map_or_else(|| "(none)".to_owned(), redact),
            source: secret_source,
        },
        ConfigEntry {
            name: "Update repository",
            env_var: "DRYDOCK_UPDATE_REPOSITORY",
            value: repository.unwrap_or_else(|| "(none)".to_owned()),
            source: repository_source,
        },
        ConfigEntry {
            name: "GitHub token",
            env_var: "DRYDOCK_GITHUB_TOKEN",
            value: github_token()
                .as_deref()
                .map_or_else(|| "(none)".to_owned(), redact),
            source: if github_token().is_some() {
                Source::Environment
            } else {
                Source::Unset
            },
        },
    ]
}

/// Shows only enough of a secret to tell two apart, never enough to use one.
fn redact(secret: &str) -> String {
    let visible: String = secret.chars().take(4).collect();
    format!("{visible}… ({} chars)", secret.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both the environment and the user-override store are process-global, so every test that
    /// touches either takes this lock and restores what it changed. Without it the override tests
    /// race each other and fail intermittently.
    static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard(&'static str);
    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            // SAFETY: all env access in these tests is serialised by `ENV_LOCK`.
            unsafe { std::env::set_var(name, value) };
            Self(name)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    #[test]
    fn resolution_order_is_env_then_settings_then_build_default() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());

        // Only a build default present.
        assert_eq!(
            resolve("DRYDOCK_TEST_VALUE", "", Some("built-in")),
            (Some("built-in".to_owned()), Source::BuildDefault)
        );
        // Settings beat the build default.
        assert_eq!(
            resolve("DRYDOCK_TEST_VALUE", "from-settings", Some("built-in")),
            (Some("from-settings".to_owned()), Source::UserSettings)
        );
        // The environment beats both.
        let _guard = EnvGuard::set("DRYDOCK_TEST_VALUE", "from-env");
        assert_eq!(
            resolve("DRYDOCK_TEST_VALUE", "from-settings", Some("built-in")),
            (Some("from-env".to_owned()), Source::Environment)
        );
    }

    #[test]
    fn blank_values_fall_through_instead_of_overriding() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        // An empty Settings field must not blank out the built-in default.
        assert_eq!(
            resolve("DRYDOCK_TEST_BLANK", "   ", Some("built-in")),
            (Some("built-in".to_owned()), Source::BuildDefault)
        );
        // …and neither must an empty environment variable.
        let _guard = EnvGuard::set("DRYDOCK_TEST_BLANK", "  ");
        assert_eq!(
            resolve("DRYDOCK_TEST_BLANK", "from-settings", Some("built-in")),
            (Some("from-settings".to_owned()), Source::UserSettings)
        );
    }

    #[test]
    fn nothing_configured_is_reported_as_unset_not_as_a_panic() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        assert_eq!(resolve("DRYDOCK_TEST_MISSING", "", None), (None, Source::Unset));
    }

    /// A Settings-supplied proxy address must reach `proxy_base_url()` and be normalised on the way.
    ///
    /// The environment has to be cleared first: cargo passes the `cargo:rustc-env` values from
    /// `build.rs` into the environment of binaries it launches, so under `cargo test` (and
    /// `cargo run`) the build-time default also occupies the highest-priority layer. A binary the
    /// user starts directly sees no such variable, which is the case this test is about.
    #[test]
    fn user_overrides_beat_the_build_default() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let restore = std::env::var("DRYDOCK_PROXY_BASE_URL").ok();
        // SAFETY: env access in these tests is serialised by `STATE_LOCK`, and the original value is
        // restored below.
        unsafe { std::env::remove_var("DRYDOCK_PROXY_BASE_URL") };

        set_user_overrides(UserOverrides {
            proxy_base_url: "https://my.proxy/".to_owned(),
            ..UserOverrides::default()
        });
        let (_, source) = proxy_base_url_with_source();
        assert_eq!(proxy_base_url().as_deref(), Some("https://my.proxy"));
        assert_eq!(source, Source::UserSettings);

        set_user_overrides(UserOverrides::default());
        if let Some(value) = restore {
            // SAFETY: as above.
            unsafe { std::env::set_var("DRYDOCK_PROXY_BASE_URL", value) };
        }
    }

    /// The proxy client appends paths straight onto this, so a trailing slash would produce `//v1/…`
    /// — which the proxy signs differently than the client did.
    #[test]
    fn normalizes_base_url_trailing_slash_and_whitespace() {
        assert_eq!(normalize_base("https://proxy.example/"), "https://proxy.example");
        assert_eq!(
            normalize_base("  https://proxy.example  "),
            "https://proxy.example"
        );
        assert_eq!(normalize_base("https://proxy.example"), "https://proxy.example");
    }

    #[test]
    fn repository_validation_rejects_junk() {
        assert!(is_valid_repository("NhMarco/Drydock"));
        assert!(is_valid_repository("some-org/repo.name_1"));
        assert!(!is_valid_repository("no-slash"));
        assert!(!is_valid_repository("/repo"));
        assert!(!is_valid_repository("owner/"));
        assert!(!is_valid_repository("owner/repo/extra"));
        assert!(!is_valid_repository("owner/repo;rm -rf"));
        assert!(!is_valid_repository("https://github.com/owner/repo"));
    }

    #[test]
    fn secrets_are_redacted_in_the_diagnostic_output() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let shown = redact("supersecretvalue");
        assert!(
            shown.starts_with("supe"),
            "a short prefix helps tell two secrets apart"
        );
        assert!(
            !shown.contains("secretvalue"),
            "the rest must never be printed: {shown}"
        );
        assert!(shown.contains("16 chars"));
    }

    #[test]
    fn describe_never_prints_a_usable_secret() {
        let _lock = STATE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        set_user_overrides(UserOverrides {
            hmac_secret: "0123456789abcdef".to_owned(),
            ..UserOverrides::default()
        });
        let rendered = describe()
            .iter()
            .map(|entry| entry.value.clone())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!rendered.contains("0123456789abcdef"));
        set_user_overrides(UserOverrides::default());
    }
}
