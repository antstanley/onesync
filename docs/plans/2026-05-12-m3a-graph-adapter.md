# onesync M3a — Graph Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax. The workspace for this milestone is `/Volumes/Delorean/onesync-m3a-graph/` (created via `jj workspace add`). All commits use `jj describe -m "..."` + `jj new`. **Never invoke `git` directly.**

**Goal:** Build `onesync-graph` — the Microsoft Graph adapter that implements the `RemoteDrive` port, handles OAuth (auth-code + PKCE + refresh) against the `consumers + organizations` authority so a single binary supports both OneDrive Personal and Business, pages `/delta`, uploads via single-PUT or upload sessions, downloads with hash verification, and maps Graph errors to the typed `GraphError` enum.

**Architecture:** A single crate under `crates/onesync-graph/`. Uses `reqwest` with `rustls-tls` only (no OpenSSL). The MSAL flow is in-crate — we do NOT depend on Microsoft's unmaintained `msal` crate. The crate is `unsafe`-free.

**Tech Stack:** `reqwest` 0.12 with `rustls-tls`, `tokio`, `serde`, `serde_json`, `url`, `base64`, `sha2`, `wiremock` (dev), plus existing workspace deps.

VCS: jj-colocated. Per-task commits inside the workspace; the workspace's `@` advances as tasks land. Trailer: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` verbatim on every commit. Author identity comes from jj config (`Ant Stanley`).

**Parallel milestone:** M3b (`onesync-keychain`) runs simultaneously in `../onesync-m3b-keychain/`. The two crates share no source files; only the workspace `Cargo.toml` dependency table needs coordination, which we resolve by landing M3a's workspace deps first and then M3b on top.

---

## Pre-flight (read before starting)

- M1 + M2 are complete; `origin/main` is at `bab03f67`. Workspace test count: 125.
- This plan executes inside the jj workspace `/Volumes/Delorean/onesync-m3a-graph/` (named `m3a-graph`). The main checkout at `/Users/stan/code/onesync/` is the M3b workspace's neighbour.
- Microsoft Graph endpoints used: `/me`, `/me/drive`, `/me/drive/root:/{path}`, `/me/drive/items/{id}/delta`, `/me/drive/items/{id}/content`, `/me/drive/items/{parent}:/{name}:/content`, `/me/drive/items/{parent}:/{name}:/createUploadSession`, `/me/drive/items/{id}` (PATCH/DELETE), `/me/drive/items/{parent}/children`.
- OAuth authority: `https://login.microsoftonline.com/consumers` for Personal, `…/organizations` for Business; combined via `…/common` only in flows that don't need tenant disambiguation. We use the `common` endpoint with explicit `tid` parsing from the `id_token` to assign `AccountKind`.
- Spec: [`docs/spec/04-onedrive-adapter.md`](../spec/04-onedrive-adapter.md). Auth flow described in §Flow; error mapping in §Error mapping.

---

## File map (M3a creates)

```
crates/onesync-graph/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── auth/
    │   ├── mod.rs
    │   ├── pkce.rs              # verifier/challenge per RFC 7636
    │   ├── code_exchange.rs     # POST /token (authorization_code grant)
    │   ├── refresh.rs           # POST /token (refresh_token grant)
    │   ├── id_token.rs          # parse JWT claims (no signature check — token comes from the IdP we just authenticated against)
    │   └── listener.rs          # loopback HTTP listener for the redirect URI
    ├── client.rs                # reqwest::Client builder, common request shape, EnsureFreshToken
    ├── throttle.rs              # per-account token bucket
    ├── error.rs                 # GraphInternalError → port-level GraphError
    ├── items.rs                 # GET /me/drive/items/* probe helpers
    ├── delta.rs                 # /delta pager
    ├── download.rs              # streaming download + hash verify
    ├── upload/
    │   ├── mod.rs
    │   ├── small.rs             # single-PUT for ≤ 4 MiB
    │   └── session.rs           # createUploadSession + chunked PUT with resume
    ├── ops.rs                   # rename, delete, mkdir
    ├── adapter.rs               # RemoteDrive impl
    └── fakes.rs                 # mock-backed fake RemoteDrive for engine tests
```

18 tasks total: 17 implementation + 1 close. Workspace test count target: 125 (entry) → ≥ 200 (exit).

---

## Task 1: Crate skeleton + workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`) — add:
  ```toml
  reqwest    = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
  url        = "2"
  base64     = "0.22"
  sha2       = "0.10"
  wiremock   = "0.6"
  futures    = "0.3"
  bytes      = "1"
  ```
- Create: `crates/onesync-graph/Cargo.toml` inheriting workspace deps + `onesync-core`, `onesync-protocol`, `async-trait`, `chrono`, `reqwest`, `url`, `base64`, `sha2`, `serde`, `serde_json`, `thiserror`, `tokio`, `futures`, `bytes`; dev-deps `wiremock`, `tempfile`, `proptest`.
- Create: `src/lib.rs` with `#![forbid(unsafe_code)]`, `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`, declare all modules, re-export `GraphAdapter` + `GraphAdapterError`.
- Create: minimal stub for each module (doc comment + placeholder struct/fn where needed so `cargo check` parses).

**Gate:** `cargo check -p onesync-graph`; workspace tests still 125; clippy + fmt clean.
**Commit:** `feat(graph): onesync-graph crate skeleton`

---

## Task 2: HTTP client + EnsureFreshToken scaffold

**Files:** `src/client.rs`

Implement:
```rust
pub struct GraphClient {
    http: reqwest::Client,
    token_cache: tokio::sync::Mutex<HashMap<AccountId, CachedToken>>,
    throttle: throttle::Bucket,
}

struct CachedToken { access_token: String, expires_at: Timestamp }

impl GraphClient {
    pub fn new() -> Self { ... }
    pub async fn ensure_fresh_token(&self, account: &AccountId, refresh_token: &str) -> Result<String, GraphInternalError>;
    pub async fn graph_get<T: DeserializeOwned>(&self, url: &str, token: &str) -> Result<T, GraphInternalError>;
    pub async fn graph_post_json<B: Serialize, T: DeserializeOwned>(&self, url: &str, token: &str, body: &B) -> Result<T, GraphInternalError>;
    // common request shape: Authorization: Bearer + client-request-id + 401-once-retry-after-refresh + map HTTP status → GraphInternalError
}
```

The `ensure_fresh_token` refreshes if `expires_at - now < TOKEN_REFRESH_LEEWAY_S` (from `onesync_core::limits`).

**Tests:** unit tests with `wiremock` covering: 200 OK happy path; 401 once → refresh → retry → 200; 401 twice → returns `Unauthorized`; 429 with `Retry-After` → returns `Throttled { retry_after_s }`; 5xx → `Transient`.

**Gate + commit:** `feat(graph): HTTP client with token cache and 401-once retry`

---

## Task 3: OAuth PKCE module

**Files:** `src/auth/pkce.rs`

```rust
pub struct PkcePair { pub verifier: String, pub challenge: String }
pub fn generate() -> PkcePair {
    // 32 bytes from getrandom → base64url-no-padding (43 chars) = verifier
    // SHA-256(verifier.as_bytes()) → base64url-no-padding = challenge
}
```

Uses `sha2::Sha256` + `base64::engine::general_purpose::URL_SAFE_NO_PAD` + `getrandom` (transitive via `rand` already in tree? if not, pin in workspace deps).

**Tests:** RFC 7636 §4 conformance — verifier matches `[A-Za-z0-9_~.-]{43,128}`; challenge length 43; round-trip a hand-known verifier `"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"` → expected challenge `"E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"` (RFC 7636 Appendix B).

**Gate + commit:** `feat(graph): PKCE verifier/challenge per RFC 7636`

---

## Task 4: ID token claim parser

**Files:** `src/auth/id_token.rs`

Decode the middle segment of the `id_token` (a JWT: `header.payload.signature`). Parse `payload` as JSON; extract `tid`, `oid`, `preferred_username` (or `upn`), `name`. Compare `tid` against `9188040d-6c67-4c5b-b112-36a304b66dad` (Microsoft consumer-MSA tenant) to derive `AccountKind`.

```rust
pub struct IdTokenClaims { pub tid: String, pub oid: String, pub upn: String, pub display_name: String, pub kind: AccountKind }
pub fn parse(id_token: &str) -> Result<IdTokenClaims, GraphInternalError>;
```

No signature verification — the token came from the OAuth response we just received over TLS from the IdP. We trust the channel, not the payload's signature (which would require fetching JWKS).

**Tests:** decode three fixture JWTs (Personal, Business, malformed). Personal fixture has `tid = 9188040d-...`; assertion: `kind == AccountKind::Personal`.

**Gate + commit:** `feat(graph): id_token claim parser for AccountKind dispatch`

---

## Task 5: Loopback redirect listener

**Files:** `src/auth/listener.rs`

A one-shot `TcpListener` on `127.0.0.1:0` (ephemeral port). Returns `(listener, port)`. `await_code(listener, state) -> Result<(String /* code */, String /* state */), GraphInternalError>` accepts one HTTP request, parses `?code=…&state=…`, verifies `state` matches the value we sent, returns `(code, state)`. Times out after `AUTH_LISTENER_TIMEOUT_S` (300).

**Tests:** spin up the listener; send a hand-crafted HTTP request via `tokio::net::TcpStream`; assert the parsed code matches. Timeout test: don't send anything for 1 s past a shortened timeout fixture; assert `Timeout` error.

**Gate + commit:** `feat(graph): loopback listener for OAuth redirect`

---

## Task 6: Authorization-code exchange

**Files:** `src/auth/code_exchange.rs`

```rust
pub async fn exchange(
    http: &reqwest::Client,
    authority: &str,                    // "consumers" | "organizations" | "common"
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    pkce_verifier: &str,
) -> Result<TokenResponse, GraphInternalError>;

pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: u64,            // seconds
    pub scope: String,
}
```

POSTs `client_id=… code=… code_verifier=… grant_type=authorization_code redirect_uri=…` as `application/x-www-form-urlencoded` to `https://login.microsoftonline.com/{authority}/oauth2/v2.0/token`.

**Tests:** wiremock 200 OK with a JSON token response; assert struct populated. 400 `invalid_grant` → `GraphInternalError::ReAuthRequired`.

**Gate + commit:** `feat(graph): authorization-code grant + token-response parse`

---

## Task 7: Refresh-token exchange

**Files:** `src/auth/refresh.rs`

Same shape as code_exchange but `grant_type=refresh_token&refresh_token=…`. Returns `TokenResponse` (note that Microsoft rotates refresh tokens; the response carries a new `refresh_token` which the caller persists).

**Tests:** 200 happy path; 400 `invalid_grant` → `ReAuthRequired` (token revoked / user changed password).

**Gate + commit:** `feat(graph): refresh-token grant`

---

## Task 8: Account profile + drive id

**Files:** `src/items.rs`

```rust
pub async fn account_profile(http: &reqwest::Client, token: &str) -> Result<AccountProfileDto, GraphInternalError>;
pub async fn default_drive(http: &reqwest::Client, token: &str) -> Result<DriveDto, GraphInternalError>;
pub async fn item_by_path(http: &reqwest::Client, token: &str, drive_id: &DriveId, path: &str) -> Result<Option<RemoteItem>, GraphInternalError>;
```

`AccountProfileDto { id: String, user_principal_name: String, display_name: String }`. `DriveDto { id: String, drive_type: String }`. `RemoteItem` carries `id`, `name`, `size`, `eTag`, `cTag`, `lastModifiedDateTime`, `folder: Option<FolderFacet>`, `file: Option<FileFacet>` (with `hashes: { sha1Hash?, quickXorHash? }`).

**Tests:** wiremock fixtures for `GET /me`, `GET /me/drive`, `GET /me/drive/root:/Documents/notes.md` → 200 + RemoteItem. `GET …/Notexist` → 404 → `Ok(None)`.

**Gate + commit:** `feat(graph): /me, /me/drive, and item-by-path probes`

---

## Task 9: Delta pager

**Files:** `src/delta.rs`

```rust
pub async fn delta_page(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    cursor: Option<&DeltaCursor>,
) -> Result<DeltaPage, GraphInternalError>;

pub struct DeltaPage {
    pub items: Vec<RemoteItem>,
    pub next_link: Option<String>,
    pub delta_token: Option<DeltaCursor>, // present on the final page only
}
```

URL: `https://graph.microsoft.com/v1.0/drives/{drive_id}/root/delta` with optional `?token=…`. Pagination via `@odata.nextLink`; terminal page has `@odata.deltaLink` (extract the `token=` query parameter).

`resyncRequired` server error → `GraphInternalError::ResyncRequired`.

**Tests:** multi-page wiremock fixtures (page 1 → page 2 → terminal). Tombstone items (`deleted` facet) are returned with `RemoteItem.deleted = true`.

**Gate + commit:** `feat(graph): /delta pager with nextLink and deltaLink`

---

## Task 10: Streaming download + hash verification

**Files:** `src/download.rs`

```rust
pub async fn download(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    item_id: &str,
    expected: Option<&FileHashes>,    // sha1Hash / quickXorHash from the /delta response
) -> Result<bytes::Bytes, GraphInternalError>;
```

`GET /me/drive/items/{id}/content` returns a 302 redirect to a pre-signed storage URL. Follow with no auth header. Stream the body in `HASH_BLOCK_BYTES` (1 MiB) chunks via `reqwest::Response::chunk()`. Compute `sha1` (Personal) or `quickXorHash` (Business) as we stream. On mismatch → `HashMismatch`.

QuickXorHash is a custom Microsoft algorithm — implement per [MS-XCDRREF §4.0](https://learn.microsoft.com/en-us/onedrive/developer/code-snippets/quickxorhash) (it's a rolling XOR shift; ~30 LOC).

For Task 10, accept just `sha1Hash` verification; add a TODO marker for `quickXorHash` and land that in a follow-up. The plan's spec page 04 calls out both — but they're independent and either alone is acceptable for the gate.

Actually no — let me do them both now since they're already specified. Implement both, gate on both.

**Tests:** wiremock fixture serving a known-content file; assert SHA1 matches the expected `sha1Hash`. Mismatch test: serve different content; assert `HashMismatch`.

**Gate + commit:** `feat(graph): streaming download with sha1/quickXor verification`

---

## Task 11: Small upload (single PUT)

**Files:** `src/upload/small.rs`

```rust
pub async fn upload_small(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    parent_item_id: &str,
    name: &str,
    bytes: &[u8],
) -> Result<RemoteItem, GraphInternalError>;
```

`PUT /me/drive/items/{parent}:/{name}:/content?@microsoft.graph.conflictBehavior=replace`. Body is raw bytes (`Content-Type: application/octet-stream`). Enforce `bytes.len() <= GRAPH_SMALL_UPLOAD_MAX_BYTES` (4 MiB) — over → `InvalidArgument` (or just route to upload session in the adapter; the helper here is small-only).

**Tests:** wiremock fixture; assert RemoteItem parsed back.

**Gate + commit:** `feat(graph): single-PUT small upload`

---

## Task 12: Upload session

**Files:** `src/upload/session.rs`

```rust
pub async fn upload_session(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    parent_item_id: &str,
    name: &str,
    total_size: u64,
    chunks: impl Iterator<Item = bytes::Bytes>,
) -> Result<RemoteItem, GraphInternalError>;
```

Two-phase:
1. `POST /me/drive/items/{parent}:/{name}:/createUploadSession` with `{ item: { @microsoft.graph.conflictBehavior: "replace" } }` → returns `{ uploadUrl, expirationDateTime, nextExpectedRanges: ["0-"] }`.
2. For each chunk (must be a multiple of 320 KiB; size from `SESSION_CHUNK_BYTES` = 10 MiB), `PUT {uploadUrl}` with `Content-Range: bytes {start}-{end}/{total}` and body. Server returns either 202 (continue) with `nextExpectedRanges` or 201/200 (final) with the `RemoteItem`.

Handle 416 (mismatched range) by re-querying the session's state via `GET {uploadUrl}` and resuming from `nextExpectedRanges[0]`.

**Tests:** wiremock multi-PUT fixture; 416 retry scenario; the upload completes and returns a `RemoteItem`.

**Gate + commit:** `feat(graph): resumable upload sessions with 416 retry`

---

## Task 13: Rename, delete, mkdir

**Files:** `src/ops.rs`

```rust
pub async fn rename(http, token, drive_id, item_id, new_name) -> Result<RemoteItem, GraphInternalError>;
pub async fn delete(http, token, drive_id, item_id) -> Result<(), GraphInternalError>;
pub async fn mkdir(http, token, drive_id, parent_item_id, name) -> Result<RemoteItem, GraphInternalError>;
```

- Rename: `PATCH /me/drive/items/{id}` with `{ "name": "..." }`.
- Delete: `DELETE /me/drive/items/{id}`. (Server moves to Recycle Bin.)
- Mkdir: `POST /me/drive/items/{parent}/children` with `{ "name": "...", "folder": {}, "@microsoft.graph.conflictBehavior": "fail" }`. 409 → fetch existing item; return it.

**Tests:** wiremock per operation. Mkdir-conflict: 409 → server returns existing folder via a follow-up GET; assert helper returns that folder.

**Gate + commit:** `feat(graph): rename, delete, mkdir helpers`

---

## Task 14: Throttling token bucket

**Files:** `src/throttle.rs`

A per-account token bucket: rate `GRAPH_RPS_PER_ACCOUNT` (8 rps), burst capacity 16. Implemented as `tokio::sync::Semaphore` + a `tokio::time::interval` permit-refill task.

```rust
pub struct Bucket { /* per-account state */ }
impl Bucket {
    pub fn new() -> Self;
    pub async fn acquire(&self, account: &AccountId);
}
```

The `GraphClient` calls `throttle.acquire(account).await` before every outbound HTTP request.

**Tests:** spam 100 acquires for one account; the 9th acquire onward blocks until ~125 ms elapsed; total elapsed for 100 calls is between 12 s and 13 s at 8 rps.

**Gate + commit:** `feat(graph): per-account token-bucket throttling`

---

## Task 15: Error mapping

**Files:** `src/error.rs`

```rust
pub enum GraphInternalError { ... }  // internal: rich variants per failure mode
fn map_to_port(err: GraphInternalError) -> onesync_core::ports::GraphError { ... }
```

Internal enum carries source detail (HTTP status, Microsoft error code, request id, etc.) that the port-level enum doesn't expose. Mapping per the spec table in `docs/spec/04-onedrive-adapter.md`:

- 401 (after refresh attempt) → `Unauthorized`
- 401 with `invalid_grant` on refresh → `ReAuthRequired`
- 403 → `Forbidden`
- 404 → `NotFound`
- 409 nameAlreadyExists → `NameConflict`
- 410 resyncRequired → `ResyncRequired`
- 412 → `Stale { server_etag }`
- 416 → `InvalidRange`
- 429/503 with Retry-After → `Throttled { retry_after_s }`
- 5xx other → `Transient`
- Network / DNS / TLS → `Network { detail }`
- Body decode → `Decode { detail }`
- Hash mismatch → `HashMismatch`
- File over `MAX_FILE_SIZE_BYTES` → `TooLarge`

Each internal variant carries the request id from the response's `request-id` header (logged but not surfaced via the port).

**Tests:** for every row in the spec table, assert that an internal error of that shape maps to the right `GraphError` variant.

**Gate + commit:** `feat(graph): error mapping per spec/04 table`

---

## Task 16: `RemoteDrive` port impl

**Files:** `src/adapter.rs`

`GraphAdapter` holds a `GraphClient` plus a `Box<dyn TokenSource>` (a small trait the keychain adapter implements — see M3b). For M3a's tests, use an in-process `TokenSource` that returns a fixed refresh token.

`impl RemoteDrive for GraphAdapter` dispatches each port method to the helper functions in `items`, `delta`, `download`, `upload`, `ops`. Every method maps errors via `error::map_to_port`.

The `TokenSource` trait lives in `onesync-graph::adapter` for now (we don't add a port dependency on keychain). M3b's `KeychainTokenSource` will implement it externally.

**Tests:** end-to-end fixture — `GraphAdapter::delta` over a wiremock-served drive returns a populated `DeltaPage`. Cover at least 4 port methods in this integration test.

**Gate + commit:** `feat(graph): GraphAdapter implements the RemoteDrive port`

---

## Task 17: In-crate fake `RemoteDrive`

**Files:** `src/fakes.rs`

In-memory `FakeRemoteDrive` for engine tests in M4. Stores items in a `HashMap<RemoteItemId, FakeItem>`, generates fake delta tokens. Behavioural rules match `GraphAdapter`:
- `delta` returns items since the cursor; first call with `None` returns all items + a fresh cursor.
- `upload_*` adds an item; returns its `RemoteItem`.
- `download` returns the stored bytes or `NotFound`.
- `rename`/`delete`/`mkdir` mutate the map.

Gated `#[cfg(test)]` for now (same pattern as M2's fakes); feature flag deferred to M4 when engine tests need cross-crate access.

**Tests:** 3-4 unit tests proving basic round-trip.

**Gate + commit:** `feat(graph): in-memory FakeRemoteDrive for engine tests`

---

## Task 18: M3a close

- Run the full workspace gate (`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run --workspace`, `cargo run -p xtask -- check-schema`).
- Update `docs/plans/2026-05-11-roadmap.md` M3 row with M3a's contribution (note: M3 closes only when both M3a and M3b are merged).
- Commit: `docs(plans): record M3a (graph adapter) completion on roadmap`.
- Do NOT advance `main` from this workspace — the controller in the main checkout coordinates the M3a + M3b merge.

**Workspace test count target:** ≥ 200.

---

## Self-review checklist

- [ ] All 9 `RemoteDrive` methods implemented and exercised by tests.
- [ ] PKCE conforms to RFC 7636 Appendix B.
- [ ] `id_token` parser correctly assigns `AccountKind::Personal` for the consumer-MSA tenant.
- [ ] Delta pager handles `nextLink` and `deltaLink` separately.
- [ ] Upload session resumes after 416 by querying `nextExpectedRanges`.
- [ ] Download verifies `sha1Hash` (Personal) and `quickXorHash` (Business); mismatch surfaces as `HashMismatch`.
- [ ] Error mapping covers every row of the spec table.
- [ ] Throttling honours `Retry-After` and bounds requests at `GRAPH_RPS_PER_ACCOUNT`.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.
- [ ] No `unsafe` outside vetted external crates; `#![forbid(unsafe_code)]` at crate root.

## Carry-overs

- E2E tier against a real Microsoft Graph test tenant is **out of scope for M3a** — it requires Azure tenant credentials. Add `#[ignore]` placeholder tests under `tests/e2e/` that document the env-var gating; the real e2e suite lands separately.
- SharePoint document libraries (`/sites/{site}/drives/{drive}`) are out of scope per `docs/spec/04-onedrive-adapter.md`.
- Webhook subscriptions are stubbed but not enabled; engine polling is sufficient.
