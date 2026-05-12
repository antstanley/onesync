-- M9 — add fields backing the new spec decisions:
--   * azure_ad_client_id on instance_config (user-owned Azure AD client per 04-onedrive-adapter)
--   * webhook_listener_port on instance_config (Cloudflare-Tunnel webhook receiver port)
--   * webhook_enabled on pairs (per-pair opt-in for Graph /subscriptions)
--
-- All additions carry server-side defaults so existing rows upgrade cleanly.

ALTER TABLE instance_config ADD COLUMN azure_ad_client_id TEXT NOT NULL DEFAULT '';
ALTER TABLE instance_config ADD COLUMN webhook_listener_port INTEGER;

ALTER TABLE pairs ADD COLUMN webhook_enabled INTEGER NOT NULL DEFAULT 0;
