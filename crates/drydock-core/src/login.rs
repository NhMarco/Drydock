//! Signing in with Discord, so the proxy knows who is asking.
//!
//! Every request to the proxy is signed with a secret that ships inside the app — and anything inside
//! a program people download can be read back out of it, so that secret alone proves nothing. A
//! Discord login adds what cannot be copied out of the program: a session token the proxy issues to
//! one person after they sign in, and which it can limit and revoke. Only the Discord account's ID and
//! name are involved (scope `identify`); no password ever passes through the app or the proxy.
//!
//! The browser hands the result back to this machine only: the app listens on a random loopback port,
//! the proxy sends the browser there with a one-time ticket, and the app trades that ticket — together
//! with the random `state` only it knows — for the session token.
//!
//! The session is kept beside the settings until the proxy lets it expire (a week by default), so
//! the user signs in once a week rather than at every start. On Windows the file is encrypted for
//! the signed-in Windows user (DPAPI): copied to another account or machine it opens nothing.

use std::fs;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::cloud::{random_url_string, read_request_target, url_decode, write_http_response};
use crate::proxy::{ProxyClient, ProxyError};

/// The session file in the per-user data directory, beside `settings.json`.
const SESSION_FILE: &str = "discord-session.dat";
/// The plain-JSON file an earlier test build wrote; removed when the session is next saved or read.
const RETIRED_SESSION_FILE: &str = "discord-session.json";
/// How long the browser has to come back.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// A signed-in Discord account: the token the proxy issued and whom it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: String,
    pub user_name: String,
    /// When the proxy stops accepting the token, Unix seconds.
    pub expires_at: u64,
}

impl Session {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at <= now()
    }
}

/// The session every proxy request carries (see [`current_token`]).
static CURRENT: RwLock<Option<Session>> = RwLock::new(None);
/// Set when the proxy answered that a login is required, for the interface to pick up.
static LOGIN_REQUIRED: AtomicBool = AtomicBool::new(false);

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn set_current(session: Option<Session>) {
    *CURRENT.write().unwrap_or_else(std::sync::PoisonError::into_inner) = session;
}

/// Makes `session` the one every proxy request carries.
fn set_session(session: &Session) {
    set_current(Some(session.clone()));
    LOGIN_REQUIRED.store(false, Ordering::Relaxed);
}

/// Where the session is kept, in the per-user data directory `settings_dir`.
#[must_use]
pub fn session_path(settings_dir: &Path) -> PathBuf {
    settings_dir.join(SESSION_FILE)
}

/// The file's bytes for `session`: encrypted for this Windows user, or `None` when that fails —
/// then the session is not written at all rather than written in the clear.
fn seal(session: &Session) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(session).ok()?;
    #[cfg(windows)]
    {
        crate::cloud::dpapi_protect(&json)
    }
    #[cfg(not(windows))]
    {
        Some(json)
    }
}

fn unseal(bytes: &[u8]) -> Option<Session> {
    #[cfg(windows)]
    let bytes = crate::cloud::dpapi_unprotect(bytes)?;
    serde_json::from_slice(&bytes).ok()
}

/// Loads the saved session and makes it the current one. An expired, unreadable, or foreign one
/// (another Windows user's, another machine's) is dropped.
pub fn load_session(path: &Path) -> Option<Session> {
    remove_retired(path);
    let session = fs::read(path)
        .ok()
        .and_then(|bytes| unseal(&bytes))
        .filter(|session| !session.is_expired() && !session.token.is_empty());
    match &session {
        Some(session) => set_session(session),
        None => forget_session(path),
    }
    session
}

/// Makes `session` the current one and keeps it for the next start.
pub fn save_session(path: &Path, session: &Session) -> io::Result<()> {
    set_session(session);
    remove_retired(path);
    let Some(bytes) = seal(session) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("dat.tmp");
    fs::write(&temporary, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&temporary, path)
}

/// Signs out: forgets the saved session and stops sending it.
pub fn forget_session(path: &Path) {
    let _ = fs::remove_file(path);
    set_current(None);
}

fn remove_retired(path: &Path) {
    if let Some(directory) = path.parent() {
        let _ = fs::remove_file(directory.join(RETIRED_SESSION_FILE));
    }
}

/// The current session, if signed in.
#[must_use]
pub fn current_session() -> Option<Session> {
    CURRENT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .filter(|session| !session.is_expired())
}

/// The token a proxy request carries, if signed in.
pub(crate) fn current_token() -> Option<String> {
    current_session().map(|session| session.token)
}

pub(crate) fn note_login_required() {
    LOGIN_REQUIRED.store(true, Ordering::Relaxed);
}

/// Whether the proxy has asked for a login since this was last asked — for the interface to show
/// the login. Reading it clears it.
pub fn take_login_required() -> bool {
    LOGIN_REQUIRED.swap(false, Ordering::Relaxed)
}

#[derive(Debug, Error)]
pub enum LoginError {
    #[error("The sign-in was cancelled")]
    Cancelled,
    #[error("The sign-in took too long — start it again")]
    TimedOut,
    #[error("Discord did not allow the sign-in")]
    Denied,
    #[error("This Discord account is blocked")]
    Banned,
    #[error("Discord could not be reached — try again in a moment")]
    Discord,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Proxy(#[from] ProxyError),
}

/// Runs the whole Discord sign-in: hands the proxy's login link to `show_link`, waits for a browser
/// to come back to this machine, trades the ticket for a session and keeps it beside the settings in
/// `settings_dir`. Returns early with [`LoginError::Cancelled`] once `cancel` is set.
///
/// `show_link` opens the link in a browser and also offers it to be copied: whether a browser came up
/// does not end the sign-in, because the link works from any browser on this machine — the one a
/// browser picker hands it to, or the one the user pastes it into.
pub fn discord_login(
    client: &ProxyClient,
    settings_dir: &Path,
    cancel: &AtomicBool,
    show_link: impl FnOnce(&str),
) -> Result<Session, LoginError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let state = random_url_string(43);
    show_link(&client.discord_login_url(port, &state));
    let ticket = wait_for_ticket(&listener, &state, cancel)?;
    let session = client.redeem_discord_login(&ticket, &state)?;
    save_session(&session_path(settings_dir), &session)?;
    Ok(session)
}

/// Waits on the loopback listener for the browser's return, answering it with a page that sends the
/// user back to the app. Anything else that knocks (a favicon, a stale tab) is answered and ignored.
fn wait_for_ticket(listener: &TcpListener, state: &str, cancel: &AtomicBool) -> Result<String, LoginError> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(LoginError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(LoginError::TimedOut);
        }
        let mut stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let Some(target) = read_request_target(&mut stream) else {
            continue;
        };
        let Some(query) = target.strip_prefix("/discord-login?") else {
            let _ = write_http_response(&mut stream, "204 No Content", "");
            continue;
        };
        let value = |key: &str| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .find(|(name, _)| *name == key)
                .map(|(_, value)| url_decode(value))
        };
        if value("state").as_deref() != Some(state) {
            // Another sign-in's tab; this one keeps waiting for its own.
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "<h1>Old sign-in</h1><p>This page belongs to an earlier attempt. Use the newest one, or \
                 start again in {product}.</p>",
            );
            continue;
        }
        if let Some(error) = value("error") {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "<h1>Sign-in failed</h1><p>You can close this tab and return to {product}.</p>",
            );
            return Err(match error.as_str() {
                "banned" => LoginError::Banned,
                "denied" => LoginError::Denied,
                _ => LoginError::Discord,
            });
        }
        if let Some(ticket) = value("ticket") {
            let _ = write_http_response(
                &mut stream,
                "200 OK",
                "<h1>Signed in</h1><p>You can close this tab and return to {product}.</p>",
            );
            return Ok(ticket);
        }
        let _ = write_http_response(&mut stream, "400 Bad Request", "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn visit(port: u16, target: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        write!(stream, "GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("request");
        let mut page = String::new();
        let _ = stream.read_to_string(&mut page);
        page
    }

    #[test]
    fn the_ticket_is_taken_only_from_this_sign_ins_return() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("port").port();
        let visits = std::thread::spawn(move || {
            let favicon = visit(port, "/favicon.ico");
            let stale = visit(port, "/discord-login?state=other&ticket=stolen");
            let real = visit(port, "/discord-login?state=mine&ticket=T1");
            (favicon, stale, real)
        });
        let ticket = wait_for_ticket(&listener, "mine", &AtomicBool::new(false)).expect("ticket");
        let (favicon, stale, real) = visits.join().expect("visits");
        assert_eq!(ticket, "T1");
        assert!(favicon.starts_with("HTTP/1.1 204"));
        assert!(
            stale.contains("Old sign-in"),
            "a stale tab is answered, not taken"
        );
        assert!(real.contains("Signed in"));
    }

    #[test]
    fn a_refused_or_blocked_sign_in_says_which() {
        for (error, expected) in [
            ("banned", "blocked"),
            ("denied", "did not allow"),
            ("discord", "reached"),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
            let port = listener.local_addr().expect("port").port();
            let target = format!("/discord-login?state=s&error={error}");
            let visit = std::thread::spawn(move || visit(port, &target));
            let result = wait_for_ticket(&listener, "s", &AtomicBool::new(false));
            visit.join().expect("visit");
            assert!(
                result.expect_err("an error").to_string().contains(expected),
                "{error}"
            );
        }
    }

    #[test]
    fn a_cancelled_sign_in_stops_waiting() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let result = wait_for_ticket(&listener, "s", &AtomicBool::new(true));
        assert!(matches!(result, Err(LoginError::Cancelled)));
    }

    #[test]
    fn a_saved_session_comes_back_sealed_and_an_expired_one_does_not() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = session_path(directory.path());
        let retired = directory.path().join(RETIRED_SESSION_FILE);
        fs::write(&retired, b"{}").expect("write");
        let session = Session {
            token: "dl1.a.b".into(),
            user_id: "4242".into(),
            user_name: "Marco".into(),
            expires_at: now() + 3600,
        };
        save_session(&path, &session).expect("save");
        assert!(!retired.exists(), "the plain file of an earlier build is removed");
        let on_disk = fs::read(&path).expect("read");
        if cfg!(windows) {
            assert!(
                !String::from_utf8_lossy(&on_disk).contains("dl1.a.b"),
                "the token is not on disk in the clear"
            );
        }
        forget_session(&directory.path().join("elsewhere.dat"));
        assert_eq!(load_session(&path), Some(session.clone()));
        assert_eq!(current_token().as_deref(), Some("dl1.a.b"));

        save_session(
            &path,
            &Session {
                expires_at: now() - 1,
                ..session
            },
        )
        .expect("save");
        assert_eq!(load_session(&path), None, "an expired session is dropped");
        assert!(!path.exists());
        assert_eq!(current_token(), None);

        fs::write(&path, b"not a sealed session").expect("write");
        assert_eq!(
            load_session(&path),
            None,
            "a foreign or broken file opens nothing"
        );
    }
}
