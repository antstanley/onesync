//! `account.*` method handlers.

use onesync_core::ports::{IdGenerator, RefreshToken};
use onesync_graph::auth::{
    code_exchange::{self, TokenResponse},
    id_token, listener, pkce,
};
use onesync_graph::items;
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    id::{AccountId, AccountTag, AuditTag},
    primitives::{DriveId, KeychainRef},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::login_registry::LoginSession;

use super::{DispatchCtx, MethodError};

/// Authority used for the `common` Microsoft identity endpoint — accepts both personal MSA
/// and work/school AAD accounts and routes per-account at sign-in.
const COMMON_AUTHORITY: &str = "common";

/// Microsoft identity authorize endpoint base URL. Override via env in tests.
const MS_AUTHORIZE_BASE: &str = "https://login.microsoftonline.com";

/// OAuth scopes requested for delegated access.
const OAUTH_SCOPES: &str = "Files.ReadWrite offline_access User.Read";

#[derive(Debug, Default, Deserialize)]
struct LoginBeginParams {
    /// Optional account-type hint: `"personal"`, `"business"`, or omitted for `common`.
    /// Affects the authority URL only; the token endpoint understands all three.
    #[serde(default)]
    authority: Option<String>,
}

/// `account.login.begin` — start the OAuth PKCE flow.
///
/// Reads `azure_ad_client_id` from `InstanceConfig` (refuses if unset). Binds an ephemeral
/// loopback listener, generates the PKCE pair + state token, stashes a [`LoginSession`]
/// under a new `login_handle`, spawns a task to await the redirect, and returns
/// `{ login_handle, auth_url }`.
pub async fn login_begin(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: LoginBeginParams = if params.is_null() {
        LoginBeginParams::default()
    } else {
        serde_json::from_value(params.clone()).map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::INVALID_PARAMS,
                format!("invalid params: {e}"),
            )
        })?
    };

    // 1. Read the user-registered client id from InstanceConfig.
    let cfg = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let client_id = cfg
        .as_ref()
        .map(|c| c.azure_ad_client_id.clone())
        .unwrap_or_default();
    if client_id.is_empty() {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 10,
            "azure_ad_client_id is unset — register an Azure AD app and call config.set first \
             (see docs/install/ for the registration steps)",
        ));
    }

    // 2. Bind the loopback listener and derive the redirect URI.
    let (listener_sock, port) = listener::bind().await.map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INTERNAL_ERROR,
            format!("loopback bind failed: {e}"),
        )
    })?;
    let redirect_uri = format!("http://localhost:{port}/callback");

    // 3. Generate the PKCE pair and a random state token.
    let pkce_pair = pkce::generate();
    let state_token = ctx.ids.new_id::<AccountTag>().to_string();
    let login_handle = format!("lgn_{}", state_token.trim_start_matches("acct_"));

    // 4. Build the authorize URL.
    let authority = p.authority.unwrap_or_else(|| COMMON_AUTHORITY.to_owned());
    let auth_url = format!(
        "{MS_AUTHORIZE_BASE}/{authority}/oauth2/v2.0/authorize\
         ?client_id={client_id}\
         &response_type=code\
         &redirect_uri={redirect_uri}\
         &response_mode=query\
         &scope={scopes}\
         &state={state}\
         &code_challenge={challenge}\
         &code_challenge_method=S256",
        client_id = url_encode(&client_id),
        redirect_uri = url_encode(&redirect_uri),
        scopes = url_encode(OAUTH_SCOPES),
        state = state_token,
        challenge = pkce_pair.challenge,
    );

    // 5. Spawn the listener task; it sends the code through a oneshot.
    let (code_tx, code_rx) = tokio::sync::oneshot::channel();
    let expected_state = state_token;
    tokio::spawn(async move {
        let result = listener::await_code(
            listener_sock,
            &expected_state,
            onesync_core::limits::AUTH_LISTENER_TIMEOUT_S,
        )
        .await
        .map(|(code, _state)| code);
        let _ = code_tx.send(result);
    });

    // 6. Stash the session.
    ctx.login_registry.insert(
        login_handle.clone(),
        LoginSession {
            code_rx,
            pkce_verifier: pkce_pair.verifier,
            redirect_uri,
        },
    );

    Ok(json!({
        "login_handle": login_handle,
        "auth_url": auth_url,
    }))
}

#[derive(Debug, Deserialize)]
struct LoginAwaitParams {
    login_handle: String,
    /// Optional override for the token endpoint authority. Defaults to the `common` tenant.
    #[serde(default)]
    authority: Option<String>,
}

/// `account.login.await` — block until the redirect arrives, exchange the code, and persist
/// the new Account row + keychain refresh token.
#[allow(clippy::too_many_lines)]
// LINT: linear OAuth flow with seven explicit phases; splitting hurts readability.
pub async fn login_await(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: LoginAwaitParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;

    let session = ctx.login_registry.take(&p.login_handle).ok_or_else(|| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 11,
            format!("unknown login_handle: {}", p.login_handle),
        )
    })?;

    // 1. Wait for the loopback listener to deliver the auth code.
    let code = session
        .code_rx
        .await
        .map_err(|_| {
            MethodError::new(
                onesync_protocol::rpc::INTERNAL_ERROR,
                "login listener task dropped before delivering code".to_owned(),
            )
        })?
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 12,
                format!("OAuth redirect failed: {e}"),
            )
        })?;

    // 2. Look up the client id again (it could have been changed between begin and await,
    //    but it must still be set).
    let cfg = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let client_id = cfg
        .as_ref()
        .map(|c| c.azure_ad_client_id.clone())
        .unwrap_or_default();
    if client_id.is_empty() {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 10,
            "azure_ad_client_id is unset",
        ));
    }

    // 3. Exchange the code for tokens.
    let authority = p.authority.unwrap_or_else(|| COMMON_AUTHORITY.to_owned());
    let tokens: TokenResponse = code_exchange::exchange(
        &ctx.http,
        &authority,
        &client_id,
        &code,
        &session.redirect_uri,
        &session.pkce_verifier,
    )
    .await
    .map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 13,
            format!("token exchange failed: {e}"),
        )
    })?;

    // 4. Parse the id_token to obtain tid/oid/upn/display_name/kind.
    let claims = id_token::parse(&tokens.id_token).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 14,
            format!("id_token parse failed: {e}"),
        )
    })?;

    // 5. Fetch /me/drive to get the OneDrive drive id.
    let drive = items::default_drive(&ctx.http, &tokens.access_token)
        .await
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 15,
                format!("/me/drive lookup failed: {e}"),
            )
        })?;

    // 6. Build the Account row and persist the refresh token in the keychain.
    let account_id: AccountId = ctx.ids.new_id::<AccountTag>();
    let refresh = RefreshToken(tokens.refresh_token);
    let keychain_ref: KeychainRef = ctx
        .vault
        .store_refresh(&account_id, &refresh)
        .await
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 16,
                format!("keychain store failed: {e}"),
            )
        })?;

    let now = ctx.clock.now();
    let account = Account {
        id: account_id,
        kind: claims.kind,
        upn: claims.upn,
        tenant_id: claims.tid,
        drive_id: DriveId::new(drive.id),
        display_name: claims.display_name,
        keychain_ref,
        scopes: tokens.scope.split_whitespace().map(str::to_owned).collect(),
        created_at: now,
        updated_at: now,
    };
    ctx.state
        .account_upsert(&account)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;

    // 7. Audit.
    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: now,
        level: onesync_protocol::enums::AuditLevel::Info,
        kind: "account.login".to_owned(),
        pair_id: None,
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "account_id".to_owned(),
                Value::String(account.id.to_string()),
            );
            m.insert("upn".to_owned(), Value::String(account.upn.clone()));
            m
        },
    };
    let _ = ctx.state.audit_append(&evt).await;
    ctx.audit.emit(evt);

    Ok(serde_json::to_value(account).unwrap_or(Value::Null))
}

/// `account.list` — return all linked accounts.
pub async fn list(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let accts = ctx
        .state
        .accounts_list()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(accts).unwrap_or(Value::Null))
}

#[derive(Debug, Deserialize)]
struct AccountByIdParams {
    id: AccountId,
}

/// `account.get` — fetch one account by id.
pub async fn get(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AccountByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let acct = ctx
        .state
        .account_get(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    match acct {
        Some(a) => Ok(serde_json::to_value(a).unwrap_or(Value::Null)),
        None => Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 1,
            format!("account not found: {}", p.id),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct AddSharePointParams {
    /// Existing `acct_<ulid>` whose refresh token has `SharePoint` scope. The new Account row
    /// re-uses this account's keychain entry; the only thing that differs is the drive id.
    base_account_id: AccountId,
    /// `SharePoint` host, e.g. `contoso.sharepoint.com`.
    host: String,
    /// Site path under `/sites/`, e.g. `sales-team`.
    site_path: String,
    /// Document-library display name, e.g. `Documents` or `Reports`.
    library_name: String,
}

/// `account.add_sharepoint` — resolve a `SharePoint` document library to a `DriveId` and mint
/// a new `Account` row pointing at it. Per the M11 decision in `04-onedrive-adapter.md`.
///
/// Pre-conditions:
/// - `base_account_id` is an existing Account with a valid keychain refresh token whose
///   scopes include `SharePoint` access (e.g. `Files.ReadWrite.All`).
/// - The user has read access to the target site + library.
///
/// The new Account record shares the keychain ref of the base account (no second login
/// required); only its `drive_id` differs. Each pair under the new account targets the
/// `SharePoint` library exactly the way `/me/drive` pairs target personal storage.
#[allow(clippy::too_many_lines)]
// LINT: linear resolve → mint → audit; splitting hurts readability.
pub async fn add_sharepoint(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AddSharePointParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let base = ctx
        .state
        .account_get(&p.base_account_id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 40,
                format!("base account not found: {}", p.base_account_id),
            )
        })?;

    // Refresh the access token using the base account's keychain entry.
    let cfg = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let client_id = cfg
        .as_ref()
        .map(|c| c.azure_ad_client_id.clone())
        .unwrap_or_default();
    if client_id.is_empty() {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 10,
            "azure_ad_client_id is unset",
        ));
    }
    let RefreshToken(rt) = ctx.vault.load_refresh(&base.id).await.map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 22,
            format!("keychain load failed: {e}"),
        )
    })?;
    let tokens = onesync_graph::auth::refresh::refresh(&ctx.http, "common", &client_id, &rt)
        .await
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 23,
                format!("token refresh failed: {e}"),
            )
        })?;
    let _ = ctx
        .vault
        .store_refresh(&base.id, &RefreshToken(tokens.refresh_token))
        .await;

    // Resolve the site, then the library.
    let site = items::site_by_path(&ctx.http, &tokens.access_token, &p.host, &p.site_path)
        .await
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 41,
                format!("site resolve failed: {e}"),
            )
        })?;
    let drive =
        items::site_library_by_name(&ctx.http, &tokens.access_token, &site.id, &p.library_name)
            .await
            .map_err(|e| {
                MethodError::new(
                    onesync_protocol::rpc::APP_ERROR_BASE - 42,
                    format!(
                        "library resolve failed for '{}' on site {}: {e}",
                        p.library_name, site.id
                    ),
                )
            })?;

    // Mint a new Account row reusing the base account's keychain ref.
    let now = ctx.clock.now();
    let new_id: AccountId = ctx.ids.new_id::<AccountTag>();
    let display = format!(
        "{} / {} / {}",
        site.display_name
            .clone()
            .unwrap_or_else(|| p.site_path.clone()),
        p.library_name,
        base.display_name
    );
    let account = Account {
        id: new_id,
        kind: onesync_protocol::enums::AccountKind::Business,
        upn: base.upn.clone(),
        tenant_id: base.tenant_id.clone(),
        drive_id: DriveId::new(drive.id),
        display_name: display,
        keychain_ref: base.keychain_ref.clone(),
        scopes: base.scopes.clone(),
        created_at: now,
        updated_at: now,
    };
    ctx.state
        .account_upsert(&account)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;

    // Audit
    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: now,
        level: onesync_protocol::enums::AuditLevel::Info,
        kind: "account.added_sharepoint".to_owned(),
        pair_id: None,
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "account_id".to_owned(),
                Value::String(account.id.to_string()),
            );
            m.insert("host".to_owned(), Value::String(p.host));
            m.insert("site_path".to_owned(), Value::String(p.site_path));
            m.insert("library_name".to_owned(), Value::String(p.library_name));
            m
        },
    };
    let _ = ctx.state.audit_append(&evt).await;
    ctx.audit.emit(evt);

    Ok(serde_json::to_value(account).unwrap_or(Value::Null))
}

/// `account.remove` — unlink an account, delete the keychain entry, cascade-remove pairs.
pub async fn remove(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AccountByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let _ = ctx.vault.delete(&p.id).await;
    ctx.state
        .account_remove(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "id": p.id.to_string() }))
}

#[allow(clippy::missing_const_for_fn)]
// LINT: cannot be const because String allocations and write! macros aren't const-stable.
/// Minimal RFC 3986 percent-encoding for URL query-string values.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(*b));
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
