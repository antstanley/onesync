# onesync — Design Overview

**Status:** Draft · **Date:** 2026-05-12 · **Owner:** Stan · **Scope:** Repo-wide

onesync is a macOS background service that performs two-way synchronisation between a designated
local folder and a designated folder in OneDrive. A user-facing CLI manages the service, pairs,
authentication, and observability. The codebase is Safe Rust (no `unsafe` blocks outside vetted
foreign-function shims in adapter crates, and none in core).

This document is the entry point. Detail pages are linked from each section.

---

## Problem

macOS users who rely on OneDrive have two production options today: the official Microsoft
OneDrive client and `rclone`/community wrappers. The Microsoft client ships as a packaged GUI
application that runs continuously, is opinionated about file placement, and lacks scriptable
introspection. `rclone bisync` is general-purpose, requires manual scheduling, and treats every
sync as a full reconciliation rather than a delta.

onesync targets the gap: a small, scriptable, long-running daemon with a dedicated CLI, written
in a language whose compiler enforces memory and concurrency safety, exposing every operation as
an inspectable JSON-RPC method. Configuration is explicit, limits are named constants, and every
sync decision is auditable.

The service is intentionally narrow. It is not a file manager, not a backup tool, and not a
general cloud-storage abstraction. It synchronises folder pairs against OneDrive `/me/drive` and
does that one job well.

---

## Goals

1. Two-way synchronisation between one or more designated local folder ↔ OneDrive folder pairs.
2. Delta-based detection on both sides: FSEvents on macOS, Microsoft Graph `delta` on OneDrive.
3. Safe-by-default conflict handling: when both sides diverge, both copies survive and the loser
   is renamed deterministically.
4. A single static binary that supports both OneDrive Personal (consumer MSA) and OneDrive for
   Business (Azure AD) accounts, detected at sign-in time.
5. CLI surface (`onesync`) for install, authentication, pair management, status, pause/resume,
   force-sync, conflict inspection, and structured log tail.
6. Operates as a per-user macOS `launchd` LaunchAgent, restartable and observable through
   standard macOS facilities.
7. No silent failures: every recoverable error is retried with bounded backoff; every
   non-recoverable error pauses the affected pair and surfaces through CLI and structured logs.
8. Every limit (file size, queue depth, retry count, interval) is a named `const` with units.

## Non-goals

- Windows or Linux support. macOS only.
- SharePoint document libraries, Teams sites, or any drive other than the signed-in account's
  `/me/drive`.
- Selective sync at sub-folder granularity. A pair is the synchronisation unit; everything
  inside it syncs.
- macOS extended attributes, resource forks, Finder tags, comments, or quarantine bits.
- Symbolic links (skipped with a warning) and hard links (treated as ordinary files independently).
- Client-side encryption of file contents. OneDrive's at-rest encryption is the only guarantee.
- A graphical user interface. The CLI is the operator surface.
- File versioning UI. OneDrive's own version history is the source of truth for remote history.

---

## System shape

```
                                 ┌──────────────────────────┐
                                 │        onesync CLI       │
                                 │  (clap, JSON-RPC client) │
                                 └─────────────┬────────────┘
                                               │  Unix domain socket
                                               │  line-delimited JSON-RPC 2.0
                                 ┌─────────────▼────────────┐
                                 │     onesync daemon       │
                                 │   (launchd LaunchAgent)  │
                                 │                          │
                                 │  ┌────────────────────┐  │
                                 │  │   core (no I/O)    │  │
                                 │  │  sync engine       │  │
                                 │  │  conflict policy   │  │
                                 │  │  scheduler         │  │
                                 │  └────────┬───────────┘  │
                                 │           │ ports        │
                                 │  ┌────────┼───────────┐  │
                                 │  │ adapters           │  │
                                 │  │  • fs (FSEvents)   │  │
                                 │  │  • graph (OneDrive)│  │
                                 │  │  • state (SQLite)  │  │
                                 │  │  • keychain (MSAL) │  │
                                 │  │  • clock           │  │
                                 │  └────────┬───────────┘  │
                                 └───────────┼──────────────┘
                                             │
              ┌──────────────────────────────┼──────────────────────────────┐
              │                              │                              │
   ┌──────────▼─────────┐       ┌────────────▼──────────┐      ┌────────────▼──────────┐
   │ macOS filesystem   │       │  Microsoft Graph API   │      │  macOS Keychain        │
   │  (FSEvents stream) │       │  /me/drive, /delta,    │      │  (OAuth refresh tokens)│
   │                    │       │  upload sessions       │      │                        │
   └────────────────────┘       └───────────────────────┘      └────────────────────────┘
```

- The **core** crate holds the sync engine, conflict policy, and scheduler. It depends on
  port traits only; no file, network, time, or keychain code lives here.
- **Adapters** implement the ports. Each is a separate crate so its dependency footprint stays
  isolated.
- The **daemon** binary wires adapters into the core, hosts the JSON-RPC server on a Unix socket,
  and is the only process that touches OneDrive, the filesystem, or the keychain.
- The **CLI** binary is a thin JSON-RPC client. It never reads the filesystem or talks to OneDrive
  directly.

---

## Detail pages

| Page | Topic |
|---|---|
| [01-domain-model.md](01-domain-model.md) | Entities, IDs, relationships, lifecycle state machines |
| [02-architecture.md](02-architecture.md) | Crate layout, ports, hexagonal layering, dependency graph |
| [03-sync-engine.md](03-sync-engine.md) | Sync cycle, change detection, conflict resolution, retry |
| [04-onedrive-adapter.md](04-onedrive-adapter.md) | Microsoft Graph client, OAuth (Personal + Business), delta tokens, upload sessions |
| [05-local-adapter.md](05-local-adapter.md) | FSEvents watcher, hashing, atomic writes, permission rules |
| [06-state-store.md](06-state-store.md) | SQLite schema, indexes, migrations, secret references |
| [07-cli-and-ipc.md](07-cli-and-ipc.md) | CLI commands, JSON-RPC method list, framing, error codes |
| [08-installation-and-lifecycle.md](08-installation-and-lifecycle.md) | LaunchAgent plist, install/uninstall, file paths, log rotation, upgrade |
| [09-development-guidelines.md](09-development-guidelines.md) | Pointer to repo dev guidelines plus onesync-specific limits and conventions |
| [canonical-types.schema.json](canonical-types.schema.json) | JSON Schema (Draft 2020-12) for every canonical domain entity |

---

## Scope summary

| Area | Implementation | Notes |
|---|---|---|
| Platform | macOS 13+ (Ventura and newer) | FSEvents API, Keychain Services, `launchd`. Apple Silicon and Intel both supported. |
| Language | Safe Rust 1.95.0 | `unsafe` forbidden in `onesync-core`; permitted only in adapter crates that wrap macOS frameworks, gated behind `#![forbid(unsafe_code)]` everywhere else. |
| OneDrive accounts | Personal and Business in one binary | Auto-detected at sign-in via Microsoft identity platform. |
| Sync pairs | Multiple per instance, bounded by `MAX_PAIRS_PER_INSTANCE` | Each pair has its own `pair_<ulid>` identity and runs independently. |
| Conflict policy | Keep-both, rename loser | Newer mtime wins canonical name; loser becomes `name (conflict YYYY-MM-DDTHH-MM-SSZ from <host>).ext`. |
| Service surface | Per-user LaunchAgent + Unix-socket JSON-RPC 2.0 | No network ports opened. CLI is sole client; protocol is documented and stable. |
| Authentication | MSAL public-client OAuth2 with PKCE | Refresh tokens stored in macOS Keychain, never on disk. |
| File-size cap | `MAX_FILE_SIZE_BYTES` (10 GiB) | Larger files are rejected with a structured error and surfaced in CLI status. |
| Determinism | `Clock` and `IdGenerator` are ports | Tests inject fakes; production uses `SystemClock` and `UlidGenerator`. |

---

## Assumptions and open questions

**Assumptions**

- The user runs macOS 13 or newer with the default APFS filesystem; case-insensitive comparison
  is the default but case-preserving behaviour must be retained.
- The user has a working Microsoft account (personal or work) with sufficient OneDrive storage.
- The user has registered their own Azure AD application (see Decision: *Azure AD client
  registration*); the install docs walk them through the multi-tenant + personal-account
  registration form.
- Network is available enough of the time that polling and webhook fallback are viable.
- The user's home directory and the chosen local folder live on the same APFS volume; cross-volume
  sync is permitted but renames may decompose into copy+delete.

**Decisions**

- *Single-binary multi-flavour.* **Both OneDrive Personal and Business supported in one build.**
  The Microsoft identity platform unifies the two through `consumers + organizations`
  authority URLs; routing happens at the Graph adapter on token issuance.
- *Multiple pairs.* **A onesync instance manages a bounded set of folder pairs.** Each pair is an
  addressable entity (`pair_<ulid>`) with its own delta token, scheduler entry, and status.
  Bounded by `MAX_PAIRS_PER_INSTANCE`.
- *Conflict policy is keep-both.* **The newer mtime wins the canonical name; the older copy is
  renamed with an unambiguous suffix.** Rationale: mirrors the OneDrive web client's own
  behaviour and never loses data; surfaces the conflict in `onesync conflicts list` for review.
- *CLI ↔ daemon IPC.* **Unix domain socket with line-delimited JSON-RPC 2.0 framing.** No
  network exposure, fast, easy to fake in tests; full method list in
  [`07-cli-and-ipc.md`](07-cli-and-ipc.md).
- *Azure AD client registration.* **The user registers their own Azure AD app; onesync does not
  ship a project-owned multi-tenant client ID.** Pushes onboarding friction up but distributes
  the per-app rate limits across users and removes a project-level liability (tenant ownership,
  app review). See also [`04-onedrive-adapter.md`](04-onedrive-adapter.md) for the registration
  parameters the install docs ask for.
- *Distribution channel.* **Two paths: a Homebrew formula and a `curl | bash` installer that
  pulls a notarised binary from the GitHub Release.** Homebrew gives auto-update and is the
  recommended path; the curl-bash route covers users who do not run Homebrew. Both ship
  notarised + stapled binaries so Gatekeeper does not prompt.
- *Large-file ceiling lowered to 10 GiB.* **`MAX_FILE_SIZE_BYTES = 10 * GIB`.** We have no
  performance data on local BLAKE3 hashing + Graph upload-session resume above this range; the
  earlier 50 GiB figure was a conservative guess. OneDrive accepts up to 250 GiB and the
  adapter is not the bottleneck — raising the cap is gated on observed soak-test numbers.

**Open questions**

- *When should the file-size cap be raised above 10 GiB?* The 10 GiB cap is intentionally
  conservative. Lifting it to 50 GiB or beyond is gated on real data from hashing throughput
  and resumable upload behaviour on representative networks.
