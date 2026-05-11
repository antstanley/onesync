# 07 — CLI and IPC

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

This page documents two surfaces that share a wire contract: the `onesync` command-line tool
that users invoke, and the JSON-RPC 2.0 protocol the CLI speaks to the `onesyncd` daemon. The
CLI is a thin client; every command maps to one or more RPC methods. New CLI commands cannot
be added without first defining (or extending) an RPC method.

The CLI crate is `onesync-cli` (`bin = onesync`). The daemon crate is `onesync-daemon`
(`bin = onesyncd`). The wire types live in `onesync-protocol`.

---

## Transport

- **Channel:** Unix domain socket of type `SOCK_STREAM`.
- **Path:** `<runtime-dir>/onesync.sock` (see
  [`08-installation-and-lifecycle.md`](08-installation-and-lifecycle.md) for the resolution).
- **Permissions:** the socket is created `chmod 0600`, owned by the user who runs the
  daemon. Cross-user access is not permitted and the daemon refuses to start if it cannot
  set the bits.
- **Framing:** line-delimited JSON. Each request and each response is a single JSON object
  terminated by `\n`. Embedded newlines within object values are not permitted; producers
  use the JSON `\n` escape if they need a newline.
- **Encoding:** UTF-8.
- **Frame size cap:** `IPC_FRAME_MAX_BYTES` (1 MiB). Larger frames are rejected with a
  `PayloadTooLarge` error; downloads/uploads stream through Graph adapters, never through
  the IPC channel.

---

## JSON-RPC 2.0

Every message conforms to JSON-RPC 2.0. Three message kinds are used:

- **Request:** carries `id`, `method`, optional `params`.
- **Response:** carries `id`, exactly one of `result` or `error`.
- **Notification:** request without `id`; used only for server-to-client subscription
  events.

`id` is a string (the CLI generates a ULID per request) for traceability. The daemon echoes
the same `id` on the response.

### Error object

```json
{
  "code": -32000,
  "message": "Pair not found",
  "data": {
    "kind": "pair.not_found",
    "pair_id": "pair_01J8…",
    "request_id": "01J8…"
  }
}
```

The `data.kind` field is the stable machine identifier; `data.request_id` is the daemon's
audit identifier for cross-reference with logs. Numeric `code` values follow the JSON-RPC
reserved ranges:

| Range | Meaning |
|---|---|
| -32700 | Parse error (malformed JSON) |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params (schema validation failed) |
| -32603 | Internal error (unexpected; surfaces a bug) |
| -32000 … -32099 | Application errors; `data.kind` discriminates |

The CLI's exit code is derived from `data.kind` (see [exit codes](#cli-exit-codes)).

---

## Methods

The full method list is below. Parameter and result shapes are formally specified by
[`canonical-types.schema.json`](canonical-types.schema.json); the table summarises.

### Health and configuration

| Method | Params | Result | Notes |
|---|---|---|---|
| `health.ping` | `{}` | `{ uptime_s, version, schema_version }` | Used by CLI to detect a running daemon. |
| `health.diagnostics` | `{}` | `Diagnostics` | Full snapshot for support escalation. |
| `config.get` | `{}` | `InstanceConfig` | |
| `config.set` | `Partial<InstanceConfig>` | `InstanceConfig` | Validates ranges; persists; broadcasts `config.changed`. |
| `config.reload` | `{}` | `InstanceConfig` | Forces re-read from db (no-op in normal operation). |

### Accounts

| Method | Params | Result | Notes |
|---|---|---|---|
| `account.login.begin` | `{ client_id? }` | `{ auth_url, state, login_handle }` | Daemon stands up loopback listener. |
| `account.login.await` | `{ login_handle }` | `Account` | Blocks until callback or timeout. |
| `account.list` | `{}` | `Account[]` | |
| `account.get` | `{ account_id }` | `Account` | |
| `account.remove` | `{ account_id, cascade_pairs: bool }` | `{ removed_pairs: PairId[] }` | Refuses unless `cascade_pairs` or no pairs reference. |

`account.login` is split into two RPCs because the CLI must perform the URL launch between
them. The `login_handle` is opaque and one-shot.

### Pairs

| Method | Params | Result | Notes |
|---|---|---|---|
| `pair.add` | `{ account_id, local_path, remote_path, display_name }` | `Pair` | Validates local/remote, resolves remote item, creates row, starts initializing. |
| `pair.list` | `{ account_id?, include_removed?: bool }` | `Pair[]` | |
| `pair.get` | `{ pair_id }` | `Pair` | |
| `pair.pause` | `{ pair_id }` | `Pair` | |
| `pair.resume` | `{ pair_id }` | `Pair` | |
| `pair.remove` | `{ pair_id, delete_local: bool, delete_remote: bool }` | `{ ok: true }` | Both deletes default to false; deletion semantics are explicit. |
| `pair.force_sync` | `{ pair_id, full_scan: bool }` | `SyncRunHandle` | Returns immediately; progress streams via subscription. |
| `pair.status` | `{ pair_id }` | `PairStatusDetail` | Includes recent runs, in-flight ops, conflict count. |

### Conflicts

| Method | Params | Result |
|---|---|---|
| `conflict.list` | `{ pair_id?, include_resolved?: bool }` | `Conflict[]` |
| `conflict.get` | `{ conflict_id }` | `Conflict` |
| `conflict.resolve` | `{ conflict_id, pick: "local"|"remote", keep_loser: bool, note?: string }` | `Conflict` |

### Observability

| Method | Params | Result |
|---|---|---|
| `audit.tail` | `{ from_ts?, level?, kind_prefix? }` | `SubscriptionAck`, then stream of `audit.event` notifications carrying `AuditEvent` |
| `audit.search` | `{ from_ts, to_ts, level?, kind_prefix?, pair_id?, limit }` | `AuditEvent[]` |
| `pair.subscribe` | `{ pair_id? }` | `SubscriptionAck`, then stream of `pair.state_changed` |
| `conflict.subscribe` | `{ pair_id? }` | `SubscriptionAck`, then stream of `conflict.detected` |
| `run.list` | `{ pair_id, limit }` | `SyncRun[]` |
| `run.get` | `{ run_id }` | `SyncRunDetail` |

### State

| Method | Params | Result |
|---|---|---|
| `state.backup` | `{ to_path }` | `{ bytes_written, manifest_path }` |
| `state.export` | `{ to_dir }` | `{ files_written: string[] }` |
| `state.repair.permissions` | `{}` | `{ adjusted: string[] }` |
| `state.compact.now` | `{}` | `{ rows_pruned: { table: count } }` |

### Subscriptions

Subscriptions are server-pushed notifications. The CLI opens a subscription with a request
and the daemon emits notifications until the CLI closes or the daemon cancels.

```
→ { "jsonrpc": "2.0", "id": "01J8…", "method": "audit.tail", "params": { "level": "info" } }
← { "jsonrpc": "2.0", "id": "01J8…", "result": { "subscription_id": "sub_01J8…" } }
← { "jsonrpc": "2.0", "method": "audit.event", "params": { "event": { … } } }
← { "jsonrpc": "2.0", "method": "audit.event", "params": { "event": { … } } }
→ { "jsonrpc": "2.0", "id": "01J8…", "method": "subscription.cancel", "params": { "subscription_id": "sub_01J8…" } }
```

Subscription notification methods carry no `id`. The full list of notification methods:

| Notification method | Payload | Origin |
|---|---|---|
| `audit.event` | `{ event: AuditEvent }` | `audit.tail` subscription |
| `pair.state_changed` | `{ pair_id, status }` | `pair.subscribe` |
| `run.progress` | `{ run_id, files_done, files_total, bytes_done, bytes_total }` | `pair.force_sync` handle |
| `conflict.detected` | `{ conflict: Conflict }` | `conflict.subscribe` |
| `config.changed` | `{ config: InstanceConfig }` | every connected client |

A subscription is reference-counted to its connection. If the connection drops, the daemon
removes the subscription within `SUB_GC_INTERVAL_MS`.

---

## CLI surface

The CLI uses `clap` v4 with derive. Top-level structure:

```
onesync
├── status                                    # daemon + pair overview
├── account
│   ├── login [--client-id <id>]
│   ├── list
│   └── remove <acct-id> [--cascade-pairs]
├── pair
│   ├── add --account <acct-id> --local <path> --remote <path> [--name <label>]
│   ├── list [--account <acct-id>] [--include-removed]
│   ├── show <pair-id>
│   ├── pause <pair-id>
│   ├── resume <pair-id>
│   ├── remove <pair-id> [--delete-local] [--delete-remote] [--yes]
│   └── sync <pair-id> [--full]
├── conflicts
│   ├── list [--pair <pair-id>] [--all]
│   ├── show <conflict-id>
│   └── resolve <conflict-id> --pick local|remote [--discard-loser] [--note <text>]
├── logs
│   ├── tail [--level info|warn|error] [--kind <prefix>]
│   └── search --since <ts> --until <ts> [--pair <pair-id>] [--level …]
├── state
│   ├── backup --to <path>
│   ├── export --to <dir>
│   ├── repair-perms
│   └── compact
├── config
│   ├── get
│   └── set <key> <value>
├── service
│   ├── install                              # write launchd plist, load
│   ├── uninstall                            # unload, remove plist
│   ├── start
│   ├── stop
│   ├── restart
│   └── doctor                               # check installation health
└── version
```

Conventions:

- Every command supports `--json` to emit the underlying RPC result as a single JSON object
  (or JSON Lines for streaming endpoints).
- Every command supports `--no-color` and respects `NO_COLOR` env var.
- Destructive operations (`pair remove`, `account remove`, `state backup` overwriting an
  existing path) require `--yes` to proceed without interactive confirmation.
- The CLI auto-starts the daemon for short-lived idempotent commands (`status`, `pair list`)
  if `onesyncd` is installed but not running, by sending a `kickstart` to launchd. The daemon
  is **never** auto-started for destructive operations.

---

## CLI exit codes

Stable, documented, scriptable:

| Exit code | Meaning |
|---|---|
| 0 | Success |
| 1 | Generic error (catch-all; details on stderr) |
| 2 | Invalid arguments |
| 3 | Daemon not running and could not auto-start |
| 4 | Authentication required (re-login) |
| 5 | Pair errored — see `onesync pair show` |
| 6 | Conflict not resolved (returned by `conflict resolve` when sides changed underfoot) |
| 7 | Permission / filesystem error |
| 8 | Network / Graph API error |
| 9 | Limit exceeded (file too large, queue full, etc.) |
| 10 | Daemon version mismatch (CLI older or newer than daemon by major) |

Exit codes are derived from `error.data.kind` via a fixed table in
`onesync-cli`. The mapping is part of the public contract.

---

## Compatibility

Schema and method evolution rules:

- **Adding** a new method, a new notification method, a new optional parameter, or a new
  result field is a minor version bump.
- **Removing** a method, a notification method, a parameter, or a required field is a major
  version bump.
- The daemon refuses to serve a CLI whose major version differs from its own; the response
  is a single `error.data.kind = "version.major_mismatch"` and exit code 10.
- The CLI carries the daemon version it was built against; if the daemon advertises a newer
  minor version on `health.ping`, the CLI warns once on first connection and continues.

---

## Local-loopback HTTP fallback

Out of scope. The IPC contract is Unix-socket-only. A remote control plane, if it ever
becomes a requirement, would be a separate opt-in adapter rather than an overload of this
surface.

---

## Assumptions and open questions

**Assumptions**

- A single `onesync` user has a single `onesyncd` instance. Multi-user macOS hosts run a
  per-user daemon and a per-user socket.
- Line-delimited JSON is acceptable for the frame sizes onesync sees (sub-MiB). If we ever
  carry large diagnostics in a single response, we will revisit.
- Subscriptions only push, never pull. The CLI does not need to query subscription state.

**Decisions**

- *JSON-RPC 2.0 over line-delimited JSON.* **No protobuf, no gRPC.** Human-readable on the
  wire, trivial to fake in tests, no codegen required.
- *Per-connection subscriptions.* **Subscriptions die with the connection.** No long-lived
  subscriptions across reconnects; the CLI re-subscribes on reconnect.
- *Explicit major-version handshake.* **CLI ≠ daemon major version refuses.** Avoids
  silent skew when an upgrade is partial.
- *Exit-code stability.* **Codes are part of the public contract.** Scripts can branch on
  them without parsing stderr.

**Open questions**

- *Authentication on the socket.* The 0600 permissions plus per-user socket dir are the
  current authorisation story. We have not decided whether to require an additional
  shared-secret token for the IPC, which would defend against same-user processes that
  shouldn't be allowed to drive the daemon.
- *Long-poll vs SSE-style for subscriptions.* The current "push frames as they happen" works
  but does not provide explicit heartbeats; a keep-alive notification every
  `IPC_KEEPALIVE_MS` is sketched but not specified yet.
