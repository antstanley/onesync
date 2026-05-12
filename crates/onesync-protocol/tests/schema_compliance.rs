//! Validates that the Rust types round-trip through `serde_json` and conform to
//! the canonical-types JSON Schema sidecar.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::redundant_clone
)]

use jsonschema::JSONSchema;
use onesync_protocol::*;

const SCHEMA_PATH: &str = "../../docs/spec/canonical-types.schema.json";

fn schema() -> JSONSchema {
    let raw =
        std::fs::read_to_string(SCHEMA_PATH).unwrap_or_else(|e| panic!("read {SCHEMA_PATH}: {e}"));
    let value: serde_json::Value = serde_json::from_str(&raw).expect("schema parses");
    JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&value)
        .expect("schema compiles")
}

fn validate_against(_schema: &JSONSchema, sub_def: &str, instance: &serde_json::Value) {
    // jsonschema can't directly validate a $def; wrap in a one-shot $ref.
    let wrapper: serde_json::Value = serde_json::json!({
        "$ref": format!("#/$defs/{sub_def}")
    });
    let base: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(SCHEMA_PATH).unwrap()).unwrap();
    let mut combined = base.clone();
    combined["$ref"] = wrapper["$ref"].clone();
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&combined)
        .expect("schema compiles");
    let result = compiled.validate(instance);
    if let Err(errors) = result {
        let messages: Vec<_> = errors.map(|e| format!("- {e}")).collect();
        panic!(
            "{sub_def} failed schema validation:\n{}",
            messages.join("\n")
        );
    }
}

#[test]
fn account_validates() {
    let s = schema();
    let _ = s; // keep `schema()` exercised in case the wrapper path changes.
    let acct = serde_json::json!({
        "id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "kind": "personal",
        "upn": "alice@example.com",
        "tenant_id": "9188040d-6c67-4c5b-b112-36a304b66dad",
        "drive_id": "drv-1",
        "display_name": "Alice",
        "keychain_ref": "kc-1",
        "scopes": ["Files.ReadWrite"],
        "created_at": "2026-05-11T10:00:00Z",
        "updated_at": "2026-05-11T10:00:00Z"
    });
    validate_against(&schema(), "Account", &acct);
    let _: account::Account = serde_json::from_value(acct).expect("rust round-trip");
}

#[test]
fn pair_validates() {
    let pair = serde_json::json!({
        "id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "account_id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "local_path": "/Users/alice/OneDrive",
        "remote_item_id": "drive-item-root",
        "remote_path": "/",
        "display_name": "OneDrive",
        "status": "active",
        "paused": false,
        "created_at": "2026-05-11T10:00:00Z",
        "updated_at": "2026-05-11T10:00:00Z",
        "conflict_count": 0,
        "webhook_enabled": false
    });
    validate_against(&schema(), "Pair", &pair);
    let _: pair::Pair = serde_json::from_value(pair).expect("rust round-trip");
}

#[test]
fn file_entry_validates() {
    let entry = serde_json::json!({
        "pair_id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "relative_path": "Documents/notes.md",
        "kind": "file",
        "sync_state": "clean",
        "updated_at": "2026-05-11T10:00:00Z"
    });
    validate_against(&schema(), "FileEntry", &entry);
    let _: file_entry::FileEntry = serde_json::from_value(entry).expect("rust round-trip");
}

#[test]
fn file_op_id_with_two_char_prefix_validates() {
    // Regression: the Id base pattern previously rejected `op_` (2-char prefix).
    let op_id = serde_json::json!("op_01J8X7CFGMZG7Y4DC0VA8DZW2H");
    validate_against(&schema(), "FileOpId", &op_id);
}

#[test]
fn instance_config_validates() {
    let cfg = serde_json::json!({
        "log_level": "info",
        "notify": true,
        "allow_metered": false,
        "min_free_gib": 2,
        "updated_at": "2026-05-11T10:00:00Z",
        "azure_ad_client_id": ""
    });
    validate_against(&schema(), "InstanceConfig", &cfg);
    let _: config::InstanceConfig = serde_json::from_value(cfg).expect("rust round-trip");
}

#[test]
fn error_envelope_validates() {
    let err = serde_json::json!({
        "kind": "graph.throttled",
        "message": "retry after 30s",
        "retryable": true
    });
    validate_against(&schema(), "ErrorEnvelope", &err);
    let _: errors::ErrorEnvelope = serde_json::from_value(err).expect("rust round-trip");
}
