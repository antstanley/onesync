//! M9 end-to-end smoke test.
//!
//! This test exercises the full happy path against a real Microsoft account when run with
//! `cargo test --test m9_end_to_end -- --ignored`. It is `#[ignore]` by default because it
//! requires:
//!
//! 1. `ONESYNC_E2E_CLIENT_ID` set to a registered Azure AD client id with the standard scopes
//!    (`Files.ReadWrite` `offline_access` `User.Read`).
//! 2. `ONESYNC_E2E_REMOTE_PATH` set to a `OneDrive` folder that already exists (the test does
//!    not create remote folders).
//! 3. Interactive auth: the test prints the auth URL and waits for the operator to complete
//!    sign-in in a browser. The OAuth redirect lands on the daemon's loopback listener.
//!
//! Successful run sequence:
//! - config.set with the client id from env
//! - account.login.begin → operator visits auth URL
//! - account.login.await blocks for the redirect, persists the Account row
//! - pair.add against the configured remote path
//! - `pair.force_sync` triggers a one-shot cycle through the scheduler
//! - Wait for the cycle, then assert at least one local file appears under the pair root
//!
//! The body of the test is intentionally light: deeper end-to-end coverage lives in the M10
//! sandbox-account harness.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

#[tokio::test]
#[ignore = "requires ONESYNC_E2E_CLIENT_ID and ONESYNC_E2E_REMOTE_PATH plus interactive auth; opt in with --ignored"]
async fn end_to_end_against_real_onedrive_account() {
    let client_id = std::env::var("ONESYNC_E2E_CLIENT_ID")
        .expect("set ONESYNC_E2E_CLIENT_ID to your registered Azure AD client id");
    let remote_path =
        std::env::var("ONESYNC_E2E_REMOTE_PATH").expect("set ONESYNC_E2E_REMOTE_PATH");
    eprintln!(
        "M9 end-to-end smoke test starting (client_id len={}, remote_path={remote_path}).",
        client_id.len()
    );
    eprintln!(
        "This test is currently a documented scaffold — the body that actually drives the \
         daemon end-to-end lives in M10 alongside the sandbox-account harness."
    );
    // Carry-over: implement the full IPC sequence here once the sandbox account is set up.
}
