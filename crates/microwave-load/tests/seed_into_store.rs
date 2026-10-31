//! The fixture must survive a real `import_snapshot`: the schema's foreign
//! keys, type triggers and ACL rules all apply.

use microwave_load::{seed, Fixture, KeyKind, SeedArgs, Sizes, ORG_ID, ORG_NAME};
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::{Compression, Permission, SqliteStore};
use std::path::Path;

fn seeded() -> Fixture {
    let mut fixture = Fixture::build(Sizes {
        nodes: 10,
        roles: 4,
        versions: 15,
        targets: 4,
        canary: 1,
    })
    .expect("fixture builds");
    microwave_load::keys::generate(&mut fixture, true).expect("keys generate");
    fixture
}

#[test]
fn import_accepts_the_fixture_and_grants_the_expected_acl() {
    let fixture = seeded();
    let directory = tempfile::tempdir().expect("tempdir");
    let store = SqliteStore::open_with_compression(
        &directory.path().join("microwave.db"),
        Compression::None,
    )
    .expect("store opens");
    store
        .import_snapshot(&fixture.snapshot)
        .expect("snapshot imports");

    let exported = store.export_snapshot().expect("snapshot exports");
    assert_eq!(exported.nodes.len(), 11);
    assert_eq!(exported.clients.len(), 5);
    assert_eq!(exported.roles.len(), 4);
    assert_eq!(exported.cookbook_versions.len(), 15);
    assert_eq!(exported.actor_keys.len(), fixture.entries.len());

    let owner = &exported.clients[0];
    let other = &exported.clients[1];
    let node = &exported.nodes[0];
    assert!(store
        .check_permission(&owner.authz_id, &node.authz_id, Permission::Read)
        .expect("permission checks"));
    assert!(store
        .check_permission(&owner.authz_id, &node.authz_id, Permission::Update)
        .expect("permission checks"));
    assert!(!store
        .check_permission(&other.authz_id, &node.authz_id, Permission::Update)
        .expect("permission checks"));

    // The group carries the cookbook reads the depsolver checks.
    let cookbook = &exported.cookbooks[0];
    assert!(store
        .check_permission(&owner.authz_id, &cookbook.authz_id, Permission::Read)
        .expect("permission checks"));

    // Dependency derivation ran through the shared helper.
    assert!(exported.cookbook_versions.iter().any(|version| version
        .meta_deps
        .as_deref()
        .is_some_and(|deps| deps != "{}")));
}

#[test]
fn every_book_blob_round_trips_through_the_object_store() {
    let fixture = seeded();
    let directory = tempfile::tempdir().expect("tempdir");
    let store = LocalObjectStore::new(directory.path().join("bookshelf"), "load", "");
    for book in &fixture.books {
        store
            .put(microwave_load::ORG_ID, &book.checksum, &book.data)
            .expect("blob writes");
        assert!(store.exists(microwave_load::ORG_ID, &book.checksum));
    }
    assert_eq!(fixture.books.len(), 15 * 3);
}

#[test]
fn superuser_is_a_manifest_entry_and_an_admin_user() {
    let fixture = seeded();
    let entry = fixture
        .entries
        .first()
        .expect("the superuser leads the manifest");
    assert_eq!(entry.kind, KeyKind::Superuser);
    assert_eq!(entry.name, microwave_load::SUPERUSER);
    assert!(fixture.snapshot.users[0].admin);
}

fn seed_args(directory: &Path) -> SeedArgs {
    SeedArgs {
        server_url: "http://127.0.0.1:9".into(),
        keys: directory.join("keys.json"),
        data_dir: directory.to_owned(),
        nodes: 4,
        roles: 4,
        versions: 6,
        targets: 2,
        canary: 0,
        shared_key: true,
        postgres_url: None,
    }
}

#[test]
fn seed_fills_the_environment_constraints_column() {
    let directory = tempfile::tempdir().expect("tempdir");
    seed(seed_args(directory.path())).expect("seed runs");

    let store = SqliteStore::open_with_compression(
        &directory.path().join("microwave.db"),
        Compression::None,
    )
    .expect("store opens");
    let org = store
        .find_org(ORG_NAME)
        .expect("org lookup")
        .expect("org exists");
    assert_eq!(org.id, ORG_ID);

    for (name, expected) in [
        ("load-constrained-a", r#"{"load-cb-0001":"1.0.0"}"#),
        ("load-constrained-b", r#"{"load-cb-0002":"~> 1.0"}"#),
    ] {
        let environment = store
            .find_environment(&org.id, name)
            .expect("environment lookup")
            .expect("environment exists");
        assert_eq!(
            environment.cookbook_constraints.as_deref(),
            Some(expected),
            "{name} carries the derived constraints"
        );
    }

    let default = store
        .find_environment(&org.id, "load-default")
        .expect("environment lookup")
        .expect("default environment exists");
    assert_eq!(default.cookbook_constraints, None);
}

#[test]
fn seed_refuses_a_populated_store() {
    let directory = tempfile::tempdir().expect("tempdir");
    seed(seed_args(directory.path())).expect("first seed runs");

    let error = seed(seed_args(directory.path())).expect_err("second seed fails");
    let message = error.to_string();
    assert!(message.contains("--data-dir"), "{message}");
    assert!(message.contains("fresh"), "{message}");
}
