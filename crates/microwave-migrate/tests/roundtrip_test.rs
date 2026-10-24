use microwave_authz::create_organization;
use microwave_migrate::{export, import, MigrateError};
use microwave_store::{ActorType, NewNode, Permission, SqliteStore};
use std::path::Path;

const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");
const PRIVATE_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key.pem");

const NODE_BODY: &str = r#"{"name":"web1","json_class":"Chef::Node","chef_type":"node","chef_environment":"_default","run_list":["role[web]"],"normal":{"tags":["blue"]}}"#;
const ROLE_BODY: &str = r#"{"name":"web","json_class":"Chef::Role","chef_type":"role","run_list":["recipe[app::default]"],"default_attributes":{},"override_attributes":{}}"#;
const ENVIRONMENT_BODY: &str = r#"{"name":"production","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{},"default_attributes":{"tier":"prod"},"override_attributes":{}}"#;
const DATA_BAG_ITEM_BODY: &str =
    r#"{"id":"database","host":"db.example.com","password":"from-a-secret-store"}"#;
const COOKBOOK_MANIFEST: &str = r#"{"name":"app-1.2.3","cookbook_name":"app","version":"1.2.3","recipes":[{"name":"default.rb","path":"recipes/default.rb","checksum":"0123456789abcdef0123456789abcdef","specificity":"default"}]}"#;
const POLICY_BODY: &str = r#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{}}"#;
const CHECKSUM: &str = "0123456789abcdef0123456789abcdef";

fn database(dir: &Path) -> std::path::PathBuf {
    dir.join("microwave.db")
}

#[test]
fn exported_snapshot_round_trips_identity_authz_and_objects() {
    let source_dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(source_dir.path())).unwrap();

    let pivotal = store
        .create_user(
            "pivotal",
            "pivotal@example.com",
            "bcrypt",
            "pivotal-hash",
            "pivotal-salt",
            true,
        )
        .unwrap();
    let alice = store
        .create_user(
            "alice",
            "alice@example.com",
            "sha1+bcrypt",
            "alice-hash",
            "alice-salt",
            false,
        )
        .unwrap();
    let bob = store
        .create_user(
            "bob",
            "bob@example.com",
            "erlang-bcrypt-0.5.0",
            "bob-hash",
            "bob-salt",
            false,
        )
        .unwrap();

    store
        .add_actor_key(&alice.id, ActorType::User, "default", PUBLIC_KEY)
        .unwrap();

    let acme =
        create_organization(&store, "acme", "Acme Corp", &alice.id, &alice.authz_id).unwrap();
    let beta = create_organization(&store, "beta", "Beta Inc", &bob.id, &bob.authz_id).unwrap();

    let web = store.create_client(&acme.id, "web", false, false).unwrap();
    store
        .add_actor_key(&web.id, ActorType::Client, "default", PUBLIC_KEY)
        .unwrap();
    let ci = store.create_client(&beta.id, "ci", true, false).unwrap();

    let node = store
        .create_node(
            &acme.id,
            NewNode {
                name: "web1",
                body: NODE_BODY,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &alice.authz_id,
            },
        )
        .unwrap();
    store
        .set_acl(&node.authz_id, Permission::Read, Some(&bob.authz_id), None)
        .unwrap();
    let ops = store.create_group_for_org(&acme.id, "ops").unwrap();
    let ops_nested = store.create_group_for_org(&acme.id, "ops-nested").unwrap();
    store
        .add_group_child(&ops.authz_id, &ops_nested.authz_id)
        .unwrap();
    store
        .add_group_member(&ops_nested.authz_id, &bob.authz_id)
        .unwrap();
    store
        .set_acl(
            &node.authz_id,
            Permission::Update,
            None,
            Some(&ops.authz_id),
        )
        .unwrap();

    let role = store
        .create_role(&acme.id, "web", ROLE_BODY, &alice.authz_id)
        .unwrap();
    let environment = store
        .create_environment(&acme.id, "production", ENVIRONMENT_BODY, &alice.authz_id)
        .unwrap();
    let data_bag = store
        .create_data_bag(&acme.id, "secrets", &alice.authz_id)
        .unwrap();
    let data_bag_item = store
        .create_data_bag_item(&acme.id, "secrets", "database", DATA_BAG_ITEM_BODY)
        .unwrap();

    let (sandbox, _) = store
        .create_sandbox(&acme.id, &[CHECKSUM.to_string()])
        .unwrap();
    store.commit_sandbox(&acme.id, &sandbox).unwrap();
    let cookbook = store
        .create_cookbook_version(
            &acme.id,
            "app",
            "1.2.3",
            COOKBOOK_MANIFEST,
            &[CHECKSUM.to_string()],
            &alice.authz_id,
        )
        .unwrap();
    let source_association = store
        .associate_policy_revision(
            &acme.id,
            "production",
            "app",
            "rev1",
            POLICY_BODY,
            "{}",
            &alice.authz_id,
        )
        .unwrap()
        .0;
    let source_policy = store.find_policy(&acme.id, "app").unwrap().unwrap();
    let source_revision = store
        .find_policy_revision(&acme.id, "app", "rev1")
        .unwrap()
        .unwrap();
    let oauth_application = store
        .create_oauth_application(
            "Supermarket",
            "supermarket-uid",
            "oauth-application-secret",
            "https://supermarket.example.com/auth/chef/callback",
            "openid profile",
            true,
        )
        .unwrap();

    let snapshot_path = source_dir.path().join("snapshot.json");
    export(source_dir.path(), &snapshot_path).unwrap();
    let document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&snapshot_path).unwrap()).unwrap();
    assert_eq!(document["format"], "microwave-server-snapshot");
    assert_eq!(document["version"], 1);
    assert_eq!(document["source"]["kind"], "local-store");
    assert_eq!(
        document["data"]["oauth_applications"][0]["secret"],
        "oauth-application-secret"
    );
    assert_eq!(
        document["data"]["users"]
            .as_array()
            .unwrap()
            .iter()
            .find(|user| user["username"] == "alice")
            .unwrap()["password_hash"],
        "alice-hash"
    );

    drop(store);
    let target_dir = tempfile::tempdir().unwrap();
    import(target_dir.path(), &snapshot_path).unwrap();
    let imported = SqliteStore::open(&database(target_dir.path())).unwrap();

    let imported_alice = imported.find_user("alice").unwrap().unwrap();
    assert_eq!(imported_alice.id, alice.id);
    assert_eq!(imported_alice.authz_id, alice.authz_id);
    assert_eq!(imported_alice.password_hash, "alice-hash");
    assert_eq!(imported_alice.password_salt, "alice-salt");
    assert_eq!(imported_alice.hash_type, "sha1+bcrypt");
    assert_eq!(imported_alice.created_at, alice.created_at);
    assert_eq!(imported_alice.updated_at, alice.updated_at);

    let imported_pivotal = imported.find_user("pivotal").unwrap().unwrap();
    assert!(imported_pivotal.admin);
    assert_eq!(imported_pivotal.id, pivotal.id);

    let imported_acme = imported.find_org("acme").unwrap().unwrap();
    assert_eq!(imported_acme.id, acme.id);
    assert_eq!(imported_acme.authz_id, acme.authz_id);
    assert_eq!(imported_acme.full_name, "Acme Corp");
    assert_eq!(imported_acme.created_at, acme.created_at);
    assert_eq!(imported.find_org("beta").unwrap().unwrap().id, beta.id);
    assert_eq!(imported.list_orgs().unwrap().len(), 2);
    assert_eq!(imported.list_users().unwrap().len(), 3);
    assert_eq!(
        imported
            .list_org_users(&imported_acme.id)
            .unwrap()
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice"]
    );

    let imported_web = imported
        .find_client(&imported_acme.id, "web")
        .unwrap()
        .unwrap();
    assert_eq!(imported_web.id, web.id);
    assert_eq!(imported_web.authz_id, web.authz_id);
    assert!(!imported_web.validator);
    let imported_ci = imported.find_client(&beta.id, "ci").unwrap().unwrap();
    assert_eq!(imported_ci.id, ci.id);
    assert!(imported_ci.validator);

    let user_key = imported
        .find_actor_key(&alice.id, ActorType::User, "default")
        .unwrap()
        .unwrap();
    assert_eq!(user_key.public_key, PUBLIC_KEY);
    let client_key = imported
        .find_actor_key(&web.id, ActorType::Client, "default")
        .unwrap()
        .unwrap();
    assert_eq!(client_key.public_key, PUBLIC_KEY);

    let imported_node = imported.find_node(&acme.id, "web1").unwrap().unwrap();
    assert_eq!(imported_node.id, node.id);
    assert_eq!(imported_node.authz_id, node.authz_id);
    assert_eq!(imported_node.serialized_object, NODE_BODY);
    assert_eq!(imported_node.created_at, node.created_at);
    assert!(imported
        .check_permission(&bob.authz_id, &imported_node.authz_id, Permission::Read)
        .unwrap());
    assert!(!imported
        .check_permission(&bob.authz_id, &imported_node.authz_id, Permission::Create)
        .unwrap());
    // The nested group (bob -> ops-nested -> ops) and its ACL survive.
    assert!(imported
        .check_permission(&bob.authz_id, &imported_node.authz_id, Permission::Update)
        .unwrap());

    let imported_role = imported.find_role(&acme.id, "web").unwrap().unwrap();
    assert_eq!(imported_role.id, role.id);
    assert_eq!(imported_role.authz_id, role.authz_id);
    assert_eq!(imported_role.serialized_object, ROLE_BODY);
    let imported_environment = imported
        .find_environment(&acme.id, "production")
        .unwrap()
        .unwrap();
    assert_eq!(imported_environment.id, environment.id);
    assert_eq!(imported_environment.serialized_object, ENVIRONMENT_BODY);
    assert_eq!(
        imported
            .find_data_bag(&acme.id, "secrets")
            .unwrap()
            .unwrap()
            .id,
        data_bag.id
    );
    let imported_item = imported
        .find_data_bag_item(&acme.id, "secrets", "database")
        .unwrap()
        .unwrap();
    assert_eq!(imported_item.id, data_bag_item.id);
    assert_eq!(imported_item.serialized_object, DATA_BAG_ITEM_BODY);
    assert!(imported.checksum_exists(&acme.id, CHECKSUM).unwrap());

    let imported_cookbook = imported
        .find_cookbook_version(&acme.id, "app", "1.2.3")
        .unwrap()
        .unwrap();
    assert_eq!(imported_cookbook.id, cookbook.id);
    assert_eq!(imported_cookbook.authz_id, cookbook.authz_id);
    assert_eq!(imported_cookbook.manifest, COOKBOOK_MANIFEST);
    assert_eq!(imported_cookbook.checksums, vec![CHECKSUM.to_string()]);
    assert_eq!(imported_cookbook.created_at, cookbook.created_at);

    let imported_policy = imported.find_policy(&acme.id, "app").unwrap().unwrap();
    assert_eq!(imported_policy.id, source_policy.id);
    assert_eq!(imported_policy.authz_id, source_policy.authz_id);
    let imported_revision = imported
        .find_policy_revision(&acme.id, "app", "rev1")
        .unwrap()
        .unwrap();
    assert_eq!(imported_revision.id, source_revision.id);
    assert_eq!(imported_revision.serialized_object, POLICY_BODY);
    let association = imported
        .find_policy_group_association(&acme.id, "production", "app")
        .unwrap()
        .unwrap();
    assert_eq!(association.id, source_association.id);
    assert_eq!(association.revision_id, "rev1");
    assert_eq!(association.serialized_object, POLICY_BODY);

    let imported_oauth = imported.list_oauth_applications().unwrap();
    assert_eq!(imported_oauth.len(), 1);
    assert_eq!(imported_oauth[0].id, oauth_application.id);
    assert_eq!(imported_oauth[0].uid, "supermarket-uid");

    // Re-importing is safe and does not duplicate rows.
    import(target_dir.path(), &snapshot_path).unwrap();
    let imported_again = SqliteStore::open(&database(target_dir.path())).unwrap();
    assert_eq!(imported_again.list_users().unwrap().len(), 3);
    assert_eq!(imported_again.list_orgs().unwrap().len(), 2);
    assert_eq!(imported_again.list_nodes(&acme.id).unwrap().len(), 1);
    assert_eq!(
        imported_again
            .list_cookbook_versions(&acme.id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn export_never_contains_plaintext_passwords_or_private_keys() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(dir.path())).unwrap();
    store
        .create_user(
            "alice",
            "alice@example.com",
            "bcrypt",
            "$2b$12$stored-hash",
            "stored-salt",
            false,
        )
        .unwrap();
    // Simulate key and password files living in the data directory. The
    // exporter must only read database rows, never adjacent secret material.
    std::fs::write(dir.path().join("alice.pem"), PRIVATE_KEY).unwrap();
    std::fs::write(dir.path().join("pivotal.password"), "super-secret-login").unwrap();

    let snapshot_path = dir.path().join("snapshot.json");
    export(dir.path(), &snapshot_path).unwrap();
    let text = std::fs::read_to_string(&snapshot_path).unwrap();

    assert!(!text.contains("BEGIN RSA PRIVATE KEY"));
    assert!(!text.contains("BEGIN PRIVATE KEY"));
    assert!(!text.contains("private_key"));
    assert!(!text.contains("super-secret-login"));
    assert!(!text.contains("password\":"));
    assert!(text.contains("$2b$12$stored-hash"));
    assert!(text.contains("stored-salt"));
    assert!(text.contains("\"hash_type\": \"bcrypt\""));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&snapshot_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn rejects_malformed_and_unsupported_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let snapshot_path = dir.path().join("snapshot.json");

    std::fs::write(&snapshot_path, "{ not json").unwrap();
    let error = import(dir.path(), &snapshot_path).unwrap_err();
    assert!(matches!(error, MigrateError::InvalidJson(_)));

    std::fs::write(
        &snapshot_path,
        r#"{"format":"microwave-server-snapshot","version":999,"generated_at":"2026-10-24T00:00:00Z","source":{"kind":"local-store"},"data":{}}"#,
    )
    .unwrap();
    let error = import(dir.path(), &snapshot_path).unwrap_err();
    assert!(matches!(
        error,
        MigrateError::UnsupportedVersion {
            found: 999,
            supported: 1
        }
    ));

    std::fs::write(
        &snapshot_path,
        r#"{"format":"some-other-tool","version":1,"generated_at":"2026-10-24T00:00:00Z","source":{"kind":"local-store"},"data":{}}"#,
    )
    .unwrap();
    let error = import(dir.path(), &snapshot_path).unwrap_err();
    assert!(matches!(error, MigrateError::UnsupportedFormat(_)));
}

#[test]
fn from_api_reports_a_clear_not_implemented_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = microwave_migrate::from_api(
        "https://chef.example.com",
        "migration-client",
        &dir.path().join("migration-client.pem"),
        &dir.path().join("snapshot.json"),
    )
    .unwrap_err();
    match error {
        MigrateError::NotImplemented(message) => {
            assert!(message.contains("from-api"));
            assert!(message.contains("not implemented"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
