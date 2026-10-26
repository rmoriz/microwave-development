use microwave_store::{KvPairRow, SqliteStore};

#[test]
fn password_reset_rows_are_excluded_from_every_snapshot_export_mode() {
    let store = SqliteStore::open_in_memory().unwrap();
    let token = store
        .create_password_reset_token("reset-user-secret")
        .unwrap();
    store.set_kv("ordinary", "ordinary-value").unwrap();

    for include_secrets in [false, true] {
        let snapshot = store.export_snapshot_with_options(include_secrets).unwrap();
        assert!(
            snapshot
                .kvpair
                .iter()
                .all(|row| !row.key.starts_with("id_password_reset:")),
            "password-reset row exported with include_secrets={include_secrets}"
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains(&token));
        assert!(!serialized.contains("reset-user-secret"));

        if include_secrets {
            assert!(
                snapshot
                    .kvpair
                    .iter()
                    .any(|row| row.key == "ordinary"
                        && row.value.as_deref() == Some("ordinary-value")),
                "unrelated kvpair rows must remain in secret-bearing exports"
            );
        }
    }
}

#[test]
fn kv_pair_debug_redacts_password_reset_tokens_and_values() {
    let row = KvPairRow {
        key: "id_password_reset:super-secret-token".into(),
        value: Some("reset-user-secret".into()),
        updated_at: "2026-10-26T00:00:00Z".into(),
    };
    let debug = format!("{row:?}");
    assert!(!debug.contains("super-secret-token"), "{debug}");
    assert!(!debug.contains("reset-user-secret"), "{debug}");

    let ordinary = KvPairRow {
        key: "ordinary".into(),
        value: Some("ordinary-value".into()),
        updated_at: "2026-10-26T00:00:00Z".into(),
    };
    let debug = format!("{ordinary:?}");
    assert!(debug.contains("ordinary-value"));
}
