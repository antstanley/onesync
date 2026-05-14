# onesync installation guide

> M9 doc: covers the pre-flight steps you complete **before** running
> `onesync service install`. Two prerequisites: register an Azure AD app and
> (optionally) set up a Cloudflare Tunnel for push notifications.

## 1. Register an Azure AD application

onesync intentionally ships no project-owned client ID — you register your own. This puts
the Microsoft per-app rate limit in your own bucket and removes a project-level
tenant-ownership liability. See the decision in
[`docs/spec/04-onedrive-adapter.md`](../spec/04-onedrive-adapter.md#assumptions-and-open-questions).

1. Sign in to the Azure portal at <https://portal.azure.com/>.
2. Search for **App registrations** → **New registration**.
3. Fill in:
   - **Name:** anything memorable, e.g. `onesync-personal`.
   - **Supported account types:**
     `Accounts in any organizational directory (Any Microsoft Entra ID tenant - Multitenant) and personal Microsoft accounts (e.g. Skype, Xbox)`.
     This matches the `common` authority onesync uses at sign-in.
   - **Redirect URI:** select **Public client/native (mobile & desktop)** and enter
     `http://localhost/callback`.
     onesync binds an ephemeral loopback port at sign-in time and sends
     `http://localhost:<port>/callback`. Microsoft Entra's loopback exception ignores
     the port at runtime as long as the host is `localhost`/`127.0.0.1`/`[::1]` and the
     **path** matches the registered value — so register the URI exactly as written
     above (with `/callback`), not just `http://localhost`.
4. After registration, copy the **Application (client) ID** from the overview page. You'll feed
   it to `onesync config set` below.
5. Under **API permissions** → **Add a permission** → **Microsoft Graph** → **Delegated
   permissions**, add:
   - `Files.ReadWrite`
   - `offline_access`
   - `User.Read`

   You do **not** need `Files.ReadWrite.All` unless you intend to sync drives outside of
   `/me/drive` (SharePoint support lands in M11 — see
   [`docs/plans/2026-05-11-roadmap.md`](../plans/2026-05-11-roadmap.md)).

6. Tell onesync the client id:

   ```sh
   onesync config set --azure-ad-client-id <THE-COPIED-CLIENT-ID>
   ```

   (Or call `config.set` directly over the JSON-RPC socket: `{"azure_ad_client_id": "…"}`.)

7. Run `onesync account login` (or call `account.login.begin` over JSON-RPC). onesync prints
   an auth URL, opens a one-shot loopback listener on an ephemeral port, and parks. Open the
   URL in your browser, complete the Microsoft sign-in flow, and the daemon completes the
   exchange + persists the refresh token in the macOS Keychain.

## 2. (Optional) Cloudflare Tunnel for push notifications

Without a tunnel, onesync polls `/delta` every `DELTA_POLL_INTERVAL_MS` (30 s by default).
With a tunnel, Microsoft Graph can push change notifications to the daemon and per-pair
latency drops to seconds.

Webhooks are **opt-in and off by default**. See the decisions in
[`docs/spec/03-sync-engine.md`](../spec/03-sync-engine.md#assumptions-and-open-questions) and
[`docs/spec/04-onedrive-adapter.md`](../spec/04-onedrive-adapter.md#assumptions-and-open-questions).

1. Install `cloudflared` (`brew install cloudflared`).
2. Authorise it: `cloudflared tunnel login`.
3. Create a named tunnel: `cloudflared tunnel create onesync`.
4. Pick a hostname under a zone you control (e.g. `onesync.example.com`).
5. Create a config at `~/.cloudflared/onesync.yml`:

   ```yaml
   tunnel: <TUNNEL_UUID_FROM_CREATE>
   credentials-file: /Users/<you>/.cloudflared/<TUNNEL_UUID>.json

   ingress:
     - hostname: onesync.example.com
       service: http://localhost:8765
     - service: http_status:404
   ```

6. Set the matching port and notification URL in onesync:

   ```sh
   onesync config set --webhook-listener-port 8765 \
                      --webhook-notification-url https://onesync.example.com/callback
   ```

   The daemon's scheduler reads `webhook_notification_url` at startup. When set + at least one
   pair has `webhook_enabled = true`, the scheduler registers `/subscriptions` against Graph
   so notifications start flowing.

7. Route the hostname: `cloudflared tunnel route dns onesync onesync.example.com`.
8. Run the tunnel: `cloudflared tunnel run onesync` (or install it as a launchd job; see
   `cloudflared service install`).
9. Enable webhooks per pair: `onesync pair set --id <pair-id> --webhook-enabled true`
   (carry-over: the explicit `pair.subscribe` registration call lands in M10).

The polling fallback stays on while the tunnel is configured; a broken tunnel only costs
latency, not correctness.

## 3. Install the binaries

Either:

- **Homebrew** (recommended for most users — see [`homebrew/README.md`](homebrew/README.md)):

  ```sh
  brew tap <owner>/onesync
  brew install onesync
  ```

- **`curl | bash`** (no Homebrew dependency; the source lives at
  [`install.sh`](install.sh)):

  ```sh
  curl -fsSL https://onesync.example.com/install.sh | bash
  ```

Both fetch the same notarised universal binary from the project's GitHub
Releases.

## 4. Install the daemon

After `account login` succeeds:

```sh
onesync service install      # writes the LaunchAgent plist
onesync service start        # launchctl bootstrap
onesync service doctor       # verify everything is wired correctly
```

For in-place upgrades after the daemon is running, see
[`upgrade.md`](upgrade.md).

## Troubleshooting

- `account.login.begin` returns `APP_ERROR_BASE - 10`: `azure_ad_client_id` is unset. Re-run
  step 1.6.
- `pair.add` returns `APP_ERROR_BASE - 25`: the remote path does not exist in your OneDrive.
  Create it via the web UI, then retry.
- Webhook deliveries aren't triggering cycles: confirm
  `onesync config get | grep webhook_listener_port` matches the port in your `cloudflared`
  config, and that `pair.get` shows `webhook_enabled: true` for the target pair.
