//! Embedded migrations driven by `refinery`.

// LINT: `embed_migrations!` generates a `pub mod migrations { pub fn runner() }` that
// lacks doc-comments (the macro owns the source) and whose items are only used
// within this module's `run` function.
#[allow(missing_docs, clippy::missing_docs_in_private_items)]
mod embedded {
    use refinery::embed_migrations;
    embed_migrations!("src/migrations");
}

/// Apply any pending migrations to the given connection.
///
/// # Errors
/// Returns the underlying refinery error when a migration fails to apply.
pub fn run(conn: &mut rusqlite::Connection) -> Result<(), crate::error::StateStoreError> {
    embedded::migrations::runner()
        .run(conn)
        .map_err(|e| crate::error::StateStoreError::Migration(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_to_fresh_memory_db() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        run(&mut conn).expect("apply migrations");

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();

        for expected in [
            "accounts",
            "audit_events",
            "conflicts",
            "file_entries",
            "file_ops",
            "instance_config",
            "pairs",
            "refinery_schema_history",
            "sync_runs",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table {expected}"
            );
        }
    }

    #[test]
    fn migrations_are_idempotent_on_second_run() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        run(&mut conn).expect("first run");
        run(&mut conn).expect("second run — must be a no-op");
    }
}
