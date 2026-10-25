//! Round-trip coverage for the `knife ec backup` directory importer.
//!
//! The fixture is synthetic but follows the real ChefFS layout: users and user
//! ACLs, optional SQL key dumps, organization objects, membership and
//! invitations, clients/containers/groups, named objects, data bags, versioned
//! cookbook directories with `status.json`, and per-object ACL directories.

use microwave_migrate::{
    apply_knife_backup_plan_postgres_with_options, apply_knife_backup_plan_sqlite,
    apply_knife_backup_plan_sqlite_with_options, import_knife_backup_to, prepare_knife_backup,
    Compression, KnifeBackupApplyOptions,
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
                "created_at": "2020-01-02 03:04:05 UTC",
                "updated_at": "2020-02-03 04:05:06 UTC",
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

fn postgres_url() -> Option<String> {
    let required = std::env::var("MICROWAVE_REQUIRE_POSTGRES")
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true"));
    match std::env::var("MICROWAVE_TEST_POSTGRES_URL") {
        Ok(url) if !url.trim().is_empty() => Some(url),
        Ok(_) if required => {
            panic!("MICROWAVE_TEST_POSTGRES_URL is empty but MICROWAVE_REQUIRE_POSTGRES is set")
        }
        Ok(_) => {
            eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is empty");
            None
        }
        Err(std::env::VarError::NotPresent) if required => {
            panic!("MICROWAVE_REQUIRE_POSTGRES is set but MICROWAVE_TEST_POSTGRES_URL is not")
        }
        Err(std::env::VarError::NotPresent) => {
            eprintln!("SKIP: MICROWAVE_TEST_POSTGRES_URL is not set");
            None
        }
        Err(error) => panic!("MICROWAVE_TEST_POSTGRES_URL is invalid: {error}"),
    }
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
    assert_eq!(alice.created_at, "2020-01-02T03:04:05Z");
    assert_eq!(alice.updated_at, "2020-02-03T04:05:06Z");
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
    let global_users = store.find_global_group("users").unwrap().unwrap();
    let global_user_members = store
        .list_group_members("", &global_users.authz_id)
        .unwrap();
    assert_eq!(
        global_user_members.users,
        vec![alice_name, bob_name, pivotal_name]
    );

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
fn rejects_metadata_long_description_paths_outside_the_cookbook() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "metadata-traversal");
    write_text(
        &root.path().join("outside-README.txt"),
        "outside the cookbook\n",
    );
    let cookbook = backup.join("organizations/acme-metadata-traversal/cookbooks/app-1.2.3");
    write_text(
        &cookbook.join("metadata.rb"),
        "name 'app'\nversion '1.2.3'\nlong_description IO.read(File.join(File.dirname(__FILE__), '../outside-README.txt'))\n",
    );
    let error = prepare_knife_backup(&backup).unwrap_err().to_string();
    assert!(error.contains("not a contained relative path"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_metadata_long_description_file() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "metadata-symlink");
    let outside = root.path().join("outside-readme.md");
    write_text(&outside, "outside the cookbook\n");
    let cookbook = backup.join("organizations/acme-metadata-symlink/cookbooks/app-1.2.3");
    write_text(
        &cookbook.join("metadata.rb"),
        "name 'app'\nversion '1.2.3'\nlong_description IO.read(File.join(File.dirname(__FILE__), 'README.md'))\n",
    );
    symlink(&outside, cookbook.join("README.md")).unwrap();
    let error = prepare_knife_backup(&backup).unwrap_err().to_string();
    assert!(error.contains("symlink"), "{error}");
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_cookbooks_collection() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "symlink-cookbooks");
    let org_dir = backup.join("organizations/acme-symlink-cookbooks");
    std::fs::remove_dir_all(org_dir.join("cookbooks")).unwrap();
    let outside = root.path().join("outside-cookbooks");
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, org_dir.join("cookbooks")).unwrap();
    let error = prepare_knife_backup(&backup).unwrap_err().to_string();
    assert!(error.contains("symlink"), "{error}");
}

#[test]
fn normalizes_legacy_key_expirations_and_skips_invalid_rows() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "expiry");
    write_json(
        &backup.join("key_table_dump.json"),
        &json!([
            {
                "name": "alice-expiry",
                "type": "user",
                "key_name": "legacy",
                "public_key": PUBLIC_KEY,
                "key_version": 1,
                "expires_at": "2030-01-02 03:04:05 UTC"
            },
            {
                "name": "alice-expiry",
                "type": "user",
                "key_name": "offset",
                "public_key": PUBLIC_KEY,
                "key_version": 1,
                "expires_at": "2030-01-02 03:04:05 +0000"
            },
            {
                "name": "alice-expiry",
                "type": "user",
                "key_name": "rfc",
                "public_key": PUBLIC_KEY,
                "key_version": 1,
                "expires_at": "2031-04-05T06:07:08Z"
            },
            {
                "name": "alice-expiry",
                "type": "user",
                "key_name": "bad",
                "public_key": PUBLIC_KEY,
                "key_version": 1,
                "expires_at": "not-a-time"
            }
        ]),
    );
    let data = tempfile::tempdir().unwrap();
    let summary = import_knife_backup_to(
        data.path(),
        &backup,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap();
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("invalid expires_at")));

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-expiry").unwrap().unwrap();
    for (key_name, expected) in [
        ("legacy", "2030-01-02T03:04:05Z"),
        ("offset", "2030-01-02T03:04:05Z"),
        ("rfc", "2031-04-05T06:07:08Z"),
    ] {
        let key = store
            .find_actor_key(&alice.id, ActorType::User, key_name)
            .unwrap()
            .unwrap_or_else(|| panic!("missing key {key_name}"));
        assert_eq!(key.expires_at, expected);
    }
    assert!(store
        .find_actor_key(&alice.id, ActorType::User, "bad")
        .unwrap()
        .is_none());
}

#[test]
fn refuses_a_bootstrapped_user_without_merge_users_and_reuses_it_with_merge() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "merge-user");
    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-merge-user",
            "existing@example.com",
            "bcrypt",
            "existing-hash",
            "existing-salt",
            false,
        )
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    let error = apply_knife_backup_plan_sqlite(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("--merge-users"), "{error}");

    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: true,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let imported = store.find_user("pivotal-merge-user").unwrap().unwrap();
    assert_eq!(imported.id, existing.id);
    assert_eq!(imported.authz_id, existing.authz_id);
    assert_eq!(imported.password_hash, "pivotal-hash");
    let key = store
        .find_actor_key(&existing.id, ActorType::User, "default")
        .unwrap()
        .expect("merged user key");
    assert_eq!(key.actor_id, existing.id);
    assert_eq!(key.public_key, PUBLIC_KEY);
    let org = store.find_org("acme-merge-user").unwrap().unwrap();
    assert!(store
        .list_org_users(&org.id)
        .unwrap()
        .iter()
        .any(|user| user.id == existing.id));
}

#[test]
fn replace_acls_removes_revoked_grants_and_group_members() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "replace");
    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let org = store.find_org("acme-replace").unwrap().unwrap();
    let alice = store.find_user("alice-replace").unwrap().unwrap();
    let bob = store.find_user("bob-replace").unwrap().unwrap();
    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    store
        .set_acl(
            &node.authz_id,
            Permission::Create,
            Some(&bob.authz_id),
            None,
        )
        .unwrap();
    store
        .add_group_member(&ops.authz_id, &bob.authz_id)
        .unwrap();
    let local_only = store
        .create_user(
            "local-only-replace",
            "local-only-replace@example.com",
            "bcrypt",
            "h",
            "s",
            false,
        )
        .unwrap();
    let global_users = store.find_global_group("users").unwrap().unwrap();
    store
        .add_group_member(&global_users.authz_id, &local_only.authz_id)
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite(data.path(), &plan, &bookshelf, Compression::None).unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    let create = store
        .get_acl_entries(&node.authz_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.permission == Permission::Create)
        .unwrap();
    assert!(create.users.contains(&bob.username) || create.clients.contains(&bob.username));
    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    assert!(store
        .list_group_members(&org.id, &ops.authz_id)
        .unwrap()
        .users
        .contains(&bob.username));
    drop(store);

    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: false,
            replace_acls: true,
            replace_user_credentials: false,
        },
    )
    .unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    let create = store
        .get_acl_entries(&node.authz_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.permission == Permission::Create)
        .unwrap();
    assert!(!create.users.contains(&bob.username));
    assert!(!create.clients.contains(&bob.username));
    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    assert_eq!(
        store
            .list_group_members(&org.id, &ops.authz_id)
            .unwrap()
            .users,
        vec![alice.username]
    );
    let global_users = store.find_global_group("users").unwrap().unwrap();
    assert!(store
        .list_group_members("", &global_users.authz_id)
        .unwrap()
        .users
        .contains(&local_only.username));
}

#[test]
fn skips_empty_global_group_names() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "empty-group");
    write_json(
        &backup.join("user_acls/pivotal-empty-group.json"),
        &json!({
            "read": { "actors": ["pivotal-empty-group"], "groups": ["", "::", "admins"] },
            "create": { "actors": [], "groups": [] },
            "update": { "actors": [], "groups": [] },
            "delete": { "actors": [], "groups": [] },
            "grant": { "actors": [], "groups": [] }
        }),
    );
    let plan = prepare_knife_backup(&backup).unwrap();
    assert!(plan
        .snapshot
        .global_groups
        .iter()
        .all(|group| !group.name.is_empty()));
    assert!(plan
        .snapshot
        .global_groups
        .iter()
        .any(|group| group.name == "server-admins"));
    assert!(!plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("unknown group")));
}

#[test]
fn user_acl_strips_global_prefix_and_skips_unknown_globals() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "global-prefix");
    write_json(
        &backup.join("user_acls/pivotal-global-prefix.json"),
        &json!({
            "read": {
                "actors": ["pivotal-global-prefix"],
                "groups": ["::admins", "::users", "::not-a-real-group"]
            },
            "create": { "actors": [], "groups": [] },
            "update": { "actors": [], "groups": [] },
            "delete": { "actors": [], "groups": [] },
            "grant": { "actors": [], "groups": [] }
        }),
    );
    let plan = prepare_knife_backup(&backup).unwrap();
    let names: Vec<&str> = plan
        .snapshot
        .global_groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    assert!(names.contains(&"server-admins"));
    assert!(names.contains(&"users"));
    assert!(!names.contains(&"::admins"));
    assert!(!names.contains(&"::users"));
    assert!(!names.contains(&"not-a-real-group"));
    assert!(plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("unknown group")));
}

#[test]
fn failed_import_restores_existing_acls_and_group_membership() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "replace-fail");
    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let org = store.find_org("acme-replace-fail").unwrap().unwrap();
    let bob = store.find_user("bob-replace-fail").unwrap().unwrap();
    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    store
        .set_acl(
            &node.authz_id,
            Permission::Create,
            Some(&bob.authz_id),
            None,
        )
        .unwrap();
    store
        .add_group_member(&ops.authz_id, &bob.authz_id)
        .unwrap();
    drop(store);

    let mut plan = prepare_knife_backup(&backup).unwrap();
    plan.snapshot.nodes[0].id = "conflicting-node-id".into();
    let error = apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: false,
            replace_acls: true,
            replace_user_credentials: false,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(!error.is_empty());

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let node = store.find_node(&org.id, "web1").unwrap().unwrap();
    let create = store
        .get_acl_entries(&node.authz_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.permission == Permission::Create)
        .unwrap();
    assert!(create.users.contains(&bob.username));
    let ops = store.find_group(&org.id, "ops").unwrap().unwrap();
    assert!(store
        .list_group_members(&org.id, &ops.authz_id)
        .unwrap()
        .users
        .contains(&bob.username));
}

#[test]
fn merge_users_without_key_dump_preserves_target_credentials_and_default_key() {
    use microwave_store::UserProfile;

    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "nokey");
    let user_path = backup.join("users/pivotal.json");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&user_path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("public_key");
    write_json(&user_path, &value);
    std::fs::remove_file(backup.join("key_dump.json")).unwrap();
    std::fs::remove_file(backup.join("key_table_dump.json")).unwrap();

    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user_with_profile(
            "pivotal-nokey",
            Some("target@example.com"),
            "bcrypt",
            "target-hash",
            "target-salt",
            true,
            &UserProfile {
                display_name: Some("Target Admin".into()),
                first_name: Some("Target".into()),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .add_actor_key(
            &existing.id,
            ActorType::User,
            "default",
            "TARGET-DEFAULT-KEY",
        )
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    let summary = apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: false,
        },
    )
    .unwrap();
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("no imported key material")));

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let user = store.find_user("pivotal-nokey").unwrap().unwrap();
    assert_eq!(user.id, existing.id);
    assert_eq!(user.password_hash, "target-hash");
    assert_eq!(user.password_salt, "target-salt");
    assert!(user.admin);
    assert_eq!(user.display_name.as_deref(), Some("Target Admin"));
    let key = store
        .find_actor_key(&existing.id, ActorType::User, "default")
        .unwrap()
        .unwrap();
    assert_eq!(key.public_key, "TARGET-DEFAULT-KEY");
}

#[test]
fn merge_users_preserves_an_existing_target_default_key() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "merge-key");
    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-merge-key",
            "target-key@example.com",
            "bcrypt",
            "target-hash",
            "target-salt",
            false,
        )
        .unwrap();
    store
        .add_actor_key(
            &existing.id,
            ActorType::User,
            "default",
            "TARGET-ONLY-DEFAULT-KEY",
        )
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: false,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let key = store
        .find_actor_key(&existing.id, ActorType::User, "default")
        .unwrap()
        .expect("target default key");
    assert_eq!(key.public_key, "TARGET-ONLY-DEFAULT-KEY");
}

#[test]
fn invalid_default_key_expiry_suppresses_inline_default_key() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "bad-default");
    write_json(
        &backup.join("key_table_dump.json"),
        &json!([
            {
                "name": "pivotal-bad-default",
                "type": "user",
                "key_name": "default",
                "public_key": PUBLIC_KEY,
                "expires_at": "   "
            },
            {
                "name": "alice-bad-default",
                "type": "user",
                "key_name": "default",
                "public_key": PUBLIC_KEY,
                "expires_at": "not-a-time"
            }
        ]),
    );
    let data = tempfile::tempdir().unwrap();
    let summary = import_knife_backup_to(
        data.path(),
        &backup,
        &data.path().join("bookshelf"),
        Compression::None,
    )
    .unwrap();
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("invalid expires_at")));

    let store = SqliteStore::open(&database(data.path())).unwrap();
    for username in ["pivotal-bad-default", "alice-bad-default"] {
        let user = store.find_user(username).unwrap().unwrap();
        assert!(
            store
                .find_actor_key(&user.id, ActorType::User, "default")
                .unwrap()
                .is_none(),
            "{username} must not regain an inline infinity default key"
        );
    }
}

#[test]
fn redacted_plan_and_blob_debug_omit_key_and_file_bytes() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "debug");
    let plan = prepare_knife_backup(&backup).unwrap();
    let plan_debug = format!("{plan:?}");
    assert!(!plan_debug.contains("BEGIN PUBLIC KEY"));
    assert!(!plan_debug.contains("serialized_object"));
    let blob_debug = format!("{:?}", plan.blobs[0]);
    assert!(!blob_debug.contains("data:"));
    assert!(blob_debug.contains("bytes"));
}

#[test]
fn warns_when_key_dumps_are_absent() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "no-key-dumps");
    std::fs::remove_file(backup.join("key_dump.json")).unwrap();
    std::fs::remove_file(backup.join("key_table_dump.json")).unwrap();
    let plan = prepare_knife_backup(&backup).unwrap();
    assert!(plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("key_dump.json is absent")));
    assert!(plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("key_table_dump.json is absent")));
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

    let Some(url) = postgres_url() else {
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
    let existing_pivotal = runtime
        .block_on(seeding.create_user(
            &format!("pivotal-{suffix}"),
            &format!("existing-pivotal-{suffix}@example.com"),
            "bcrypt",
            "existing-hash",
            "existing-salt",
            false,
        ))
        .expect("create existing pivotal");
    drop(seeding);

    let plan = prepare_knife_backup(&backup).unwrap();
    let summary = apply_knife_backup_plan_postgres_with_options(
        &url,
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: true,
            replace_user_credentials: true,
        },
    )
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
    let pivotal = runtime
        .block_on(store.find_user(&format!("pivotal-{suffix}")))
        .unwrap()
        .expect("merged pivotal");
    assert_eq!(pivotal.id, existing_pivotal.id);
    assert_eq!(pivotal.authz_id, existing_pivotal.authz_id);
    assert_eq!(pivotal.password_hash, "pivotal-hash");
    let pivotal_key = runtime
        .block_on(store.find_actor_key(&existing_pivotal.id, ActorType::User, "default"))
        .unwrap()
        .expect("merged pivotal key");
    assert_eq!(pivotal_key.actor_id, existing_pivotal.id);
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

#[test]
fn merge_users_reconciles_server_admins_for_preserved_admin_state() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "admin-preserve");
    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-admin-preserve",
            "target-admin-preserve@example.com",
            "bcrypt",
            "target-hash",
            "target-salt",
            false,
        )
        .unwrap();
    let server_admins = store.ensure_global_group("server-admins").unwrap();
    store
        .add_group_member(&server_admins.authz_id, &existing.authz_id)
        .unwrap();
    let other = store
        .create_user(
            "other-admin-preserve",
            "other-admin-preserve@example.com",
            "bcrypt",
            "other-hash",
            "other-salt",
            true,
        )
        .unwrap();
    store
        .add_group_member(&server_admins.authz_id, &other.authz_id)
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: false,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let imported = store.find_user("pivotal-admin-preserve").unwrap().unwrap();
    assert_eq!(imported.id, existing.id);
    assert!(!imported.admin, "preserved target admin flag must win");
    let members = store
        .list_group_members("", &server_admins.authz_id)
        .unwrap()
        .users;
    assert!(
        !members.contains(&imported.username),
        "non-admin merged user must not stay in server-admins"
    );
    assert!(
        members.contains(&other.username),
        "target-only admin membership must be preserved"
    );
}

#[test]
fn merge_users_with_replace_credentials_removes_stale_server_admin() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "admin-demote");
    let key_dump_path = backup.join("key_dump.json");
    let mut key_dump: Value =
        serde_json::from_str(&std::fs::read_to_string(&key_dump_path).unwrap()).unwrap();
    key_dump[0]["admin"] = json!(false);
    write_json(&key_dump_path, &key_dump);

    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-admin-demote",
            "target-admin-demote@example.com",
            "bcrypt",
            "target-hash",
            "target-salt",
            true,
        )
        .unwrap();
    let server_admins = store.ensure_global_group("server-admins").unwrap();
    store
        .add_group_member(&server_admins.authz_id, &existing.authz_id)
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: true,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let imported = store.find_user("pivotal-admin-demote").unwrap().unwrap();
    assert_eq!(imported.id, existing.id);
    assert!(!imported.admin);
    let members = store
        .list_group_members("", &server_admins.authz_id)
        .unwrap()
        .users;
    assert!(
        !members.contains(&imported.username),
        "demoted merged user must be removed from server-admins"
    );
}

#[test]
fn replace_user_credentials_removes_target_default_key_when_backup_has_none() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "drop-default");
    let user_path = backup.join("users/pivotal.json");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&user_path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("public_key");
    write_json(&user_path, &value);

    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-drop-default",
            "target-drop-default@example.com",
            "bcrypt",
            "target-hash",
            "target-salt",
            false,
        )
        .unwrap();
    store
        .add_actor_key(
            &existing.id,
            ActorType::User,
            "default",
            "TARGET-DEFAULT-KEY",
        )
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: true,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let imported = store.find_user("pivotal-drop-default").unwrap().unwrap();
    assert_eq!(imported.id, existing.id);
    assert_eq!(imported.password_hash, "pivotal-hash");
    assert!(
        store
            .find_actor_key(&existing.id, ActorType::User, "default")
            .unwrap()
            .is_none(),
        "revocation mode must drop a target default key the backup does not supply"
    );
}

#[test]
fn merged_user_key_preservation_does_not_affect_non_merged_users() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "mixed-key");
    let data = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let existing = store
        .create_user(
            "pivotal-mixed-key",
            "target-mixed-key@example.com",
            "bcrypt",
            "target-hash",
            "target-salt",
            true,
        )
        .unwrap();
    store
        .add_actor_key(
            &existing.id,
            ActorType::User,
            "default",
            "TARGET-PIVOTAL-KEY",
        )
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: false,
        },
    )
    .unwrap();

    // The second import sees the non-merged alice with the same id/authz. The
    // merged-user-only preservation must not keep her stale default key.
    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-mixed-key").unwrap().unwrap();
    store
        .update_actor_key(
            &alice.id,
            ActorType::User,
            "default",
            None,
            Some("STALE-ALICE-KEY"),
            None,
        )
        .unwrap();
    drop(store);

    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &data.path().join("bookshelf"),
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: false,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-mixed-key").unwrap().unwrap();
    let alice_key = store
        .find_actor_key(&alice.id, ActorType::User, "default")
        .unwrap()
        .expect("non-merged default key");
    assert_eq!(
        alice_key.public_key, PUBLIC_KEY,
        "a non-merged user's planned default key must be applied"
    );
    let pivotal_key = store
        .find_actor_key(&existing.id, ActorType::User, "default")
        .unwrap()
        .expect("merged default key");
    assert_eq!(
        pivotal_key.public_key, "TARGET-PIVOTAL-KEY",
        "a merged user's target default key must be preserved"
    );
}

#[test]
fn replace_acls_restores_target_state_after_postgres_failure() {
    use microwave_store::{AuthzRepository, IdentityRepository, ObjectRepository};

    let Some(url) = postgres_url() else {
        return;
    };
    let suffix = format!(
        "pg-acl-fail-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), &suffix);
    let bookshelf = root.path().join("bookshelf");
    microwave_migrate::import_knife_backup_postgres_with_compression(
        &url,
        &backup,
        &bookshelf,
        Compression::None,
    )
    .unwrap();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("connect to configured PostgreSQL");
    let org_name = format!("acme-{suffix}");
    let bob_name = format!("bob-{suffix}");
    let org = runtime
        .block_on(store.find_org(&org_name))
        .unwrap()
        .unwrap();
    let bob = runtime
        .block_on(store.find_user(&bob_name))
        .unwrap()
        .unwrap();
    let node = runtime
        .block_on(store.find_node(&org.id, "web1"))
        .unwrap()
        .unwrap();
    let ops = runtime
        .block_on(store.find_group(&org.id, "ops"))
        .unwrap()
        .unwrap();
    runtime
        .block_on(store.set_acl(
            &node.authz_id,
            Permission::Create,
            Some(&bob.authz_id),
            None,
        ))
        .unwrap();
    runtime
        .block_on(store.add_group_member(&ops.authz_id, &bob.authz_id))
        .unwrap();
    drop(store);

    let mut plan = prepare_knife_backup(&backup).unwrap();
    plan.snapshot.nodes[0].id = "conflicting-node-id".into();
    let error = apply_knife_backup_plan_postgres_with_options(
        &url,
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: false,
            replace_acls: true,
            replace_user_credentials: false,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(!error.is_empty());

    let store = runtime
        .block_on(microwave_store_postgres::PostgresStore::connect(&url))
        .expect("reconnect to configured PostgreSQL");
    let node = runtime
        .block_on(store.find_node(&org.id, "web1"))
        .unwrap()
        .unwrap();
    let entries = runtime
        .block_on(store.get_acl_entries(&node.authz_id))
        .unwrap();
    let create = entries
        .iter()
        .find(|entry| entry.permission == Permission::Create)
        .unwrap();
    assert!(create.users.contains(&bob_name));
    let ops = runtime
        .block_on(store.find_group(&org.id, "ops"))
        .unwrap()
        .unwrap();
    let members = runtime
        .block_on(store.list_group_members(&org.id, &ops.authz_id))
        .unwrap();
    assert!(members.users.contains(&bob_name));
}

#[test]
fn default_reimport_does_not_reconcile_server_admins() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "default-admin");
    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-default-admin").unwrap().unwrap();
    let group = store
        .find_global_group("server-admins")
        .unwrap()
        .expect("server-admins");
    store
        .add_group_member(&group.authz_id, &alice.authz_id)
        .unwrap();
    drop(store);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite(data.path(), &plan, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let members = store.list_group_members("", &group.authz_id).unwrap().users;
    assert!(
        members.contains(&alice.username),
        "an additive re-import must not remove a target-only server-admins grant"
    );
}

#[test]
fn replace_credentials_does_not_remove_default_keys_of_exact_users() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "exact-key");
    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-exact-key").unwrap().unwrap();
    store
        .update_actor_key(
            &alice.id,
            ActorType::User,
            "default",
            None,
            Some("TARGET-ALICE-KEY"),
            None,
        )
        .unwrap();
    drop(store);

    // Removing the backup's inline public key makes alice a same-id user with
    // no source default while pivotal is still merged with replacement.
    let user_path = backup.join("users/alice.json");
    let mut value: Value =
        serde_json::from_str(&std::fs::read_to_string(&user_path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("public_key");
    write_json(&user_path, &value);

    let plan = prepare_knife_backup(&backup).unwrap();
    apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: true,
        },
    )
    .unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let alice = store.find_user("alice-exact-key").unwrap().unwrap();
    let key = store
        .find_actor_key(&alice.id, ActorType::User, "default")
        .unwrap()
        .expect("exact user default key");
    assert_eq!(
        key.public_key, "TARGET-ALICE-KEY",
        "replacement applies only to actually merged users"
    );
}

#[test]
fn imports_a_nested_cookbook_readme() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "nested-readme");
    let cookbook = backup.join("organizations/acme-nested-readme/cookbooks/app-1.2.3");
    write_text(
        &cookbook.join("docs/README.md"),
        "Nested long description\n",
    );
    write_text(
        &cookbook.join("metadata.rb"),
        "name 'app'\nversion '1.2.3'\nlong_description IO.read(File.join(File.dirname(__FILE__), 'docs/README.md'))\n",
    );
    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let org = store.find_org("acme-nested-readme").unwrap().unwrap();
    let manifest = read_cookbook_manifest(&store, &org.id);
    assert_eq!(
        manifest["metadata"]["long_description"],
        "Nested long description\n"
    );
}

#[test]
fn warns_when_key_dumps_are_empty() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "empty-keys");
    write_json(&backup.join("key_dump.json"), &json!([]));
    write_json(&backup.join("key_table_dump.json"), &json!([]));
    let plan = prepare_knife_backup(&backup).unwrap();
    assert!(plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("key_dump.json is empty")));
    assert!(plan
        .summary
        .warnings
        .iter()
        .any(|warning| warning.contains("key_table_dump.json is empty")));
}

#[test]
fn admin_reconciliation_failure_restores_admin_flag_and_membership() {
    let root = tempfile::tempdir().unwrap();
    let backup = fixture(root.path(), "admin-rollback");
    let key_dump_path = backup.join("key_dump.json");
    let mut key_dump: Value =
        serde_json::from_str(&std::fs::read_to_string(&key_dump_path).unwrap()).unwrap();
    key_dump[0]["admin"] = json!(false);
    write_json(&key_dump_path, &key_dump);

    let data = tempfile::tempdir().unwrap();
    let bookshelf = data.path().join("bookshelf");
    import_knife_backup_to(data.path(), &backup, &bookshelf, Compression::None).unwrap();

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let pivotal = store.find_user("pivotal-admin-rollback").unwrap().unwrap();
    store
        .update_user_patch(
            &pivotal.username,
            &microwave_store::UserPatch {
                admin: Some(true),
                ..Default::default()
            },
        )
        .unwrap();
    let group = store
        .find_global_group("server-admins")
        .unwrap()
        .expect("server-admins");
    store
        .add_group_member(&group.authz_id, &pivotal.authz_id)
        .unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(database(data.path())).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_server_admin_delete
             BEFORE DELETE ON authz_group_actors
             BEGIN
                 SELECT RAISE(ABORT, 'injected server-admins failure');
             END;",
        )
        .unwrap();
    drop(connection);

    let plan = prepare_knife_backup(&backup).unwrap();
    let error = apply_knife_backup_plan_sqlite_with_options(
        data.path(),
        &plan,
        &bookshelf,
        Compression::None,
        &KnifeBackupApplyOptions {
            merge_users: true,
            replace_acls: false,
            replace_user_credentials: true,
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("injected"), "{error}");

    let store = SqliteStore::open(&database(data.path())).unwrap();
    let pivotal = store.find_user("pivotal-admin-rollback").unwrap().unwrap();
    assert!(
        pivotal.admin,
        "failed reconciliation must restore the admin flag"
    );
    let members = store.list_group_members("", &group.authz_id).unwrap().users;
    assert!(
        members.contains(&pivotal.username),
        "failed reconciliation must restore server-admins membership"
    );
}
