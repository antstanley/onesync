//! In-flight OAuth login session tracking.
//!
//! `account.login.begin` allocates a [`LoginSession`] keyed by an opaque `login_handle`
//! (a ULID string), spawns the loopback listener task, and stashes the session here.
//! `account.login.await` pops the session by handle, awaits the code, and continues the
//! exchange.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use onesync_graph::error::GraphInternalError;

/// One in-flight OAuth login.
///
/// Held in the registry between `account.login.begin` and `account.login.await`.
pub struct LoginSession {
    /// Receiver for the auth code (sent by the listener task on redirect).
    pub code_rx: oneshot::Receiver<Result<String, GraphInternalError>>,
    /// PKCE verifier from `begin`, sent to the token endpoint by `await`.
    pub pkce_verifier: String,
    /// `http://localhost:<port>/callback` — must match the value passed to the auth URL.
    pub redirect_uri: String,
}

/// Registry of in-flight login sessions, keyed by login-handle string.
#[derive(Default)]
pub struct LoginRegistry {
    sessions: Mutex<HashMap<String, LoginSession>>,
}

impl LoginRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a session under `handle`. Overwrites any prior session with the same handle.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    pub fn insert(&self, handle: String, session: LoginSession) {
        #[allow(clippy::expect_used)]
        // LINT: mutex-poison in a daemon-internal collection is unrecoverable; daemon must crash.
        let mut g = self.sessions.lock().expect("login registry mutex poisoned");
        g.insert(handle, session);
    }

    /// Remove and return the session for `handle`, or `None` if it does not exist.
    ///
    /// # Panics
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn take(&self, handle: &str) -> Option<LoginSession> {
        #[allow(clippy::expect_used)]
        // LINT: mutex-poison in a daemon-internal collection is unrecoverable; daemon must crash.
        let mut g = self.sessions.lock().expect("login registry mutex poisoned");
        g.remove(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_then_take_returns_the_session() {
        let reg = LoginRegistry::new();
        let (tx, rx) = oneshot::channel();
        reg.insert(
            "lgn-1".to_owned(),
            LoginSession {
                code_rx: rx,
                pkce_verifier: "v".to_owned(),
                redirect_uri: "http://localhost:1234/callback".to_owned(),
            },
        );

        // Drop the sender to satisfy the receiver later.
        drop(tx);

        let session = reg.take("lgn-1").expect("present");
        assert_eq!(session.pkce_verifier, "v");
        assert_eq!(session.redirect_uri, "http://localhost:1234/callback");
    }

    #[tokio::test]
    async fn take_returns_none_for_unknown_handle() {
        let reg = LoginRegistry::new();
        assert!(reg.take("missing").is_none());
    }

    #[tokio::test]
    async fn take_is_idempotent_consume() {
        let reg = LoginRegistry::new();
        let (_tx, rx) = oneshot::channel::<Result<String, GraphInternalError>>();
        reg.insert(
            "lgn-2".to_owned(),
            LoginSession {
                code_rx: rx,
                pkce_verifier: "v".to_owned(),
                redirect_uri: "http://localhost:1234/callback".to_owned(),
            },
        );
        assert!(reg.take("lgn-2").is_some());
        // Second take returns None because the session was removed.
        assert!(reg.take("lgn-2").is_none());
    }
}
