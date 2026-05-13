//! M9 / M12b end-to-end smoke test against a real Microsoft account.
//!
//! Runs with `cargo test --test m9_end_to_end -- --ignored`. `#[ignore]` because it
//! requires:
//!
//! 1. `ONESYNC_E2E_CLIENT_ID` set to a registered Azure AD client id with the standard
//!    scopes (`Files.ReadWrite` `offline_access` `User.Read`). The app must register
//!    `http://localhost/callback` as a Public Client redirect URI; Microsoft's loopback
//!    exception lets any ephemeral port match at runtime as long as the host segment is
//!    `localhost`/`127.0.0.1`/`[::1]` and the path matches.
//! 2. `ONESYNC_E2E_REMOTE_PATH` set to a OneDrive folder that already exists (the test
//!    does not create remote folders).
//! 3. Interactive auth: the test prints the auth URL and waits for the operator to
//!    complete sign-in in a browser. The OAuth redirect lands on the daemon's loopback
//!    listener on an ephemeral port.
//!
//! ### Audit notes (M12b, Task C)
//!
//! Walking the PKCE flow on paper against `login.microsoftonline.com`:
//!
//! - **Authority** — `common` is correct. It accepts MSA (personal) and AAD (work/school)
//!   accounts and routes per-account at sign-in. `consumers` is MSA-only; `organizations`
//!   is AAD-only; a tenant GUID is single-tenant only.
//! - **Scopes** — `Files.ReadWrite offline_access User.Read` is the right delegated set.
//!   `offline_access` is required for a refresh token. `Files.ReadWrite.All` is only
//!   needed for SharePoint (M11).
//! - **Redirect URI** — the daemon binds an ephemeral loopback port. Microsoft's loopback
//!   special-case ignores port mismatches as long as the host is `localhost`/`127.0.0.1`/
//!   `[::1]` and the path matches the registered value. Registering `http://localhost/callback`
//!   in the app portal makes `http://localhost:<port>/callback` accepted at runtime.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    clippy::panic,
    clippy::doc_markdown
)]

use std::sync::Arc;
use std::time::Instant;

use onesync_daemon::ipc::server;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_protocol::rpc::{JsonRpcRequest, JsonRpcResponse};
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Default)]
struct NullAuditSink;
impl onesync_core::ports::AuditSink for NullAuditSink {
    fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
}

fn make_ctx() -> DispatchCtx {
    DispatchCtx {
        started_at: Instant::now(),
        state: Arc::new(InMemoryStore::new()),
        local_fs: Arc::new(onesync_fs_local::fakes::InMemoryLocalFs::new()),
        clock: Arc::new(onesync_time::SystemClock),
        ids: Arc::new(onesync_time::UlidGenerator::default()),
        audit: Arc::new(NullAuditSink),
        vault: Arc::new(onesync_keychain::fakes::InMemoryTokenVault::default()),
        http: reqwest::Client::new(),
        login_registry: Arc::new(onesync_daemon::login_registry::LoginRegistry::new()),
        shutdown_token: onesync_daemon::shutdown::ShutdownToken::new(),
        state_dir: std::path::PathBuf::from("/tmp/onesync-test-state"),
        scheduler: onesync_daemon::scheduler::SchedulerHandle::for_tests(),
        subscriptions: onesync_daemon::ipc::subscriptions::SubscriptionRegistry::new(),
    }
}

async fn start_server() -> (ShutdownToken, std::path::PathBuf, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let ctx = make_ctx();

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("IPC server error");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    (token, sock_path, tmp)
}

/// Send one JSON-RPC request over the daemon's Unix socket and read one response.
async fn call(
    sock_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> JsonRpcResponse {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let req = JsonRpcRequest::new("1", method, params);
    let json = serde_json::to_string(&req).expect("serialize");
    write_half.write_all(json.as_bytes()).await.expect("write");
    write_half.write_all(b"\n").await.expect("write newline");

    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");
    serde_json::from_str(line.trim()).expect("parse response")
}

fn ok_result(resp: JsonRpcResponse, method: &str) -> serde_json::Value {
    match resp {
        JsonRpcResponse::Ok(o) => o.result,
        JsonRpcResponse::Err(e) => panic!("{method} failed: {e:?}"),
    }
}

#[tokio::test]
#[ignore = "requires ONESYNC_E2E_CLIENT_ID and ONESYNC_E2E_REMOTE_PATH plus interactive auth; opt in with --ignored"]
async fn end_to_end_against_real_onedrive_account() {
    let client_id = std::env::var("ONESYNC_E2E_CLIENT_ID")
        .expect("set ONESYNC_E2E_CLIENT_ID to your registered Azure AD client id");
    let remote_path =
        std::env::var("ONESYNC_E2E_REMOTE_PATH").expect("set ONESYNC_E2E_REMOTE_PATH");

    let (_token, sock_path, _tmp) = start_server().await;

    // 1. config.set — install the user's Azure AD client id so login_begin can read it.
    let set_resp = call(
        &sock_path,
        "config.set",
        serde_json::json!({ "azure_ad_client_id": client_id }),
    )
    .await;
    let _ = ok_result(set_resp, "config.set");

    // 2. account.login.begin — daemon binds the loopback listener and returns the auth URL.
    let begin_resp = call(&sock_path, "account.login.begin", serde_json::json!({})).await;
    let begin_val = ok_result(begin_resp, "account.login.begin");
    let auth_url = begin_val["auth_url"]
        .as_str()
        .expect("login.begin returns auth_url")
        .to_owned();
    let login_handle = begin_val["login_handle"]
        .as_str()
        .expect("login.begin returns login_handle")
        .to_owned();

    assert!(
        auth_url.contains("login.microsoftonline.com/common/oauth2/v2.0/authorize"),
        "auth_url must point at the v2 common authority: {auth_url}"
    );
    assert!(
        auth_url.contains("scope=Files.ReadWrite%20offline_access%20User.Read"),
        "auth_url must request the documented scope set: {auth_url}"
    );
    assert!(
        auth_url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A"),
        "auth_url must use the loopback redirect with an ephemeral port: {auth_url}"
    );

    eprintln!("M9/M12b end-to-end: open the following URL in a browser and sign in.");
    eprintln!("{auth_url}");
    eprintln!("Waiting for the OAuth redirect on the daemon's loopback listener...");

    // 3. account.login.await — blocks for the redirect, completes the code exchange.
    let await_resp = call(
        &sock_path,
        "account.login.await",
        serde_json::json!({ "login_handle": login_handle }),
    )
    .await;
    let account = ok_result(await_resp, "account.login.await");
    let account_id = account["id"]
        .as_str()
        .expect("account row carries an id")
        .to_owned();

    // 4. pair.add — connect a tempdir to the operator-supplied remote folder.
    let local_pair_dir = TempDir::new().expect("local pair tempdir");
    let local_pair_path = local_pair_dir
        .path()
        .to_str()
        .expect("utf8 path")
        .to_owned();
    let pair_resp = call(
        &sock_path,
        "pair.add",
        serde_json::json!({
            "account_id": account_id,
            "local_path": local_pair_path,
            "remote_path": remote_path,
        }),
    )
    .await;
    let pair = ok_result(pair_resp, "pair.add");
    assert_eq!(pair["status"], "initializing");

    // 5. pair.force_sync — kick the scheduler so the first cycle runs immediately.
    let pair_id = pair["id"].as_str().expect("pair id").to_owned();
    let _force = call(
        &sock_path,
        "pair.force_sync",
        serde_json::json!({ "id": pair_id }),
    )
    .await;

    // Give the daemon a moment to settle the first cycle; the assertions stop here so the
    // operator can inspect logs. Deeper coverage (file count, conflicts) lives in M10's
    // sandbox-account harness.
}
