# onesync — Documentation

onesync is a macOS background service that performs two-way synchronisation between a
designated local folder and a designated folder in OneDrive. It is written in Safe Rust and
managed through a dedicated CLI.

## Design specs

The design is documented in [`spec/`](spec/). Read in order:

- [`spec/00-overview.md`](spec/00-overview.md) — entry point: problem, goals, non-goals, system shape
- [`spec/01-domain-model.md`](spec/01-domain-model.md) — entities, IDs, relationships, lifecycle state machines
- [`spec/02-architecture.md`](spec/02-architecture.md) — crate layout, ports, hexagonal layering, dependency graph
- [`spec/03-sync-engine.md`](spec/03-sync-engine.md) — sync cycle, change detection, conflict resolution, retry
- [`spec/04-onedrive-adapter.md`](spec/04-onedrive-adapter.md) — Microsoft Graph client, OAuth, delta tokens, upload sessions
- [`spec/05-local-adapter.md`](spec/05-local-adapter.md) — FSEvents watcher, hashing, atomic writes
- [`spec/06-state-store.md`](spec/06-state-store.md) — SQLite schema, indexes, migrations
- [`spec/07-cli-and-ipc.md`](spec/07-cli-and-ipc.md) — CLI commands and the JSON-RPC 2.0 contract
- [`spec/08-installation-and-lifecycle.md`](spec/08-installation-and-lifecycle.md) — LaunchAgent plist, paths, install, upgrade, uninstall
- [`spec/09-development-guidelines.md`](spec/09-development-guidelines.md) — the self-contained development rulebook: Tiger Style, defensive coding, testing pyramid, naming, named limits, AI-agent checklist
- [`spec/canonical-types.schema.json`](spec/canonical-types.schema.json) — JSON Schema (Draft 2020-12) for every domain entity and IPC payload

## Install guide

- [`install/README.md`](install/README.md) — pre-flight steps for first-time setup: Azure AD
  app registration and (optional) Cloudflare Tunnel for push notifications.

## Plans

The active milestone plans live in [`plans/`](plans/). The roadmap at
[`plans/2026-05-11-roadmap.md`](plans/2026-05-11-roadmap.md) tracks status across milestones.
The path-to-v0.1 carry-over list is at
[`plans/2026-05-13-m12b-path-to-v01.md`](plans/2026-05-13-m12b-path-to-v01.md).
