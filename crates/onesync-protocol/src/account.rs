//! Account entity.

use serde::{Deserialize, Serialize};

use crate::enums::AccountKind;
use crate::id::AccountId;
use crate::primitives::{DriveId, KeychainRef, Timestamp};

/// A cloud-storage account linked by the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Unique account identifier.
    pub id: AccountId,
    /// Whether the account is personal or business.
    pub kind: AccountKind,
    /// User Principal Name (email address) for the account.
    pub upn: String,
    /// Azure AD tenant identifier.
    pub tenant_id: String,
    /// `OneDrive` drive identifier.
    pub drive_id: DriveId,
    /// Human-readable display name.
    pub display_name: String,
    /// Pointer into the macOS Keychain for the refresh token.
    pub keychain_ref: KeychainRef,
    /// OAuth scopes granted for this account.
    pub scopes: Vec<String>,
    /// When this account record was created.
    pub created_at: Timestamp,
    /// When this account record was last updated.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_round_trips_through_json() {
        let raw = serde_json::json!({
            "id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "kind": "business",
            "upn": "alice@example.com",
            "tenant_id": "11111111-1111-1111-1111-111111111111",
            "drive_id": "drv-1",
            "display_name": "Alice",
            "keychain_ref": "kc-1",
            "scopes": ["Files.ReadWrite", "offline_access"],
            "created_at": "2026-05-11T10:00:00Z",
            "updated_at": "2026-05-11T10:00:00Z"
        });
        let acct: Account = serde_json::from_value(raw.clone()).expect("parses");
        let back = serde_json::to_value(&acct).expect("serializes");
        assert_eq!(back, raw);
    }
}
