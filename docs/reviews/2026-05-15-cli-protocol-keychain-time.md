# Review: CLI, protocol, keychain, time — 2026-05-15

## Scope (by crate)
- keychain: `lib.rs`, `macos.rs`, `stub.rs`, `fakes.rs`, `token_source.rs`
- time: `lib.rs`, `system_clock.rs`, `ulid_generator.rs`, `fakes.rs`
- protocol: all module files + `tests/schema_compliance.rs`; cross-check with `crates/onesync-state/schema.sql`
- cli: `main.rs`, `cli.rs`, `commands.rs`, `rpc.rs`, `service.rs`, `output.rs`, `error.rs`, `exit_codes.rs` (and `lib.rs`/`tests/` if present)

## Method
Semi-formal certificates applied where reasoning crosses crate boundaries:
- Fault localisation on keychain delete-on-logout, ULID monotonicity, RPC error → exit-code mapping.
- Patch verification template held mentally when assessing whether a single-line fix would suffice.
- Cross-crate trace: protocol error variants ↔ daemon RPC ↔ CLI exit-code mapper.

## Findings
(most severe first; F1, F2 … tagged with [crate])

### F1. [keychain] `RefreshToken` is a plain `String` with no `zeroize`/`secrecy` and `Debug` is derived — CONCERN
**Location:** `crates/onesync-core/src/ports/token_vault.rs:18-22`
**Severity:** CONCERN
**Summary:** The OAuth refresh token type is a raw `String` newtype with `#[derive(Debug)]`; nothing zeroizes the buffer on drop and `RefreshToken("xyz")` prints in any `Debug` formatter, including logs and panic backtraces.
**Evidence:**
```
#[derive(Debug)]
pub struct RefreshToken(
    /// The token string itself, as issued by Microsoft Identity.
    pub String,
);
```
Field is `pub`, used in tests as `back.0` (keychain/fakes/token_source tests). No use of `secrecy::SecretString`, `zeroize::Zeroizing`, or a manual `Drop` that wipes the heap allocation. `Cargo.toml` does not depend on `secrecy` or `zeroize`. `Debug` will print the secret if anyone logs the struct.
**Reasoning:** macOS process isolation does mitigate cross-process recovery of in-process memory, but the spec explicitly elevates refresh-token confidentiality (spec/04-onedrive-adapter.md L286: "Refresh tokens in keychain only. Never persisted to the SQLite database"). Leaks here come from three vectors not covered by process isolation: (a) `tracing` spans/events that capture the token by Debug; (b) panic/backtrace dumps (the workspace bans `panic!`, but `expect`/`unwrap` are tolerated in fakes/tests and `String::from_utf8` is fallible — though it does not embed bytes in the error); (c) heap reuse after deallocation (a later allocation of similar size could observe residue in a core dump). A `SecretString` (`secrecy` crate) plus `Zeroizing<String>` would fix all three at minimal cost.
**Suggested direction:** Replace `pub String` with `secrecy::SecretString`; remove `pub` field; remove `Debug` derive (or implement a redacted Debug); audit log call sites to ensure the token is never traced.

### F2. [keychain] Tokens written to keychain by `bytes`, but on read converted to `String` via `from_utf8` — NIT
**Location:** `crates/onesync-keychain/src/macos.rs:46-48`
**Severity:** NIT
**Summary:** The keychain backend stores the raw bytes of the refresh-token UTF-8 then re-decodes to `String`. The error variant is `Backend("non-utf8 secret: …")` — the `Utf8Error` `Display` shows byte offsets but no plaintext, so this is not a leakage hazard.
**Evidence:** `String::from_utf8(bytes).map_err(|e| VaultError::Backend(format!("non-utf8 secret: {e}")))?`. `from_utf8` retains the buffer in the error (`FromUtf8Error::into_bytes`), but since the `Backend` variant only stringifies `Display`, the actual bytes are not embedded in the error.
**Suggested direction:** None required. If a stricter posture is desired, drop the error context (`.map_err(|_| VaultError::Backend("non-utf8 secret".into()))`) and explicitly drop the error to deallocate the bytes.

### F3. [keychain] Stub `KeychainTokenVault` cannot accidentally compile into production — clean
**Location:** `crates/onesync-keychain/src/lib.rs:12-20`
**Evidence:** `#[cfg(target_os = "macos")]` vs `#[cfg(not(target_os = "macos"))]` are mutually exclusive at the target-triple level — not feature-gated, so a misconfigured feature flag cannot select the stub on macOS. Good.

### F4. [keychain] No `unsafe` blocks in adapter; FFI is encapsulated by `security-framework` — clean
**Location:** entire crate
**Evidence:** `#![forbid(unsafe_code)]` at `lib.rs:5`. All keychain access goes through `security_framework::passwords::{set,get,delete}_generic_password`.

### F5. [keychain] Single-account scope per `delete()` — sign-out wipes only one entry; multi-entry purge happens outside the crate — note
**Location:** `crates/onesync-keychain/src/macos.rs:51-65`; spec `08-installation-and-lifecycle.md:211`
**Severity:** NIT (design note)
**Summary:** `TokenVault::delete` deletes one keychain entry keyed by `(SERVICE_NAME, AccountId)`. The spec says `service uninstall --purge` "requests the keychain adapter to remove every onesync entry (one per account)." The crate has no `delete_all` / enumerate API, so the orchestration must iterate accounts known to the state store before deleting. Partial-failure mid-loop (e.g., second account's delete fails) leaves earlier deletions committed — there is no rollback, but for a delete-on-logout this is the desired (eventually-consistent) behaviour.
**Suggested direction:** Consider adding a `purge_all` (best-effort) entry-point keyed only on `SERVICE_NAME`, using `SecKeychainSearchCopyNext` / class queries. If keeping the per-account loop, document that the CLI must collect partial-failure errors and surface them rather than aborting on the first.

### F6. [keychain] `KeychainRef::new(account.to_string())` — handle equals the AccountId — NIT
**Location:** `crates/onesync-keychain/src/macos.rs:31`, also `fakes.rs:40`
**Severity:** NIT
**Summary:** `store_refresh` returns `KeychainRef::new(account.to_string())`. The keychain "ref" is supposed to be opaque (a pointer into the keychain), yet here it carries the AccountId stringified. Callers who serialise both fields into state will store the AccountId twice. Not a leak — just dead information.
**Suggested direction:** Either make `KeychainRef` carry the composite `SERVICE_NAME:account` so the adapter can look up by ref alone, or remove `KeychainRef` from the `TokenVault` return surface (since `load_refresh` already takes `AccountId`).

### F7. [keychain] `token_source.rs` is one-line delegation; no near-expiry refresh logic lives in this crate — note
**Location:** `crates/onesync-keychain/src/token_source.rs:16-21`
**Severity:** none
**Summary:** Despite the file name, this crate does not implement any "near-expiry refresh" threshold or two-caller race detection. The doc-comment defers all that to the daemon/graph crate. `fetch_refresh` is a thin pass-through. Race-condition reasoning therefore lives outside this crate and is out of scope for the keychain review.

### F8. [time] ULID generation is not monotonic within the same millisecond — CONCERN
**Location:** `crates/onesync-time/src/ulid_generator.rs:18-20`
**Severity:** CONCERN
**Summary:** `UlidGenerator::new_id` calls `Ulid::new()`, which produces a fresh 80-bit CSPRNG payload every call. Two ids generated in the same millisecond are unordered with each other; their byte-order (and `Display`) comparison may go either way. The `ulid` crate's `Generator::monotonic()` constructor — which clamps to "same-ms → previous + 1" — is not used.
**Evidence:**
```rust
impl IdGenerator for UlidGenerator {
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
        Id::from_ulid(Ulid::new())
    }
}
```
There is no shared state across calls (the struct is `Default`, `PhantomData` only); there is no `Mutex<ulid::Generator>` to enforce monotonicity.
**Reasoning:** Spec 01-domain-model L14-15 says "ULIDs sort", and 03-sync-engine L313 says only "`Clock::now` is monotonic enough for tie-breaks within a cycle" — so strict same-ms monotonicity is desirable but not contractually required. Two real risks remain: (a) `ORDER BY id` queries in `onesync-state` (audit log, sync_run) may produce non-stable orderings when ids are produced in batches inside one cycle — visible in CLI `audit tail` output ordering; (b) clock-going-backwards is silently accepted — a fresh `Ulid::new()` after an NTP step backward will produce an id that sorts *before* earlier ids. The `Clock` port has no monotonic guard either.
**Suggested direction:** Hold a `Mutex<ulid::Generator>` inside `UlidGenerator` and call `gen.generate()` (monotonic in-ms). Document the cross-process / cross-restart limit explicitly in the rustdoc.

### F9. [time] Production code outside `onesync-time` calls `chrono::Utc::now()` directly, bypassing the `Clock` port — CONCERN
**Location:** `crates/onesync-graph/src/client.rs:67`, `:86`; `crates/onesync-graph/src/ops.rs:188`; `crates/onesync-daemon/src/scheduler.rs:229`, `:275`; `crates/onesync-state/src/store.rs:176`; `crates/onesync-fs-local/src/scan.rs:235`
**Severity:** CONCERN (CLI/keychain/time/protocol-review scope: noting as a time-port enforcement gap visible from the time crate's design)
**Summary:** The clippy `disallowed-methods` rule banning `chrono::Utc::now` is annotated away with `#[allow(clippy::disallowed_methods)]` in *production* code paths in `onesync-graph`, `onesync-daemon`, `onesync-state`, and `onesync-fs-local`. The justification comment in `client.rs:64` says "we don't have the Clock port" — but `SystemClock` is exactly that port and is exported.
**Evidence:**
```rust
// crates/onesync-graph/src/client.rs:64-67
// LINT: chrono::Utc::now is disallowed in prod code but we don't have the Clock port
let now = chrono::Utc::now();
```
```rust
// crates/onesync-daemon/src/scheduler.rs:229
let now = chrono::Utc::now();
```
**Reasoning:** These bypasses make those code paths un-fakeable in tests (TestClock cannot move time forward for them), and they undermine the lint that exists to enforce the Clock invariant.
**Suggested direction:** Thread `Arc<dyn Clock>` (or generic `C: Clock`) into the graph client, scheduler, state store, and fs-local scanner. Remove the `#[allow]` escape hatches once threaded.

### F10. [time] `TestIdGenerator` deterministic but does *not* match `Ulid::new` shape (timestamp bits = seed) — NIT
**Location:** `crates/onesync-time/src/fakes.rs:73-81`
**Severity:** NIT
**Summary:** `TestIdGenerator` packs `seed << 64 | counter` into the 128-bit ULID payload. The high 48 bits — which a real ULID uses for timestamp — become `seed`. If a test seeds with `0` it gets ULIDs whose timestamp portion is `1970-01-01T00:00:00Z`, which is fine for equality tests but causes `id.timestamp()` to compute wrong values if a test ever reads them.
**Suggested direction:** Document the constraint, or accept a `DateTime<Utc>` base parameter so test ids carry sensible timestamps.

### F12. [protocol] No `#[serde(deny_unknown_fields)]` on any wire struct — CONCERN
**Location:** all of `account.rs`, `pair.rs`, `file_entry.rs`, `file_op.rs`, `conflict.rs`, `sync_run.rs`, `audit.rs`, `config.rs`, `handles.rs`, `rpc.rs`, `errors.rs`
**Severity:** CONCERN
**Summary:** None of the protocol structs use `#[serde(deny_unknown_fields)]`. Unknown fields are silently dropped on deserialise. This is the wrong default for an inward-facing IPC: a future field added by a newer daemon, then read by an older CLI, will be silently lost; the older CLI's response to the user will quietly omit data; and a mistyped field name in CLI request params will be silently ignored rather than rejected with `INVALID_PARAMS`.
**Reasoning:** The protocol crate is a *closed* wire surface — daemon and CLI ship as one binary set in the same release. Forward-compat lenience (the usual reason to omit `deny_unknown_fields`) is not a requirement here: the spec (07-cli-and-ipc.md) treats the CLI/daemon protocol as version-locked to the daemon's `schema_version`. Strict rejection would catch wire drift at deserialise time, with `INVALID_PARAMS` (-32602) for unknown request params and `INTERNAL_ERROR` (-32603) for unknown response fields.
**Suggested direction:** Add `#[serde(deny_unknown_fields)]` to every wire struct, accepting the trade that adding a field becomes a minor wire-bump. Alternatively, gate the strictness behind a `#[cfg(test)]` derive (`deny` in tests, `allow` in prod) so the test suite catches drift while prod stays lenient. The spec should be updated either way.

### F13. [protocol] `JsonRpcRequest.id` is `Option<serde_json::Value>` but `JsonRpcOk.id`/`JsonRpcErrorResponse.id` are `String`/`Option<String>` — BUG (interop)
**Location:** `crates/onesync-protocol/src/rpc.rs:35`, `:105`, `:116`
**Severity:** BUG
**Summary:** JSON-RPC 2.0 allows `id` to be a string, a number, or null. `JsonRpcRequest` accepts any `serde_json::Value`. But `JsonRpcOk.id: String` will fail to deserialise a daemon response that *echoes* a numeric id. Spec 07 line 42 says "id is a string (the CLI generates a ULID per request)" — but the *daemon* response struct is what enforces this on the parser side, and if any client (third-party, debugging tool, or the daemon itself in a future change) sends a numeric id, the CLI will hard-fail with a serde error before reaching application code.
**Evidence:**
```rust
pub struct JsonRpcRequest { pub id: Option<serde_json::Value>, … }
pub struct JsonRpcOk        { pub id: String, … }
pub struct JsonRpcErrorResponse { pub id: Option<String>, … }
```
**Reasoning:** Premises: P1 daemon must echo whatever id the client sent. P2 client (per spec) sends `String` ULIDs only. Function-resolution trace: client builds `JsonRpcRequest::new("req-1", …)` → `Some(Value::String)`. Daemon receives, calls `serde_json::from_str::<JsonRpcRequest>`, gets `Some(Value::String)`. Daemon constructs `JsonRpcResponse::ok(id, result)` — `id: impl Into<String>` is called with that `Value`'s rendered form (caller responsibility). Where does the daemon convert `Value::String` to `String`? If it uses `value.as_str().unwrap_or_default().to_owned()`, *strings* round-trip but a non-string would silently become empty. If it uses `value.to_string()`, a `Value::String("req-1")` would round-trip as `"\"req-1\""` (JSON-encoded) — i.e. the daemon would echo `id: "\"req-1\""` rather than `req-1`. The mismatch in id types is a latent footgun.
**Suggested direction:** Either make all three `id` types `serde_json::Value` (most-faithful to JSON-RPC), or document a strict ULID-string-only id discipline and provide a helper conversion. The current asymmetric pair is the worst option.

### F14. [protocol] `JsonRpcResponse` uses `#[serde(untagged)]`; a daemon bug producing both `result` and `error` would silently parse as `Ok` — CONCERN
**Location:** `crates/onesync-protocol/src/rpc.rs:64-71`
**Severity:** CONCERN
**Summary:** `#[serde(untagged)]` tries variants in order. A malformed daemon response containing both `result: …` and `error: …` (which JSON-RPC 2.0 forbids) deserialises as `JsonRpcResponse::Ok` because `JsonRpcOk` doesn't require absence of `error`. The CLI then never surfaces the error.
**Suggested direction:** Add `#[serde(deny_unknown_fields)]` to both variants — then a response with both fields fails to parse, surfacing a `PARSE_ERROR` to the CLI which is the desired safe failure. (Same fix as F12, just specifically motivated.)

### F15. [protocol] `RemoteItemId` and `remote::DriveItemId`/`primitives::DriveItemId` — duplicated remote item id types — CONCERN
**Location:** `crates/onesync-protocol/src/primitives.rs:141` and `crates/onesync-protocol/src/remote.rs:163-172`
**Severity:** CONCERN
**Summary:** `primitives::DriveItemId` is the canonical `opaque_string!` wrapping a `String` used in `pair.rs:20` (`Pair.remote_item_id`) and `file_side.rs:25` (`FileSide.remote_item_id`). `remote::RemoteItemId` is a *separate* `pub struct RemoteItemId(pub String)` used by the graph adapter. Two distinct types for the same concept will require conversion glue, and a future maintainer touching one but not the other will create drift.
**Suggested direction:** Delete `remote::RemoteItemId` and use `primitives::DriveItemId` throughout. If the graph adapter's `RemoteItem.id: String` is meaningfully different (e.g. it includes deltatoken-specific syntax) document that.

### F16. [protocol] `RemoteItem.id` is `pub String` rather than `DriveItemId`; same for `parent_reference.id` — NIT
**Location:** `crates/onesync-protocol/src/remote.rs:70`, `:54`
**Severity:** NIT
**Summary:** `RemoteItem` and `ParentReference` use raw `String` for ids. Spec 01-domain-model treats `DriveItemId` as a typed wrapper, and other protocol structs honour that.
**Suggested direction:** Either wrap with `DriveItemId` or document that `remote.rs` types deliberately stay as `String` because they come straight off the Graph wire and are only narrowed once mapped into the engine's domain types.

### F17. [protocol] `AccessToken(pub String)` lives in `remote.rs` — same exposure issue as `RefreshToken` (F1) — CONCERN
**Location:** `crates/onesync-protocol/src/remote.rs:133-142`
**Severity:** CONCERN
**Summary:** `AccessToken` is `#[derive(Clone, Debug)]` with `pub String` — `Debug` prints the bearer token. Even though access tokens are shorter-lived than refresh tokens, leaking one in a log enables real Graph calls for ~1h.
**Suggested direction:** Same as F1 — wrap in `SecretString`; remove `Debug` derive.

### F18. [protocol] Error-code namespace coverage gap: spec describes `data.kind` strings but no Rust enum lists them — CONCERN
**Location:** `crates/onesync-protocol/src/errors.rs:9` (`kind: String`); spec 07 lines 51-72
**Severity:** CONCERN
**Summary:** `ErrorEnvelope.kind: String` is open-ended. The spec lists discrete `pair.not_found`, etc. There is no central enumeration that the daemon producers and CLI exit-code mapper share — so a typo in one place ("pair.notfound") is silently a *different* error to the CLI's match arms.
**Suggested direction:** Define a typed `enum ErrorKind { PairNotFound, AccountNotFound, … }` with `#[serde(rename_all = "snake_case")]` or explicit `#[serde(rename = "pair.not_found")]` per variant; back `ErrorEnvelope.kind` with that enum. The CLI's `exit_codes.rs` then `match`es it exhaustively (compile-time check on coverage).

### F19. [protocol] `Account.scopes: Vec<String>` — DB stores `scopes_json` — CONCERN (schema drift trace)
**Location:** `crates/onesync-protocol/src/account.rs:27`; `crates/onesync-state/schema.sql:11` (`scopes_json TEXT NOT NULL`)
**Severity:** NIT (consistency)
**Summary:** Wire form is `scopes: ["Files.ReadWrite", …]`; DB form is one JSON-encoded text blob. That's fine *if* the state-store row mapper handles the conversion, but it means `scopes` cannot be filtered/queried at the SQL level. Note for documentation.

### F20. [protocol] `audit_events.pair_id` on schema is nullable; protocol mirrors with `Option<PairId>` — clean

### F21. [protocol] `instance_config.azure_ad_client_id TEXT NOT NULL DEFAULT ''` — empty string sentinel; Rust mirror is `String` (no `Option`) — NIT
**Location:** `crates/onesync-protocol/src/config.rs:21-24`; schema.sql:92
**Severity:** NIT
**Summary:** "Unconfigured" is represented as empty string in both layers. A `None` discriminator would be more semantically honest. Spec 04-onedrive-adapter calls out BYO-app explicitly. Cost is a small migration; benefit is no risk of "" being silently treated as a real value somewhere.

### F22. [protocol] `RemoteReadStream` does not implement `Debug` despite being a public struct in the protocol crate — NIT
**Location:** `crates/onesync-protocol/src/remote.rs:178`
**Severity:** NIT
**Summary:** Most public types here derive `Debug`. `RemoteReadStream(pub bytes::Bytes)` does not — minor consistency thing. Wrapping `Bytes` (which itself is `Debug`-printable as bytes) means accidental `Debug` could dump file contents.
**Suggested direction:** Leave `Debug` off, document explicitly that this type intentionally suppresses content dumping; or implement a redacted `Debug`.

### F23. [protocol] `path.rs` — `AbsPath` does *not* NFC-normalise but `RelPath` does — CONCERN
**Location:** `crates/onesync-protocol/src/path.rs:108-118`
**Severity:** CONCERN
**Summary:** `RelPath::from_str` normalises to NFC before validation; `AbsPath::from_str` does *not*. macOS paths returned by the kernel are NFD (decomposed). If a user sets `local_path` to a Mac-supplied NFD string and the CLI doesn't normalise upstream, the `Pair.local_path` and the equivalent path observed by FSEvents diverge in their byte representation. Equality checks against the DB will fail.
**Reasoning:** Premises: P1 `RelPath` already NFC-normalises (line 102). P2 `AbsPath` is used for the pair's *local* root (`Pair.local_path`). P3 macOS APFS will not auto-normalise but kernel APIs *historically* served HFS+ NFD-normalised forms. Trace: CLI shells `onesync pair add /Users/…/Caf\u{0065}\u{0301}/Notes` → `AbsPath::from_str(s)` accepts as-is → stored in DB as NFD → FSEvents watcher walks the directory and the canonical kernel path is in NFC → unique-index `pairs_local_path_uq` does not collide but the engine's lookup-by-path returns no match. This is a latent bug that surfaces only with accented characters on the pair root.
**Suggested direction:** Apply the same NFC normalisation in `AbsPath::from_str` and `TryFrom<String>`. Re-run the test corpus with NFD inputs.

### F24. [protocol] `validate_common` enforces `MAX_PATH_BYTES = 1024`, but `s.split('/')` allows empty components (`//`) — NIT
**Location:** `crates/onesync-protocol/src/path.rs:87-91`
**Severity:** NIT
**Summary:** A path like `"a//b"` passes validation (no component equals `..`). Whether that is intended depends on whether `RelPath` should be canonicalised against double-slashes.
**Suggested direction:** Reject empty components, or strip redundant slashes during normalisation.

### F25. [cli] `pair add` / `state backup` / `state export` send raw user PathBuf via `to_string_lossy` — BUG (data loss)
**Location:** `crates/onesync-cli/src/commands.rs:109`, `:250`, `:258`
**Severity:** BUG
**Summary:** All three commands accept `std::path::PathBuf` from clap and shove it across the wire via `to_string_lossy()`. On macOS, `OsStr` is WTF-8/CESU-style: any byte sequence that isn't valid UTF-8 gets replaced with U+FFFD (the lossy replacement char), so the daemon receives a *different* path string than the user typed. The daemon then either rejects it (`AbsPath::from_str` will reject the U+FFFD-containing path only if it contains NUL — U+FFFD is valid UTF-8), or worse, accepts and stores a path that does not exist.
**Evidence:**
```rust
PairCmd::Add { local, … } => rpc(socket, "pair.add", json!({ "local_path": local.to_string_lossy(), … }))
```
```rust
StateCmd::Backup { to } => rpc(socket, "state.backup", json!({ "to_path": to.to_string_lossy() }))
```
**Reasoning:** Premises: P1 `to_string_lossy` replaces invalid sequences with U+FFFD. P2 user-supplied paths on macOS are valid UTF-8 in normal use but NFD/NFC differences (F23) and any rare non-UTF8 invocation would silently corrupt. Trace: user types `onesync pair add --local /Users/x/Café/Notes` where the path was created with NFD on disk → clap stores the shell's NFC bytes → `to_string_lossy` round-trips faithfully → daemon's `AbsPath::from_str` accepts NFC → engine queries SQLite where the path is stored NFC but FSEvents emits NFC too (modern macOS) → mostly fine. The real risk surface is paths with embedded invalid UTF-8 (e.g. legacy files): `to_string_lossy` lossy-replaces, daemon stores a string that does not exist on disk, subsequent ops fail with FileNotFound. The right approach is either reject early (using `.to_str().ok_or(InvalidArgs)?`) or use a binary-safe path encoding.
**Suggested direction:** Replace every `to_string_lossy()` with a strict `to_str()` that returns `CliError::InvalidArgs` on failure. Also canonicalise the path here (`std::fs::canonicalize`) so the daemon sees the same form FSEvents will emit, then NFC-normalise (per F23) before sending.

### F26. [cli] `RpcClient::call` has no timeout; a hung daemon hangs the CLI forever — BUG
**Location:** `crates/onesync-cli/src/rpc.rs:58-85`
**Severity:** BUG
**Summary:** `read_line` on `BufReader<OwnedReadHalf>` blocks indefinitely. If the daemon accepts the connection, reads the request, then stalls (lock contention, deadlock, partial write), the CLI process blocks with no way to interrupt other than SIGINT. There is no `tokio::time::timeout` wrapping the call.
**Evidence:** `let n = self.reader.read_line(&mut line).await?;` — no timeout.
**Reasoning:** Premises: P1 daemon process may stall for any reason. P2 CLI is invoked by scripts and CI. P3 `service.run` only sets `PING_TIMEOUT_S` for the wait-for-daemon loop, not for in-progress calls. Trace: scripted `onesync pair list | jq …` against a deadlocked daemon → indefinite block → CI timeout fires at 10min boundary → developer interpretation is "CLI hung" not "daemon hung". The right fix is a per-call timeout (e.g. 30s default, configurable per method since `pair.force_sync` returns immediately but `account.login.await` blocks for the full OAuth flow).
**Suggested direction:** Add `tokio::time::timeout(Duration::from_secs(30), …)` around `read_line`. Distinguish operations that should be long-lived: `account.login.await` needs minutes; `health.ping` should fail fast. Either expose `--timeout` or hard-code per-method.

### F27. [cli] `RpcError`/`JsonRpcError` → `CliError` mapping is open-ended string-matching; misses several spec'd kinds — CONCERN
**Location:** `crates/onesync-cli/src/error.rs:71-85`; spec 07 §CLI exit codes (table)
**Severity:** CONCERN
**Summary:** The mapping is a single `match kind` with string literals plus two `k.starts_with(...)` arms. Several spec'd cases lack explicit kinds: invalid-args (exit 2 — only used as "reserved", clap handles arg errors before this branch is reached, so kind from daemon goes to Generic), pair-errored (`"pair.errored"` matches but kind from `pair.status` returns is silently `Generic` — spec implies daemon never returns `pair.errored` on plain operations because it's the *pair status*, not an RPC error). The bigger gap: there is *no test* covering the round-trip `data.kind` → exit code beyond the single self-test in `exit_codes.rs`.
**Reasoning:** Cross-crate trace: daemon producer of `data.kind` strings lives in `onesync-daemon`. The protocol crate (F18) has no enum. The CLI consumer matches on string literals. A daemon refactor that renames `"auth.required"` to `"authentication.required"` silently degrades exit code 4 to exit code 1 — visible only in production. The `starts_with("network")` and `starts_with("graph")` arms cover an open set, which means a daemon that emits `"graphql"` would erroneously map to Network. Low likelihood but easy to harden.
**Suggested direction:** Define the kind set in `onesync-protocol::errors::ErrorKind` (F18) and exhaust-match here. The tests in `exit_codes.rs` should also include `From<RpcError>` round trips per kind.

### F28. [cli] `serde_json::from_value::<ErrorEnvelope>(v).ok()` silently drops envelope decode errors — CONCERN
**Location:** `crates/onesync-cli/src/error.rs:57-66`
**Severity:** CONCERN
**Summary:** If the daemon returns an `error.data` payload that doesn't deserialise as `ErrorEnvelope` (typo in field name, missing `retryable`, etc.), the CLI silently falls back to `Generic(format!("{} (code {})", msg, code))` — exit code 1 — instead of surfacing the deserialise failure. A bug in the daemon's error shape becomes "generic error" with no diagnostic.
**Suggested direction:** Log a stderr warning (`eprintln!("warn: malformed error envelope from daemon: …")`) when `from_value` returns `Err`, then continue with `Generic`. Keep exit code unchanged.

### F29. [cli] `From<std::io::Error>` maps `ErrorKind::NotFound` to `DaemonNotRunning` — BUG (mis-categorisation)
**Location:** `crates/onesync-cli/src/error.rs:87-98`
**Severity:** BUG
**Summary:** Any `NotFound` `std::io::Error` becomes `DaemonNotRunning` (exit 3). But `NotFound` is also raised by `state.backup` if the destination path's parent does not exist, by `service install` if the source binary is missing (already wrapped in `Generic` there, so safe), and by any file IO downstream. After the initial connect, a `NotFound` from a non-socket operation will exit 3 — which the spec reserves specifically for "daemon not running and could not auto-start".
**Evidence:**
```rust
if matches!(e.kind(), std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound) {
    Self::DaemonNotRunning(e.to_string())
}
```
**Reasoning:** Premises: P1 `From<io::Error>` is global. P2 Multiple call sites convert IO errors via `?`. Trace: `rpc.rs:48 UnixStream::connect` is the only socket-connect site → `NotFound` there genuinely means socket file missing → daemon not running. ✓. But any other `?` on an `io::Error` (e.g. `service.rs` if someone added one) takes the same global From. Currently `service.rs` wraps every IO in `.map_err(|e| Generic(format!(…)))` so the global From is bypassed there. The bug is *latent* — any new `?` propagation of an IO error will surface as DaemonNotRunning. Either keep the global From narrower (only `ConnectionRefused`) or use a dedicated `connect_io_error` helper at the call site.
**Suggested direction:** Narrow the global `From<io::Error>` to map *only* `ConnectionRefused` to `DaemonNotRunning`; let `NotFound` (and other ErrorKinds) fall through to `Generic`. Add an explicit `connect()` site that detects `NotFound` → `DaemonNotRunning`.

### F30. [cli] `service install` is not atomic and does not detect a running daemon — BUG
**Location:** `crates/onesync-cli/src/service.rs:143-204`
**Severity:** BUG
**Summary:** `install()` does: mkdir → copy binary → chmod → write plist → `launchctl bootstrap` → `launchctl kickstart`. If a daemon is already running (older install), the binary copy will overwrite the file *behind a running process* (macOS allows this for non-text-busy executables, but launchd will re-exec the new binary on next start). There is no pre-flight check for "is the daemon already running?" — spec 08 says `service install` should be idempotent and warn rather than silently overlay.
**Reasoning:** Premises: P1 install runs without checking socket. P2 the spec describes "install/uninstall stops cleanly" — implies install should be aware of prior state. Trace: User upgrades onesync, runs `onesync service install` without first `service stop`. New binary is written under the old daemon's `argv[0]` mapping. `launchctl bootstrap` may error ("Service already bootstrapped") which currently produces `CliError::Generic(format!("launchctl bootstrap failed: …"))` — exit code 1. Better: detect the running daemon at the top of `install`, refuse with `Generic("daemon is already running; run `onesync service uninstall` first or use `service restart`")`, or wrap the whole flow in a single transactional script (stop → swap → start).
**Suggested direction:** At the start of `install`, attempt `RpcClient::connect(&default_socket_path()).await`. If it succeeds, refuse with a guidance message. Atomicise the binary swap: write to `bin/onesyncd.new`, then `rename` over `bin/onesyncd`.

### F31. [cli] `install()` ignores binary signature/checksum integrity and copies without verification — CONCERN
**Location:** `crates/onesync-cli/src/service.rs:159-161`
**Severity:** CONCERN
**Summary:** `locate_daemon_binary` returns the first `onesyncd` it finds next to the CLI or in `$PATH`. A user with a poisoned `$PATH` would have a malicious binary copied to `~/Library/Application Support/onesync/bin/onesyncd` and registered as a LaunchAgent. There is no checksum, no codesign check, no warning.
**Suggested direction:** Verify the binary is codesigned by the same Team ID as the CLI (`codesign -dvv --team-id`), or distribute as a stapled DMG/PKG and refuse copies from unsigned source.

### F32. [cli] `uninstall(purge: true)` deletes state via `remove_dir_all` without confirmation — BUG (data loss)
**Location:** `crates/onesync-cli/src/service.rs:231-237`
**Severity:** BUG
**Summary:** `onesync service uninstall --purge` deletes `~/Library/Application Support/onesync` and `~/Library/Logs/onesync` with no confirmation prompt, no `--yes` flag pattern (which the `account remove` and `pair remove` paths have). Errors from `remove_dir_all` are swallowed (`let _ = …`), so the user can't tell if it succeeded. Spec 08 line 210 explicitly says `--purge` "deletes the state directory" — but the spec also says (line 213) "uninstalling never deletes the user's synced data" which is preserved (the pair *folders* under `$HOME` are untouched). Still: silently nuking the SQLite + audit log without confirmation is a data-loss footgun.
**Suggested direction:** Mirror `account remove` / `pair remove` and require `--yes` (or interactive confirmation). Don't swallow `remove_dir_all` errors — surface them so a partial purge is visible.

### F33. [cli] `JsonRpcRequest.id` constructed via `id_value(String)` — never reaches the spec's ULID requirement — NIT
**Location:** `crates/onesync-cli/src/rpc.rs:17-19`, `:63`
**Severity:** NIT
**Summary:** Spec 07 line 42 says "id is a string (the CLI generates a ULID per request)". The CLI generates `req_00000000000000000001` style ids (zero-padded counter). These are not ULIDs and break the spec's traceability promise — operators correlating CLI requests with daemon logs by ULID will find no match.
**Suggested direction:** Use `ulid::Ulid::new().to_string()` with a `req_` prefix.

### F34. [cli] `health.ping` polling does not honour the same `--socket` override the parent command uses — BUG
**Location:** `crates/onesync-cli/src/service.rs:373`, `:210`, `:257`, `:304`
**Severity:** BUG
**Summary:** `wait_for_daemon`, `uninstall`, `stop`, and `doctor` all call `default_socket_path()` directly instead of the `--socket` override that the rest of the CLI honours (via `commands.rs::run`). If a user runs `onesync --socket /custom/path service start`, the start succeeds but the post-start health-ping loop talks to `${TMPDIR}onesync/onesync.sock` and times out.
**Reasoning:** Trace: `Cli::parse()` captures `socket: Option<PathBuf>`. `commands::run` uses `cli.socket.clone().unwrap_or_else(default_socket_path)` and passes the result downward. But `Command::Service { cmd }` is dispatched to `service::run(cfg, cmd)` which receives only `cfg` and `cmd` — the socket override is dropped.
**Suggested direction:** Thread the socket-path override into `service::run` and use it consistently. Update tests.

### F35. [cli] `config.set` silently coerces unparseable JSON to `Value::String(value)` — CONCERN
**Location:** `crates/onesync-cli/src/commands.rs:271-273`
**Severity:** CONCERN
**Summary:** `onesync config set min_free_gib 3` sends `{ "min_free_gib": 3 }` (number) but `onesync config set min_free_gib three` sends `{ "min_free_gib": "three" }` (string). The daemon then must reject the string. A user typo silently *changes the wire type* rather than failing early.
**Suggested direction:** Either require an explicit `--type` hint, or have the daemon `config.set` return a clear `INVALID_PARAMS` for type mismatches and document this CLI behaviour.

### F36. [cli] `service install` writes a LaunchAgent plist; plist body is correct per spec — clean
**Location:** `crates/onesync-cli/src/service.rs:103-141`
**Evidence:** `RunAtLoad`, `KeepAlive{ SuccessfulExit=false, Crashed=true }`, `ProcessType=Background`, `LowPriorityIO`, `StandardOut/ErrPath`, `EnvironmentVariables`, `SoftResourceLimits.NumberOfFiles`. Matches `docs/spec/08-installation-and-lifecycle.md`. No `WorkingDirectory` key — note this means the daemon's cwd is launchd's default (`/`), which the daemon must not depend on for any relative path.

### F37. [cli] `commands::run` does not validate `account_id`/`pair_id`/`conflict_id` arguments — CONCERN
**Location:** `crates/onesync-cli/src/cli.rs:92, 139, 146, 156, 172, 175`; `commands.rs` everywhere
**Severity:** CONCERN
**Summary:** All entity-id arguments are `String` with no parser. The CLI passes them through to the daemon as-is. Two consequences: (a) the daemon must do *all* id validation, returning `INVALID_PARAMS` for malformed input; (b) the CLI cannot give the user a useful "did you mean `pair_…`?" error before round-tripping. Compare with `AbsPath`/`RelPath` which *are* validated client-side via `try_from`.
**Suggested direction:** Use clap's `value_parser` with `clap::value_parser!(PairId)` (requires `Id::from_str`); reject malformed ids at parse time. This keeps the daemon's `INVALID_PARAMS` path as a defence-in-depth check rather than the primary validation surface.

### F38. [cli] `service install` does not preserve binary attrs (xattr, codesign) when copying — NIT
**Location:** `crates/onesync-cli/src/service.rs:160-161`
**Severity:** NIT
**Summary:** `std::fs::copy` preserves mode (on Unix) but NOT extended attributes. macOS code-signing data lives in a special section of Mach-O headers and *is* preserved (it's part of the binary content) — so signing is safe. But the quarantine xattr (`com.apple.quarantine`) and provenance xattrs are not. Whether that matters depends on distribution. Worth verifying once a signed pkg is in place.

### F39. [cli] No `unsafe`; `service.rs` shells out via `std::process::Command` which is sync inside an async fn — NIT
**Location:** `crates/onesync-cli/src/service.rs:53-61, 357-369`
**Severity:** NIT
**Summary:** `std::process::Command` blocks the tokio runtime when called from within an async fn. Since `service` operations run on `new_current_thread`, this stalls the entire runtime for the duration of `launchctl` / `id -u`. Each call is short, so this is theoretical, but switching to `tokio::process::Command` would be more idiomatic.

### F40. [cli] `default_socket_path` builds `${TMPDIR}onesync/onesync.sock` with no separator handling — CONCERN
**Location:** `crates/onesync-cli/src/rpc.rs:30-36`; comment on `cli.rs:21`
**Severity:** NIT
**Summary:** `PathBuf::push("onesync")` on a `TMPDIR` that ends with `/` (typical: macOS `/var/folders/…/T/`) joins correctly to `/var/folders/…/T/onesync/onesync.sock`. But on a `TMPDIR` that does *not* end with `/` (e.g. someone sets `TMPDIR=/tmp`), `PathBuf::push` is also correct. So this is mostly fine — but the rustdoc comment on `cli.rs:21` says `${TMPDIR}onesync/onesync.sock` with no slash, which is misleading. The implementation does insert a separator via `PathBuf::push`. Documentation drift.

### F11. [time] No `unsafe`; `SystemClock` correctly the only sanctioned `Utc::now()` site within the time crate — clean
**Location:** `crates/onesync-time/src/system_clock.rs`, `lib.rs:3`
**Evidence:** `#![forbid(unsafe_code)]`. The `#[allow(clippy::disallowed_methods)]` on `SystemClock::now` is the legitimate port implementation.

## Cross-cutting observations

- **Token-string hygiene (F1 + F17).** `RefreshToken(pub String)` and `AccessToken(pub String)` both derive `Debug` and lack zeroize/secrecy wrappers. Together they form a consistent gap: anywhere a token is dropped into `tracing` or a panic backtrace, it leaks. The fix is one PR — add `secrecy = "*"` to the workspace, swap both `pub String` fields for `SecretString`, remove `Debug` derives.

- **Error-kind contract is a string handshake across three crates (F18 + F27).** Daemon emits strings; protocol's `ErrorEnvelope.kind: String` carries them; CLI matches on string literals to choose exit codes. There is no compile-time enforcement of coverage. Adding an `ErrorKind` enum in `onesync-protocol::errors` and `match`-ing it exhaustively in `onesync-cli::error::From<RpcError>` closes the loop and lets the test suite catch drift.

- **Wire shape leniency (F12 + F14).** No `deny_unknown_fields`. For a closed in-process IPC where daemon and CLI ship together, lenient deserialisation is the wrong default — it hides typos and silently downgrades a mistyped param into "default value" semantics.

- **Path handling diverges between layers (F23 + F25).** `RelPath` NFC-normalises but `AbsPath` does not; CLI sends `to_string_lossy()` rather than strict `to_str()`. Both together mean a pair created against an accented-character path on disk can end up with a stored `local_path` that does not match what FSEvents emits.

- **`Clock` port discipline is partial (F9).** Most production code paths in `onesync-graph`, `onesync-daemon`, `onesync-state`, and `onesync-fs-local` `#[allow(clippy::disallowed_methods)]` and call `chrono::Utc::now()` directly. The `SystemClock` adapter exists but is not threaded through. This makes those modules untestable with `TestClock`.

- **Install/uninstall surface is the most fragile area of the CLI (F30, F32, F34).** Three coupled bugs: install doesn't detect a running daemon and isn't atomic; uninstall `--purge` swallows errors with no confirmation; service ops drop the global `--socket` override. The whole flow needs hardening as a unit before M7 ships.

- **Protocol-CLI coupling via untyped `Value`.** `commands.rs` uses `rpc<Value>` everywhere — the CLI never deserialises into typed entities, just forwards JSON to `emit_value`. Pro: small. Con: missing field detection (F12) is invisible at the CLI layer. The typed surface in `protocol/handles.rs` etc. is only exercised by the daemon side.

## What looks correct

- **Keychain service/account naming** matches the spec exactly (`dev.onesync.refresh-token`, `AccountId` literal).
- **Keychain feature-flag separation** — `#[cfg(target_os = "macos")]` vs `not(macos)` is target-triple-based, so the stub cannot be force-compiled into production.
- **No `unsafe`** anywhere in the four crates: every `lib.rs` has `#![forbid(unsafe_code)]`; `security-framework` encapsulates the only FFI.
- **Keychain `delete` is idempotent** (errSecItemNotFound is folded to `Ok`).
- **`Id<T>` parsing and serialisation** are tight: typed prefix enforcement, ULID body shape, schema-matching regex `^[a-z]{2,4}_…$`.
- **`ContentHash`** validates 64-hex strictly and renders lowercase deterministically.
- **`RelPath` NFC-normalises** and rejects `..`, NUL, and leading `/`. Good.
- **JSON-RPC error code constants** are correct per RFC 4627-bis / JSON-RPC 2.0 (parse-error -32700, etc.).
- **CLI `exit_codes`** has full coverage of every `CliError` variant with a self-test.
- **`render_plist`** matches the spec's KeepAlive / RunAtLoad / ProcessType requirements.
- **`uninstall` does best-effort `service.shutdown { drain: true }` before `launchctl bootout`** — spec compliant.
- **`fakes.rs` in both keychain and time crates** is `#[cfg(any(test, feature = "fakes"))]`-gated so it cannot leak into prod.
