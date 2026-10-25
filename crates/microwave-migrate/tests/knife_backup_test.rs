//! Round-trip coverage for the `knife ec backup` directory importer.
//!
//! The fixture is synthetic but follows the real ChefFS layout: users and user
//! ACLs, optional SQL key dumps, organization objects, membership and
//! invitations, clients/containers/groups, named objects, data bags, versioned
//! cookbook directories with `status.json`, and per-object ACL directories.

use microwave_migrate::{
    import_knife_backup_postgres_with_compression, import_knife_backup_to, prepare_knife_backup,
    Compression,
};
use microwave_objectstore::{object_path, LocalObjectStore, ObjectStore};
use microwave_store::{ActorType, Permission, SqliteStore};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");

const NODE_BODY: &str = r#"{"name":"web1","json_class":"Chef::Node","chef_type":"node","chef_environment":"production","run_list":["role[web]"],"normal":{"tags":["blue"]}}"#;
const ROLE_BODY: &str = r#"{"name":"web","json_class":"Chef::Role","chef_type":"role","run_list":["recipe[app::default]"],"default_attributes":{},"override_attributes":{}}"#;
const ENVIRONMENT_BODY: &str = r#"{"name":"production","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{"app":"= 1.2.3"},"default_attributes":{"tier":"prod"},"override_attributes":{}}"#;
const DATA_BAG_ITEM_BODY: &str =
    r#"{"id":"database","host":"db.example.com","password":"not-logged"}"#;
const METADATA_RB: &str = r#"name 'app'
maintainer 'Example'
maintainer_email 'ops@example.com'
license 'Apache-2.0'
description 'Example cookbook'
version '1.2.3'
long_description 'A long description'
depends 'apt', '~> 7.0'
supports 'ubuntu', '>= 20.04'
"#;
const RECIPE: &str = "log 'hello from the fixture'\n";
const COOKBOOK_FILE: &str = "fixture file contents\n";

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn write_text(path: &Path, value: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, value).unwrap();
}

fn acl(actors: &[&str], groups: &[&str]) -> Value {
    json!({ "actors": actors, "groups": groups })
}

fn acl_document(read_actors: &[&str], read_groups: &[&str]) -> Value {
    json!({
        "create": acl(read_actors, read_groups),
        "read": acl(read_actors, read_groups),
        "update": acl(read_actors, read_groups),
        "delete": acl(read_actors, read_groups),
        "grant": acl(read_actors, read_groups)
    })
}

/// Create a synthetic backup directory and return its path.
fn fixture(root: &Path, suffix: &str) -> PathBuf {
    let org = format!("acme-{suffix}");
    let pivotal = format!("pivotal-{suffix}");
    let alice = format!("alice-{suffix}");
    let bob = format!("bob-{suffix}");
    let backup = root.join(format!("backup-{suffix}"));
    let org_dir = backup.join("organizations").join(&org);

    write_json(
        &backup.join("users/pivotal.json"),
        &json!({
            "username": pivotal.clone(),
            "email": format!("{pivotal}@example.com"),
            "display_name": "Pivotal",
            "first_name": "Piv",
            "last_name": "Otal",
            "public_key": PUBLIC_KEY,
        }),
    );
    write_json(
        &backup.join("users/alice.json"),
        &json!({
            "username": alice.clone(),
            "email": format!("{alice}@example.com"),
            "display_name": "Alice",
            "recovery_authentication_enabled": true,
            "external_authentication_uid": format!("ldap-{alice}"),
            "public_key": PUBLIC_KEY,
        }),
    );
    write_json(
        &backup.join("users/bob.json"),
        &json!({
            "username": bob.clone(),
            "email": format!("{bob}@example.com"),
        }),
    );
    write_json(
        &backup.join(format!("user_acls/{pivotal}.json")),
        &acl_document(&[pivotal.as_str()], &["admins", "users"]),
    );
    write_json(
        &backup.join(format!("user_acls/{alice}.json")),
        &acl_document(&[], &["admins"]),
    );

    write_json(
        &backup.join("key_dump.json"),
        &json!([
            {
                "id": format!("pivotal-id-{suffix}"),
                "authz_id": format!("pivotal-authz-{suffix}"),
                "username": pivotal.clone(),
                "email": format!("{pivotal}@example.com"),
                "hashed_password": "pivotal-hash",
                "salt": "pivotal-salt",
                "hash_type": "bcrypt",
                "admin": true,
            },
            {
                "id": format!("alice-id-{suffix}"),
                "authz_id": format!("alice-authz-{suffix}"),
                "username": alice.clone(),
                "email": format!("{alice}@example.com"),
                "hashed_password": "alice-hash",
                "salt": "alice-salt",
                "hash_type": "sha1+bcrypt",
                "admin": false,
            },
            {
                "username": bob.clone(),
                "hashed_password": "bob-hash",
                "salt": "bob-salt",
                "hash_type": "bcrypt",
                "admin": false,
            }
        ]),
    );
    write_json(
        &backup.join("key_table_dump.json"),
        &json!([
            {
                "id": format!("alice-id-{suffix}"),
                "name": alice.clone(),
                "type": "user",
                "key_name": "workstation",
                "public_key": PUBLIC_KEY,
                "key_version": 0,
                "expires_at": "infinity",
            },
            {
                "id": format!("web-id-{suffix}"),
                "org_id": format!("{org}-id"),
                "org_name": &org,
                "name": "web",
                "type": "client",
                "key_name": "deploy",
                "public_key": PUBLIC_KEY,
                "key_version": 1,
                "expires_at": "infinity",
            }
        ]),
    );

    write_json(
        &org_dir.join("org.json"),
        &json!({
            "name": &org,
            "full_name": "Acme Corporation",
            "guid": format!("{suffix}-guid"),
        }),
    );
    write_json(
        &org_dir.join("members.json"),
        &json!([
            { "user": { "username": pivotal.clone() } },
            { "user": { "username": alice.clone() } }
        ]),
    );
    write_json(
        &org_dir.join("invitations.json"),
        &json!([
            { "id": format!("invite-{suffix}"), "username": bob.clone() }
        ]),
    );

    write_json(
        &org_dir.join("clients/web.json"),
        &json!({
            "name": "web",
            "validator": false,
            "public_key": PUBLIC_KEY,
        }),
    );
    write_json(
        &org_dir.join("clients/chef-validator.json"),
        &json!({
            "name": "chef-validator",
            "validator": true,
            "public_key": PUBLIC_KEY,
        }),
    );

    for container in [
        "nodes",
        "roles",
        "environments",
        "data",
        "cookbooks",
        "groups",
    ] {
        write_json(
            &org_dir.join("containers").join(format!("{container}.json")),
            &json!({ "containername": container }),
        );
    }

    write_json(
        &org_dir.join("groups/admins.json"),
        &json!({
            "groupname": "admins",
            "actors": [pivotal.clone()],
            "groups": ["users"]
        }),
    );
    write_json(
        &org_dir.join("groups/users.json"),
        &json!({
            "groupname": "users",
            "users": [alice.clone()]
        }),
    );
    write_json(
        &org_dir.join("groups/clients.json"),
        &json!({
            "groupname": "clients",
            "clients": ["web", "chef-validator"]
        }),
    );
    write_json(
        &org_dir.join("groups/ops.json"),
        &json!({
            "groupname": "ops",
            "users": [alice.clone()],
            "groups": ["users"]
        }),
    );

    write_json(
        &org_dir.join("nodes/web1.json"),
        &serde_json::from_str(NODE_BODY).unwrap(),
    );
    write_json(
        &org_dir.join("roles/web.json"),
        &serde_json::from_str(ROLE_BODY).unwrap(),
    );
    write_json(
        &org_dir.join("environments/production.json"),
        &serde_json::from_str(ENVIRONMENT_BODY).unwrap(),
    );
    write_json(
        &org_dir.join("data_bags/secrets/database.json"),
        &serde_json::from_str(DATA_BAG_ITEM_BODY).unwrap(),
    );

    let cookbook = org_dir.join("cookbooks/app-1.2.3");
    write_text(&cookbook.join("metadata.rb"), METADATA_RB);
    write_text(&cookbook.join("status.json"), "{\"frozen\":true}\n");
    write_text(&cookbook.join("recipes/default.rb"), RECIPE);
    write_text(&cookbook.join("files/default/fixture.txt"), COOKBOOK_FILE);

    for (kind, name) in [
        ("nodes", "web1"),
        ("roles", "web"),
        ("environments", "production"),
        ("data_bags", "secrets"),
        ("cookbooks", "app"),
        ("groups", "admins"),
        ("containers", "nodes"),
        ("clients", "web"),
    ] {
        write_json(
            &org_dir.join("acls").join(kind).join(format!("{name}.json")),
            &acl_document(&[alice.as_str(), pivotal.as_str()], &["admins", "ops"]),
        );
    }
    write_json(
        &org_dir.join("acls/organization.json"),
        &acl_document(&[pivotal.as_str()], &["admins", "users"]),
    );

    backup
}

fn database(dir: &Path) -> PathBuf {
    dir.join("microwave.db")
}

fn read_cookbook_manifest(store: &SqliteStore, org_id: &str) -> Value {
    let version = store
        .find_cookbook_version(org_id, "app", "1.2.3")
        .unwrap()
        .expect("cookbook version");
    serde_json::from_str(&version.manifest).unwrap()
}

#[test]
fn imports_users_keys_orgs_objects_cookbooks_and_acls() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "roundtrip");
    let plan = prepare_knife_backup(&backup).unwrap();
    assert_eq!(plan.summary.users, 3);
    assert_eq!(plan.summary.organizations, 1);
    assert_eq!(plan.summary.clients, 2);
    assert_eq!(plan.summary.cookbook_versions, 1);
    assert_eq!(plan.summary.cookbook_files, 3);
    assert_eq!(plan.summary.checksums, 3);
    assert_eq!(plan.blobs.len(), 3);
    assert_eq!(plan.summary.cookbook_files, plan.summary.checksums);

    let data = tempfile::tempdir().unwrap();
    let summary = import_knife_backup_to(
        data.path(),
        &backup,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice_name = "alice-roundtrip";
    let pivotal_name = "pivotal-roundtrip";
    let bob_name = "bob-roundtrip";
    let alice = store.find_user(alice_name).unwrap().unwrap();
    assert_eq!(alice.password_hash, "alice-hash");
    assert_eq!(alice.password_salt, "alice-salt");
    assert_eq!(alice.hash_type, "sha1+bcrypt");
    assert!(!alice.admin);
    assert!(alice.recovery_authentication_enabled);
    assert_eq!(
        alice.external_authentication_uid.as_deref(),
        Some("ldap-alice-roundtrip")
    );
    let pivotal = store.find_user(pivotal_name).unwrap().unwrap();
    assert!(pivotal.admin);
    assert_eq!(pivotal.password_hash, "pivotal-hash");

    let alice_keys = store.find_actor_keys(&alice.id, ActorType::User).unwrap();
    let names: Vec<_> = alice_keys.iter().map(|key| key.key_name.as_str()).collect();
    assert!(names.contains(&"default"));
    assert!(names.contains(&"workstation"));
    let pivotal_key = store
        .find_actor_key(&pivotal.id, ActorType::User, "default")
        .unwrap()
        .expect("pivotal default key");
    assert_eq!(pivotal_key.public_key, PUBLIC_KEY);

    let org = store.find_org("acme-roundtrip").unwrap().unwrap();
    assert_eq!(org.id, "roundtrip-guid");
    assert_eq!(org.full_name, "Acme Corporation");
    let web = store.find_client(&org.id, "web").unwrap().unwrap();
    assert!(!web.validator);
    let validator = store
        .find_client(&org.id, "chef-validator")
        .unwrap()
        .unwrap();
    assert!(validator.validator);
    let web_default = store
        .find_actor_key(&web.id, ActorType::Client, "default")
        .unwrap()
        .expect("inline client key");
    assert_eq!(web_default.public_key, PUBLIC_KEY);
    let deploy = store
        .find_actor_key(&web.id, ActorType::Client, "deploy")
        .unwrap()
        .expect("key table client key");
    assert_eq!(deploy.public_key, PUBLIC_KEY);

    let members: Vec<_> = store
        .list_org_users(&org.id)
        .unwrap()
        .into_iter()
        .map(|user| user.username)
        .collect();
    assert_eq!(members, vec![alice_name, pivotal_name]);
    let invitations = store.list_org_invitations(&org.id).unwrap();
    assert_eq!(invitations.len(), 1);
    assert_eq!(invitations[0].id, "invite-roundtrip");
    assert_eq!(
        invitations[0].user_id,
        store.find_user(bob_name).unwrap().unwrap().id
    );

    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&node.serialized_object).unwrap(),
        serde_json::from_str::<Value>(NODE_BODY).unwrap()
    );
    assert_eq!(node.environment, "production");
    let role = store.find_role(&org.id, "web").unwrap().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&role.serialized_object).unwrap(),
        serde_json::from_str::<Value>(ROLE_BODY).unwrap()
    );
    let environment = store
        .find_environment(&org.id, "production")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&environment.serialized_object).unwrap(),
        serde_json::from_str::<Value>(ENVIRONMENT_BODY).unwrap()
    );
    let item = store
        .find_data_bag_item(&org.id, "secrets", "database")
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&item.serialized_object).unwrap(),
        serde_json::from_str::<Value>(DATA_BAG_ITEM_BODY).unwrap()
    );

    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    let ops_members = store.list_group_members(&org.id, &ops.authz_id).unwrap();
    assert_eq!(ops_members.users, vec![alice_name]);
    assert_eq!(ops_members.groups, vec!["users"]);
    let admins = store.find_group(&org.id, "admins").unwrap().unwrap();
    let admin_members = store.list_group_members(&org.id, &admins.authz_id).unwrap();
    assert_eq!(admin_members.users, vec![pivotal_name]);
    assert_eq!(admin_members.groups, vec!["users"]);

    let node_acl = store.get_acl_entries(&node.authz_id).unwrap();
    let node_read = node_acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert_eq!(node_read.users, vec![alice_name, pivotal_name]);
    assert!(node_read.clients.is_empty());
    assert!(node_read.groups.contains(&"admins".to_string()));
    assert!(node_read.groups.contains(&"ops".to_string()));
    assert!(store
        .check_permission(&alice.authz_id, &node.authz_id, Permission::Read)
        .unwrap());

    let user_acl = store.get_acl_entries(&pivotal.authz_id).unwrap();
    let user_read = user_acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert!(user_read.groups.contains(&"::server-admins".to_string()));
    assert!(user_read.groups.contains(&"::users".to_string()));

    let cookbook = store
        .find_cookbook_version(&org.id, "app", "1.2.3")
        .unwrap()
        .unwrap();
    assert!(cookbook.frozen);
    assert_eq!(cookbook.checksums.len(), 3);
    let manifest = read_cookbook_manifest(&store, &org.id);
    assert_eq!(manifest["cookbook_name"], "app");
    assert_eq!(manifest["version"], "1.2.3");
    assert_eq!(manifest["frozen?"], true);
    assert_eq!(manifest["metadata"]["dependencies"]["apt"], "~> 7.0");
    assert_eq!(manifest["metadata"]["platforms"]["ubuntu"], ">= 20.04");
    assert_eq!(
        manifest["metadata"]["long_description"],
        "A long description"
    );
    assert_eq!(manifest["recipes"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["root_files"].as_array().unwrap().len(), 1);
    assert_eq!(manifest["files"].as_array().unwrap().len(), 1);

    let bookshelf = LocalObjectStore::new(data.path().join("bookshelf"), "", "http://localhost");
    for checksum in &cookbook.checksums {
        let bytes = bookshelf.get(&org.id, checksum).unwrap();
        assert!(!bytes.is_empty());
        assert!(object_path(&data.path().join("bookshelf"), &org.id, checksum).is_file());
    }

    // The same backup is idempotent: a second import does not duplicate rows.
    let summary_again = import_knife_backup_to(
        data.path(),
        &backup,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap();
    assert!(summary_again.warnings.is_empty());
    let store = SqliteStore::open(&database(data.path())).unwrap();
    assert_eq!(store.list_users().unwrap().len(), 3);
    assert_eq!(store.list_orgs().unwrap().len(), 1);
    assert_eq!(store.list_cookbook_versions(&org.id).unwrap().len(), 1);
}

#[test]
fn reuses_an_existing_global_server_admins_group() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "merge");
    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store.ensure_global_group("server-admins").unwrap();
    drop(store);

    let summary = import_knife_backup_to(
        data.path(),
        &backup,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let group = store
        .find_global_group("server-admins")
        .unwrap()
        .expect("server-admins");
    assert_eq!(group.authz_id, existing.authz_id);
    assert_eq!(
        store
            .list_global_groups()
            .unwrap()
            .iter()
            .filter(|group| group.name == "server-admins")
            .count(),
        1
    );
    let pivotal = store.find_user("pivotal-merge").unwrap().unwrap();
    assert!(store
        .check_permission(&pivotal.authz_id, &pivotal.authz_id, Permission::Read)
        .unwrap());
    let acl = store.get_acl_entries(&pivotal.authz_id).unwrap();
    let read = acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert!(read.groups.contains(&"::server-admins".to_string()));
}

#[test]
fn dry_run_parsing_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "dryrun");
    let plan = prepare_knife_backup(&backup).unwrap();
    assert_eq!(plan.summary.organizations, 1);
    assert!(!root.path().join("microwave.db").exists());
    assert!(!root.path().join("bookshelf").exists());
}

#[test]
fn rejects_a_backup_that_is_not_a_knife_repository() {
    let root = tempfile::tempdir().unwrap();
    let error = prepare_knife_backup(root.path()).unwrap_err().to_string();
    assert!(error.contains("users/ or organizations/"));
}

#[test]
fn postgres_import_round_trip() {
    use microwave_store::{
        AuthzRepository, CookbookRepository, IdentityRepository, ObjectRepository,
    };

    let Some(url) = std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
    else {
        eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
        return;
    };
    let suffix = format!(
        "pg-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), &suffix);
    let bookshelf = root.path().join("bookshelf");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    // Simulate a real target that already has the global server-admins group
    // from another installation. The importer must reuse it, not collide on
    // its unique name.
    let seeding = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let existing_server_admins = runtime
        .block_on(seeding.ensure_global_group("server-admins"))
        .expect("ensure server-admins");
    drop(seeding);

    let summary =
        import_knife_backup_postgres_with_compression(&url, &backup, &bookshelf, Compression::None)
            .unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);

    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let server_admins = runtime
        .block_on(store.find_global_group("server-admins"))
        .unwrap()
        .expect("server-admins group");
    assert_eq!(server_admins.authz_id, existing_server_admins.authz_id);
    let org_name = format!("acme-{suffix}");
    let alice_name = format!("alice-{suffix}");
    let user = runtime
        .block_on(store.find_user(&alice_name))
        .unwrap()
        .expect("imported user");
    assert_eq!(user.password_hash, "alice-hash");
    let org = runtime
        .block_on(store.find_org(&org_name))
        .unwrap()
        .expect("imported org");
    let client = runtime
        .block_on(store.find_client(&org.id, "web"))
        .unwrap()
        .expect("imported client");
    assert_eq!(client.name, "web");
    let node = runtime
        .block_on(store.find_node(&org.id, "web1"))
        .unwrap()
        .expect("imported node");
    assert_eq!(
        serde_json::from_str::<Value>(&node.serialized_object).unwrap(),
        serde_json::from_str::<Value>(NODE_BODY).unwrap()
    );
    assert!(runtime
        .block_on(store.check_permission(&user.authz_id, &node.authz_id, Permission::Read))
        .unwrap());
    let cookbook = runtime
        .block_on(store.find_cookbook_version(&org.id, "app", "1.2.3"))
        .unwrap()
        .expect("imported cookbook");
    assert!(cookbook.frozen);
    assert_eq!(cookbook.checksums.len(), 3);
    let books = LocalObjectStore::new(&bookshelf, "", "http://localhost");
    for checksum in cookbook.checksums {
        assert!(books.exists(&org.id, &checksum));
    }
}
