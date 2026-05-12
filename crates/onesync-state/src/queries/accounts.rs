//! Account queries.

use rusqlite::{Connection, OptionalExtension, params};

use onesync_protocol::{
    account::Account,
    enums::AccountKind,
    id::AccountId,
    primitives::{DriveId, KeychainRef},
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert or update an account row.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the underlying SQL call fails or scopes can't be encoded.
pub fn upsert(conn: &Connection, account: &Account) -> Result<(), StateStoreError> {
    let scopes_json = serde_json::to_string(&account.scopes)
        .map_err(|e| StateStoreError::Sqlite(format!("encode scopes: {e}")))?;
    conn.execute(
        "INSERT INTO accounts \
            (id, kind, upn, tenant_id, drive_id, display_name, keychain_ref, scopes_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
            kind = excluded.kind, \
            upn = excluded.upn, \
            tenant_id = excluded.tenant_id, \
            drive_id = excluded.drive_id, \
            display_name = excluded.display_name, \
            keychain_ref = excluded.keychain_ref, \
            scopes_json = excluded.scopes_json, \
            updated_at = excluded.updated_at",
        params![
            account.id.to_string(),
            kind_to_str(account.kind),
            account.upn,
            account.tenant_id,
            account.drive_id.as_str(),
            account.display_name,
            account.keychain_ref.as_str(),
            scopes_json,
            account.created_at.into_inner().to_rfc3339(),
            account.updated_at.into_inner().to_rfc3339(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch all accounts ordered by id.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn list(conn: &Connection) -> Result<Vec<Account>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, upn, tenant_id, drive_id, display_name, keychain_ref, scopes_json, created_at, updated_at \
             FROM accounts ORDER BY id",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map([], row_to_account)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

/// Delete an account row by id.
///
/// Foreign-key cascades remove the account's pairs and any rows that referenced them.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails.
pub fn remove(conn: &Connection, id: &AccountId) -> Result<(), StateStoreError> {
    conn.execute("DELETE FROM accounts WHERE id = ?", params![id.to_string()])
        .map(|_| ())
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch an account by id.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or a row can't be decoded.
pub fn get(conn: &Connection, id: &AccountId) -> Result<Option<Account>, StateStoreError> {
    conn.query_row(
        "SELECT id, kind, upn, tenant_id, drive_id, display_name, keychain_ref, scopes_json, created_at, updated_at \
         FROM accounts WHERE id = ?",
        params![id.to_string()],
        row_to_account,
    )
    .optional()
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let id_str: String = row.get(0)?;
    let kind_str: String = row.get(1)?;
    let scopes_json: String = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;

    Ok(Account {
        id: id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        kind: kind_from_str(&kind_str)?,
        upn: row.get(2)?,
        tenant_id: row.get(3)?,
        drive_id: DriveId::new(row.get::<_, String>(4)?),
        display_name: row.get(5)?,
        keychain_ref: KeychainRef::new(row.get::<_, String>(6)?),
        scopes: serde_json::from_str(&scopes_json)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
        created_at: parse_timestamp(&created_at)?,
        updated_at: parse_timestamp(&updated_at)?,
    })
}

const fn kind_to_str(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Personal => "personal",
        AccountKind::Business => "business",
    }
}

fn kind_from_str(s: &str) -> rusqlite::Result<AccountKind> {
    match s {
        "personal" => Ok(AccountKind::Personal),
        "business" => Ok(AccountKind::Business),
        other => Err(rusqlite::Error::InvalidColumnType(
            1,
            format!("unknown account kind: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::id::{AccountTag, Id};
    use onesync_protocol::primitives::Timestamp;
    use tempfile::TempDir;
    use ulid::Ulid;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(
            &tmp.path().join("t.sqlite"),
            &crate::queries::test_timestamp(),
        )
        .expect("open");
        (tmp, pool)
    }

    fn sample_account() -> Account {
        Account {
            id: Id::<AccountTag>::from_ulid(Ulid::from(1u128 << 64)),
            kind: AccountKind::Personal,
            upn: "alice@example.com".into(),
            tenant_id: "9188040d-6c67-4c5b-b112-36a304b66dad".into(),
            drive_id: DriveId::new("drv-1"),
            display_name: "Alice".into(),
            keychain_ref: KeychainRef::new("kc-1"),
            scopes: vec!["Files.ReadWrite".into()],
            created_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
            updated_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
        }
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let acct = sample_account();
        upsert(&conn, &acct).expect("upsert");
        let back = get(&conn, &acct.id).expect("get").expect("present");
        assert_eq!(back, acct);
    }

    #[test]
    fn get_returns_none_for_missing_id() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let id = Id::<AccountTag>::from_ulid(Ulid::from(99u128 << 64));
        assert!(get(&conn, &id).expect("get").is_none());
    }

    #[test]
    fn upsert_is_idempotent_with_updated_fields() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let mut a = sample_account();
        upsert(&conn, &a).expect("first");
        a.display_name = "Alice Updated".into();
        upsert(&conn, &a).expect("second");
        let back = get(&conn, &a.id).expect("get").expect("present");
        assert_eq!(back.display_name, "Alice Updated");
    }
}
