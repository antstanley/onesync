//! Operator-tunable instance configuration.

use serde::{Deserialize, Serialize};

use crate::enums::LogLevel;
use crate::primitives::Timestamp;

/// Operator-tunable settings for the running onesync instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Minimum verbosity level for log output.
    pub log_level: LogLevel,
    /// Whether to emit macOS user notifications for sync events.
    pub notify: bool,
    /// Whether syncing is permitted over metered network connections.
    pub allow_metered: bool,
    /// Minimum local free disk space (GiB) before sync is paused.
    pub min_free_gib: u32,
    /// When this configuration was last written.
    pub updated_at: Timestamp,
    /// User-registered Azure AD application client id. Empty string means unconfigured;
    /// `account.login.begin` refuses to start until this is set.
    #[serde(default)]
    pub azure_ad_client_id: String,
    /// Local port the Cloudflare-Tunnel webhook receiver binds to. `None` disables the
    /// receiver; the `/delta` polling path is always available as the fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_listener_port: Option<u16>,
    /// Publicly-reachable HTTPS URL of the Cloudflare Tunnel that maps to the local webhook
    /// receiver. The scheduler passes this to Graph `/subscriptions` as `notificationUrl`.
    /// When `None`, the daemon does not register any Graph subscriptions, even for
    /// `webhook_enabled` pairs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_notification_url: Option<String>,
}
