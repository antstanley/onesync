# 04 — OneDrive Adapter

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

The OneDrive adapter implements the `RemoteDrive` port against Microsoft Graph. It handles
authentication for both consumer (OneDrive Personal) and work/school (OneDrive for Business)
accounts in a single binary, manages access and refresh tokens via the keychain adapter,
issues delta queries, performs file uploads, downloads, renames, and deletes, and translates
Graph's wire types into the engine's `RemoteItem` / `FileSide` shape.

The crate is `onesync-graph`. Its single dependency on external HTTP code is `reqwest` (with
`rustls-tls` only — no OpenSSL). MSAL is implemented in-crate as a thin authorisation-code +
refresh-token client; we do not depend on Microsoft's MSAL-Rust because it is unmaintained.

---

## Responsibilities

1. Acquire and refresh access tokens for the Microsoft identity platform using OAuth 2.0
   authorisation code with PKCE, suitable for both Personal and Business accounts.
2. Identify the account kind on first sign-in and persist it in `Account.kind`.
3. Issue Graph requests against `/me/drive` with bounded retries and 429-aware throttling.
4. Page `/me/drive/items/{id}/delta` calls and return `DeltaPage` records to the engine.
5. Upload files of any size up to `MAX_FILE_SIZE_BYTES`, using small-upload for
   ≤ `GRAPH_SMALL_UPLOAD_MAX_BYTES` and upload sessions for larger.
6. Stream downloads to disk via a `RemoteReadStream`.
7. Map Graph errors to the port-level `GraphError` enum, surfacing throttling (`Retry-After`),
   resync requirements, and authentication failures distinctly.

---

## Authentication

### Identity authority

The adapter uses the `https://login.microsoftonline.com/consumers + organizations` authority
URL (`/common/oauth2/v2.0/…` historically) so the same client can sign in users of either
flavour. The token endpoint returns claims that include `tid` (tenant id) and `idp` (identity
provider); these drive `AccountKind` selection:

| Token claim | Account kind |
|---|---|
| `tid = 9188040d-6c67-4c5b-b112-36a304b66dad` | `Personal` (consumer MSA tenant) |
| `tid = <any other GUID>` | `Business` |

This is the only difference in handling between the two flavours. Both flow through
`/me/drive` from the moment a token is issued.

### Application registration

The adapter expects a single `client_id`, a redirect URI of `http://localhost:<port>/callback`
(bound on an ephemeral port at sign-in time), and the scopes
`Files.ReadWrite offline_access User.Read`. Whether the project ships a pre-registered
`client_id` or asks the user to bring their own is an
[open question](#assumptions-and-open-questions); the code path accepts either.

### Flow

```
1. CLI runs:   onesync account login [--client-id <id>]
2. Daemon:
   - Generates PKCE verifier + challenge.
   - Binds a one-shot loopback listener on a free TCP port.
   - Builds the authorisation URL with scope, redirect, code_challenge, code_challenge_method=S256.
3. CLI opens the URL in the user's default browser via `open(1)`.
4. User authenticates with Microsoft and consents to scopes.
5. Microsoft redirects to http://localhost:<port>/callback?code=…&state=…
6. Daemon's loopback listener captures the code, verifies `state`.
7. Daemon exchanges code + PKCE verifier for { access_token, refresh_token, id_token }.
8. Daemon decodes id_token claims to derive AccountKind, upn, tenant_id.
9. Daemon calls `/me` to fetch drive_id and display_name.
10. Daemon writes Account row to the state store and refresh_token to the keychain.
```

Steps 2–6 are bounded by `AUTH_LISTENER_TIMEOUT_S` (default 300). If the listener times out
the daemon tears it down and surfaces a structured error to the CLI.

The browser is launched by the CLI, not the daemon, so the daemon never invokes external
processes. The CLI receives the auth URL over the IPC `account_login` RPC and uses
`/usr/bin/open` to launch it.

### Token lifecycle

- Access tokens are kept in memory only, never persisted.
- Access tokens are refreshed on demand: every Graph call goes through
  `EnsureFreshToken(account_id)` which returns a token valid for at least
  `TOKEN_REFRESH_LEEWAY_S` more seconds.
- Refresh tokens live in the macOS Keychain via `TokenVault`. The keychain entry's service
  name is `dev.onesync.refresh-token` and the account name is the `AccountId` literal.
- On HTTP 401 with `WWW-Authenticate: Bearer error="invalid_token"`, the adapter forces a
  refresh and retries the request exactly once. Subsequent 401 is treated as
  `GraphError::Unauthorized` and surfaces to the engine, which transitions the pair to
  `Errored("auth")`.
- On HTTP 400 `invalid_grant` from the refresh endpoint, the adapter deletes the keychain
  entry, clears the in-memory token cache for that account, and surfaces
  `GraphError::ReAuthRequired`.

### Sign-out

`onesync account remove <acct_…>` removes the keychain entry, deletes the account row
(cascade-deletes pairs after confirmation), and posts a best-effort revoke to
`/oauth2/v2.0/logout`. A failed revoke is logged but not surfaced as an error; the local
state is the source of truth.

---

## Graph endpoints used

| Purpose | Endpoint | Notes |
|---|---|---|
| User profile | `GET /me` | once per sign-in, to populate `Account` |
| Drive metadata | `GET /me/drive` | once per sign-in, for `drive_id` |
| Resolve folder by path | `GET /me/drive/root:/{path}` | used when registering a `Pair` |
| Initial delta | `GET /me/drive/items/{id}/delta` | first call has no `?token=` |
| Subsequent delta | `GET …/delta?token={cursor}` | cursor opaque |
| Download | `GET /me/drive/items/{id}/content` | follows 302 to the storage URL |
| Small upload | `PUT /me/drive/items/{parent}:/{name}:/content` | size ≤ `GRAPH_SMALL_UPLOAD_MAX_BYTES` (4 MiB) |
| Upload session | `POST /me/drive/items/{parent}:/{name}:/createUploadSession` | for larger files |
| Upload chunk | `PUT {uploadUrl}` with `Content-Range` | chunks are multiples of 320 KiB; `SESSION_CHUNK_BYTES` = 10 MiB |
| Rename | `PATCH /me/drive/items/{id}` with `{ "name": "…" }` | |
| Delete | `DELETE /me/drive/items/{id}` | moves to OneDrive Recycle Bin |
| Mkdir | `POST /me/drive/items/{parent}/children` with folder facet | conflict behaviour `fail` |
| Webhook (optional) | `POST /subscriptions` | only when remote-webhook is enabled |

All requests carry the `Authorization: Bearer …` header. No request body is logged; URL
paths are logged with the item id and parent id only.

---

## Delta queries

The delta endpoint returns batches of `driveItem` resources. The adapter:

1. Sends the request with the stored cursor in `Pair.delta_token`, or no cursor on the first
   call.
2. Streams the response body and yields each `driveItem` as a `DeltaPage::Item`.
3. Follows `@odata.nextLink` to fetch subsequent pages until the response carries
   `@odata.deltaLink`.
4. Returns the `deltaLink`'s `token` to the engine as the new cursor.

The adapter does not interpret `deleted` items: a `driveItem` with the `deleted` facet
produces a `DeltaPage::Item` whose `RemoteItem.deleted` flag is set, and the engine handles
the deletion logic in reconciliation.

`resyncRequired` is returned by the server when the cursor is too old. The adapter surfaces it
as `GraphError::ResyncRequired`; the engine drops the cursor and re-initialises.

---

## Uploads

### Small files

For files at or below `GRAPH_SMALL_UPLOAD_MAX_BYTES` (4 MiB), the adapter issues a single
`PUT` with the file bytes as the body. The response is parsed into a `RemoteItem`; its
`eTag`, `cTag`, `size`, `lastModifiedDateTime`, and `file.hashes` populate the new
`FileSide.remote`.

### Upload sessions

For larger files, the adapter:

1. `POST`s `createUploadSession` with `item.@microsoft.graph.conflictBehavior = "replace"`.
2. Streams chunks of `SESSION_CHUNK_BYTES` (10 MiB; multiple of 320 KiB as required by Graph)
   with `Content-Range: bytes {start}-{end}/{total}`.
3. Tracks the returned `nextExpectedRanges` to validate progress and resume after partial
   failure. The session URL is persisted in `FileOp.metadata` so a daemon restart can resume
   without re-uploading.
4. Treats a 416 `Requested Range Not Satisfiable` as "ask the server which ranges it still
   needs"; re-fetches the session, and continues.
5. Treats 5xx and 429 as retryable per the engine's [retry policy](03-sync-engine.md#retry).

`MAX_FILE_SIZE_BYTES` is enforced before opening a session; the adapter refuses to begin an
upload that exceeds the cap.

### Conflict behaviour at upload

We always use `replace` because the engine has already decided that the local content
should overwrite the remote. The keep-both conflict policy operates one level higher; by the
time an upload reaches the adapter, the conflict (if any) is already resolved into a rename.

---

## Downloads

`GET /me/drive/items/{id}/content` returns 302 to a storage URL. The adapter follows the
redirect with no auth header (storage URLs are presigned) and streams the body. The bytes are
piped through `LocalFs::write_atomic` so the file does not appear partial at the destination
path during a crash.

Hash verification on download: when the server's `file.hashes` includes a `sha1Hash`
(Personal) or `quickXorHash` (Business), the adapter computes the same hash while streaming
and rejects the download if it does not match. A mismatch is `GraphError::HashMismatch`,
which is retryable (likely a corrupted transfer).

---

## Renames, deletes, mkdir

These are direct API calls and the wire shape matches the port's parameters one-to-one. The
only nuance is that `DELETE` is **destructive on the loser-rename path of conflict resolution
only if** the loser-rename has been successfully reflected on the local side first; the
engine sequences the calls, the adapter does not need to reason about safety here.

For directory creation we set `@microsoft.graph.conflictBehavior=fail` so the call returns
the existing folder if one already lives at the target name; the engine then promotes the
existing item rather than creating a duplicate.

---

## Throttling

Microsoft Graph returns `Retry-After: <seconds>` on 429 and on 503 when throttling. The
adapter:

- Honours `Retry-After` strictly; the engine receives `GraphError::Throttled { retry_after }`
  and reschedules the cycle.
- Maintains a per-account token bucket of `GRAPH_RPS_PER_ACCOUNT` (default 8 rps) requests
  per second to stay well under the documented limits and avoid being throttled in the first
  place.
- Tags every outbound request with a `client-request-id` UUID and logs the corresponding
  `request-id` and `client-request-id` from the response in audit events for tracing in
  Graph Explorer / Azure logs.

---

## Error mapping

`GraphError` is the port-level error. Each variant carries the original `request-id` for
traceability.

| HTTP / cause | `GraphError` variant | Engine treatment |
|---|---|---|
| 401 invalid_token | `Unauthorized` (after one refresh attempt) | Pair → `Errored("auth")` |
| 401 invalid_grant on refresh | `ReAuthRequired` | Pair → `Errored("auth")`; CLI prompts re-login |
| 403 access_denied | `Forbidden` | Pair → `Errored("permission")` |
| 404 itemNotFound | `NotFound` | Engine deletes local mirror if appropriate |
| 409 nameAlreadyExists | `NameConflict` | Engine treats as concurrent create; runs conflict policy |
| 410 resyncRequired | `ResyncRequired` | Cursor reset, full re-scan |
| 412 preconditionFailed | `Stale { server_etag }` | Engine re-reads, re-plans |
| 416 invalidRange | `InvalidRange` | Adapter handles internally on upload session resume |
| 429 / 503 with Retry-After | `Throttled { retry_after }` | Pair-wide pause until deadline |
| 5xx without Retry-After | `Transient` | Standard backoff retry |
| Network / DNS / TLS | `Network { source }` | Standard backoff retry |
| Body decode failure | `Decode { detail }` | Standard backoff retry (treated as transient) |
| Hash mismatch on download | `HashMismatch` | Standard backoff retry |
| File too large at upload-session open | `TooLarge` | Op moves to `Failed`, surfaced; non-retryable |

`GraphError` deliberately does not carry the original `reqwest::Error`; it carries a
serialisable detail string and a typed cause kind, so it can cross crate boundaries without
leaking adapter internals.

---

## Webhooks (sketched, not enabled)

The adapter declares a `subscribe_webhook(parent_item_id, callback_url) -> SubscriptionId`
method but the default build does not register webhooks. The receiver URL story is
unresolved (see open questions); the polling path is sufficient for correctness and webhooks
are a latency optimisation only.

---

## Assumptions and open questions

**Assumptions**

- The Microsoft identity platform's `consumers + organizations` authority will continue to
  unify both account types behind one OAuth flow.
- `/me/drive` is the right endpoint for both flavours; SharePoint document libraries via
  `/sites/{site}/drives/{drive}` are scoped for a future milestone (see Decision:
  *SharePoint document libraries*).
- Graph's documented small-upload limit of 4 MiB is stable enough to bake into a `const`.
  Microsoft has raised this in the past; we re-check at major-version bumps.

**Decisions**

- *Single-binary multi-flavour at the Graph layer.* **Both account kinds route through the
  same code; only `Account.kind` differs.** Branching exists only in user-facing labels and
  in which hash field (`sha1Hash` vs `quickXorHash`) we cross-check on download.
- *In-crate MSAL.* **No dependency on Microsoft's `msal` crate.** The required flow
  (auth-code + PKCE + refresh) is small and well-specified; the existing `msal` crate is
  unmaintained and pulls in OpenSSL.
- *`rustls-tls` only.* **No system OpenSSL.** Keeps the build hermetic and `cargo build`
  reproducible across macOS versions.
- *Refresh tokens in keychain only.* **Never persisted to the SQLite database.** Compromising
  the state file does not yield credentials.
- *Azure AD client registration.* **The user registers their own Azure AD app; onesync does
  not ship a project-owned multi-tenant client ID.** The install docs walk the user through
  the registration form, listing the redirect URI (`http://localhost:<port>/callback`), the
  required delegated scopes (`Files.ReadWrite.All offline_access User.Read`), and the
  supported-account-types selector. Distributes the per-app rate limits across users and
  removes a project-level tenant-ownership liability. Cross-referenced from
  [`00-overview.md`](00-overview.md).
- *Webhook receiver via Cloudflare Tunnel.* **The `/subscriptions` callback URL is terminated
  by a `cloudflared` tunnel the operator runs; webhooks are opt-in and off by default.** The
  install docs include a sample `cloudflared` config; the daemon exposes the receiver on a
  local port and the tunnel maps a stable HTTPS URL to it. Polling via `/delta` remains the
  always-on fallback so a flaky tunnel does not break correctness, only latency.
- *SharePoint document libraries.* **In scope for a future milestone (M9+).** The engine and
  Graph adapter route through `/me/drive` today; SharePoint requires resolving a target via
  `/sites/{site}/drives/{drive}` plus a selector syntax in `pair add` (e.g.
  `--site contoso.sharepoint.com --library Documents`). The schema does not need new entities
  because a SharePoint drive maps to a `DriveId` plus a `DriveItemId`; only the Graph adapter
  and the `pair add` flow need to learn the resolution step.

**Open questions**

- (None at this stage.)
