# onesync

macOS background daemon and CLI for two-way synchronisation between a designated local
folder and a designated folder in OneDrive (Personal or Business). Written in Safe Rust.

- **Design specs:** [`docs/spec/`](docs/spec/)
- **Roadmap:** [`docs/plans/2026-05-11-roadmap.md`](docs/plans/2026-05-11-roadmap.md)
- **Full install guide:** [`docs/install/README.md`](docs/install/README.md)

## Install

### Homebrew (recommended)

```sh
brew tap <owner>/onesync          # see docs/install/homebrew/README.md
brew install onesync
```

### `curl | bash`

```sh
curl -fsSL https://onesync.example.com/install.sh | bash
```

Both fetch the same notarised universal binary from GitHub Releases. The
installer script ([`docs/install/install.sh`](docs/install/install.sh)) verifies
the SHA-256 before writing to `/usr/local/bin`.

### From source

Requires Rust 1.95.0 (pinned via `rust-toolchain.toml`) and `cargo-nextest`.

```sh
cargo build --workspace --release
install -m 0755 target/release/onesync  /usr/local/bin/onesync
install -m 0755 target/release/onesyncd /usr/local/bin/onesyncd
```

## Configure

The full step-by-step (with the Azure portal walk-through and optional
Cloudflare Tunnel setup) is in [`docs/install/README.md`](docs/install/README.md).
The short version:

### 1. Register an Azure AD app

onesync ships **no project-owned client ID**: register your own multi-tenant
Public Client app, add the `Files.ReadWrite`, `offline_access`, and `User.Read`
delegated Microsoft Graph permissions, and register
`http://localhost/callback` as the redirect URI. Copy the **Application (client)
ID**.

### 2. Tell onesync about the client id, then sign in

```sh
onesync config set --azure-ad-client-id <YOUR-CLIENT-ID>
onesync account login       # opens browser; redirect lands on loopback
```

### 3. Add a sync pair

```sh
onesync pair add \
  --account-id <acct_…> \
  --local-path  ~/OneDrive \
  --remote-path /Documents/OneSync
```

The remote folder must already exist (create it in the OneDrive web UI first).

### 4. Install the daemon

```sh
onesync service install     # writes the LaunchAgent + launchctl bootstrap
onesync service doctor      # verifies plist, binary, state dir, IPC socket
```

`brew services start onesync` is the Homebrew equivalent. On either path the
daemon runs under your user `gui/<uid>` launchd domain.

## Day-2 operations

| Need | Command |
|---|---|
| One-shot sync now | `onesync pair force-sync --id <pair_…>` |
| Watch live events | `onesync logs tail` |
| List open conflicts | `onesync conflicts list` |
| Self-check (state store, Keychain, FSEvents, FDA) | `onesyncd --check` |
| Restart the daemon | `onesync service restart` |
| In-place binary upgrade | see [`docs/install/upgrade.md`](docs/install/upgrade.md) |
| Stop the daemon | `onesync service stop` |
| Full removal | `onesync service uninstall --purge` |

## Development

```sh
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run -p xtask -- check-schema
```

## Status

Milestones M1 through M13 are complete. The current release-engineering
pipeline (universal binaries, codesign + notarisation, Homebrew tap, `curl |
bash` installer) is wired but unblocked-pending Apple Developer ID credentials
+ a GitHub Release tag — see [`docs/plans/2026-05-11-roadmap.md`](docs/plans/2026-05-11-roadmap.md).
