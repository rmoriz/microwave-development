//! Backend-neutral store matrix.
//!
//! Every suite in this file is generic over [`MetadataStore`] and runs twice:
//! once against an in-memory SQLite store, and once against PostgreSQL when
//! `MICROWAVE_TEST_POSTGRES_URL` is set. The PostgreSQL half deliberately fails
//! loudly when the variable is present but the database cannot be reached or
//! migrated; only an unset variable skips.
//!
//! Names and checksums are unique per run so the PostgreSQL suites can run in
//! parallel against a shared database and against the existing
//! `microwave-store-postgres` integration tests.

use microwave_store::{
    ActorType, GroupMembers, MetadataStore, NewNode, Org, Permission, SqliteStore, StoreError,
    UserPatch, UserProfile,
};
use std::sync::{Arc, Once, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Backend selection
// ---------------------------------------------------------------------------

fn test_postgres_url() -> Option<String> {
    let Some(value) = std::env::var_os("MICROWAVE_TEST_POSTGRES_URL") else {
        static WARN: Once = Once::new();
        WARN.call_once(|| {
            eprintln!(
                "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; run \
                 scripts/run-postgres-tests.sh to include the PostgreSQL half \
                 of the dual-backend matrix"
            );
        });
        return None;
    };
    let url = value
        .into_string()
        .expect("MICROWAVE_TEST_POSTGRES_URL must be valid UTF-8");
    assert!(
        !url.trim().is_empty(),
        "MICROWAVE_TEST_POSTGRES_URL is set but empty; unset it to skip PostgreSQL \
         or provide a connection URL"
    );
    Some(url)
}

/// A backend store plus the PostgreSQL handle needed for test cleanup.
struct TestStore {
    metadata: Arc<dyn MetadataStore>,
    postgres: Option<microwave_store_postgres::PostgresStore>,
}

impl TestStore {
    async fn cleanup_global_groups(&self, names: &[String]) {
        let Some(postgres) = &self.postgres else {
            return;
        };
        for name in names {
            let authz_id: Option<String> =
                sqlx::query_scalar("DELETE FROM global_groups WHERE name = $1 RETURNING authz_id")
                    .bind(name)
                    .fetch_optional(postgres.pool())
                    .await
                    .unwrap_or_else(|error| panic!("delete test global group {name}: {error}"));
            if let Some(authz_id) = authz_id {
                sqlx::query("DELETE FROM authz_entities WHERE authz_id = $1")
                    .bind(authz_id)
                    .execute(postgres.pool())
                    .await
                    .unwrap_or_else(|error| {
                        panic!("delete test global group authz entity {name}: {error}")
                    });
            }
        }
    }
}

async fn sqlite_store() -> Option<TestStore> {
    Some(TestStore {
        metadata: Arc::new(SqliteStore::open_in_memory().expect("open in-memory SQLite store")),
        postgres: None,
    })
}

/// Apply the embedded schema exactly once per test binary.
///
/// Each `#[tokio::test]` owns its runtime, so a shared `sqlx` pool cannot be
/// reused across tests. A process-wide `OnceLock` still lets every test call
/// the normal connect-and-migrate path exactly once without running migration
/// DDL concurrently with another suite's DML, which can deadlock on
/// PostgreSQL. The result is cached instead of panicking inside `Once`, so
/// every test reports the original connection or migration error.
fn ensure_postgres_schema(url: &str) {
    static MIGRATED: OnceLock<Result<(), String>> = OnceLock::new();
    let result = MIGRATED.get_or_init(|| {
        let url = url.to_string();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("build migration runtime: {error}"))?;
            runtime.block_on(async move {
                microwave_store_postgres::PostgresStore::connect(&url)
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            })
        })
        .join()
        .unwrap_or_else(|_| Err("PostgreSQL migration thread panicked".to_string()))
    });
    if let Err(error) = result {
        panic!(
            "MICROWAVE_TEST_POSTGRES_URL is set but connecting or migrating the \
             test database failed: {error}"
        );
    }
}

async fn postgres_store() -> Option<TestStore> {
    let url = test_postgres_url()?;
    ensure_postgres_schema(&url);
    let store = microwave_store_postgres::PostgresStore::connect_existing(&url)
        .await
        .unwrap_or_else(|error| {
            panic!(
                "MICROWAVE_TEST_POSTGRES_URL is set but connecting to the migrated \
                 test database failed: {error}"
            )
        });
    Some(TestStore {
        metadata: Arc::new(store.clone()),
        postgres: Some(store),
    })
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct Fixture {
    org: Org,
    creator_authz_id: String,
}

const ORG_CONTAINERS: [&str; 12] = [
    "clients",
    "containers",
    "cookbooks",
    "cookbook_artifacts",
    "data",
    "environments",
    "groups",
    "nodes",
    "policies",
    "policy_groups",
    "roles",
    "sandboxes",
];

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

fn checksum() -> String {
    Uuid::new_v4().simple().to_string()
}

/// Run `count` futures as independent Tokio tasks released together by a
/// barrier. The matrix tests use a multi-thread runtime so this exercises real
/// database concurrency instead of polling two futures sequentially.
async fn run_concurrently<T, F, Fut>(count: usize, f: F) -> Vec<T>
where
    T: Send + 'static,
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = T> + Send + 'static,
{
    let barrier = Arc::new(tokio::sync::Barrier::new(count));
    let mut handles = Vec::with_capacity(count);
    for index in 0..count {
        let barrier = Arc::clone(&barrier);
        let future = f(index);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            future.await
        }));
    }
    let mut results = Vec::with_capacity(count);
    for handle in handles {
        results.push(handle.await.expect("concurrent matrix task panicked"));
    }
    results
}

fn is_32_hex(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Standard organization bootstrap: org, creator actor, containers and the
/// `users`/`admins` groups used by membership flows.
async fn fixture<S: MetadataStore + ?Sized>(store: &S) -> Fixture {
    let org = store
        .create_org(&unique("matrix-org"), "Matrix Organization")
        .await
        .expect("create matrix org");
    let creator_authz_id = store
        .create_actor()
        .await
        .expect("create matrix creator actor");
    for container in ORG_CONTAINERS {
        store
            .create_container_for_org_with_acl(&org.id, container, &creator_authz_id)
            .await
            .expect("create matrix org container");
    }
    for group in ["users", "admins"] {
        store
            .create_group_for_org_with_acl(&org.id, group, &creator_authz_id)
            .await
            .expect("create matrix org group");
    }
    Fixture {
        org,
        creator_authz_id,
    }
}

async fn cleanup_org<S: MetadataStore + ?Sized>(store: &S, org_id: &str) {
    store.delete_org(org_id).await.expect("delete matrix org");
}

async fn create_user<S: MetadataStore + ?Sized>(store: &S, prefix: &str) -> microwave_store::User {
    let username = unique(prefix);
    store
        .create_user(
            &username,
            &format!("{username}@example.test"),
            "bcrypt",
            "$2b$12$matrix",
            "salt",
            false,
        )
        .await
        .expect("create matrix user")
}

// ---------------------------------------------------------------------------
// Suites
// ---------------------------------------------------------------------------

mod suite {
    use super::*;

    pub async fn org_deletion(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let name = unique("delete-org");
        let org = store
            .create_org(&name, "Delete Organization")
            .await
            .expect("create org for deletion");
        let read_access_name = format!("{name}_read_access_group");
        let read_access = store
            .ensure_global_group(&read_access_name)
            .await
            .expect("create org read access group");
        store
            .ensure_global_group("server-admins")
            .await
            .expect("ensure server admins");
        let child = store
            .create_group_for_org(&org.id, "admins")
            .await
            .expect("create org group");
        store
            .create_group_for_org(&org.id, "users")
            .await
            .expect("create users group");
        let member = create_user(store.as_ref(), "deleted-org-member").await;
        store
            .add_user_to_org(&org.id, &member.id)
            .await
            .expect("add member to deleted org");
        assert!(store
            .find_group(&org.id, &member.id)
            .await
            .expect("find member USAG")
            .is_some());
        store
            .add_group_child(&read_access.authz_id, &child.authz_id)
            .await
            .expect("join read access group to org group");

        assert!(store
            .find_global_group(&read_access_name)
            .await
            .expect("find read access group")
            .is_some());
        assert!(!store
            .get_acl_entries(&read_access.authz_id)
            .await
            .expect("read access group ACL rows")
            .is_empty());

        store.delete_org(&org.id).await.expect("delete org");

        assert!(store
            .find_org(&name)
            .await
            .expect("find deleted org")
            .is_none());
        assert!(store
            .find_group(&org.id, &member.id)
            .await
            .expect("find deleted member USAG")
            .is_none());
        assert!(store
            .find_global_group(&read_access_name)
            .await
            .expect("find deleted read access group")
            .is_none());
        match store.get_acl_entries(&read_access.authz_id).await {
            Ok(entries) => assert!(
                entries.is_empty(),
                "read access ACL rows survived org deletion: {entries:?}"
            ),
            Err(StoreError::NotFound(_)) => {}
            Err(error) => panic!("read access ACL rows after delete: {error}"),
        }
        assert!(
            store
                .find_global_group("server-admins")
                .await
                .expect("find server admins after org deletion")
                .is_some(),
            "server-admins must survive org deletion"
        );
        Vec::new()
    }

    pub async fn identity(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;

        assert_eq!(store.find_org(&org.name).await.unwrap().as_ref(), Some(org));
        assert_eq!(
            store.find_org_by_id(&org.id).await.unwrap().as_ref(),
            Some(org)
        );
        assert!(store.list_orgs().await.unwrap().contains(org));

        let updated_org = store
            .update_org(&org.name, "Matrix Organization Updated")
            .await
            .expect("update org");
        assert_eq!(updated_org.id, org.id);
        assert_eq!(updated_org.full_name, "Matrix Organization Updated");
        assert!(store.list_orgs().await.unwrap().contains(&updated_org));

        let profile = UserProfile {
            display_name: Some("Matrix User".into()),
            first_name: Some("Matrix".into()),
            middle_name: Some(String::new()),
            last_name: Some("User".into()),
            external_authentication_uid: Some(format!("uid-{}", unique("ext"))),
        };
        let username = unique("user");
        let email = format!("{username}@example.test");
        let user = store
            .create_user_with_profile(
                &username,
                Some(&email),
                "bcrypt",
                "$2b$12$matrix",
                "salt",
                false,
                &profile,
            )
            .await
            .expect("create user with profile");
        assert!(is_32_hex(&user.id));
        assert!(is_32_hex(&user.authz_id));
        assert_eq!(user.email.as_deref(), Some(email.as_str()));
        assert_eq!(user.display_name.as_deref(), Some("Matrix User"));
        assert_eq!(user.first_name.as_deref(), Some("Matrix"));
        assert_eq!(user.middle_name.as_deref(), Some(""));
        assert_eq!(user.last_name.as_deref(), Some("User"));
        assert!(!user.admin);

        assert_eq!(
            store.find_user(&username).await.unwrap().as_ref(),
            Some(&user)
        );
        assert_eq!(
            store.find_user_by_id(&user.id).await.unwrap().as_ref(),
            Some(&user)
        );
        assert!(store.list_users().await.unwrap().contains(&user));
        assert!(store
            .list_users_filtered(Some(&email.to_uppercase()), None)
            .await
            .unwrap()
            .contains(&user));
        assert!(store
            .list_users_filtered(None, profile.external_authentication_uid.as_deref(),)
            .await
            .unwrap()
            .contains(&user));

        // Emails are unique case-insensitively.
        let duplicate = store
            .create_user(
                &unique("duplicate"),
                &email.to_uppercase(),
                "bcrypt",
                "$2b$12$other",
                "salt",
                false,
            )
            .await;
        assert!(matches!(duplicate, Err(StoreError::Conflict(_))));

        let peer = create_user(store.as_ref(), "peer").await;
        assert!(!store
            .users_share_org(&user.id, &peer.id)
            .await
            .expect("users do not share an org yet"));
        store
            .add_user_to_org(&org.id, &user.id)
            .await
            .expect("add user to matrix org");
        store
            .add_user_to_org(&org.id, &peer.id)
            .await
            .expect("add peer to matrix org");
        assert!(store
            .users_share_org(&user.id, &peer.id)
            .await
            .expect("users share an org"));

        // Full update path used by the API's PUT handler.
        let peer_email = format!("{}@example.test", unique("peer-updated"));
        let peer = store
            .update_user(
                &peer.username,
                Some(&peer_email),
                Some(("$2b$12$updated", "updated-salt", "bcrypt")),
                Some(true),
            )
            .await
            .expect("update peer");
        assert_eq!(peer.email.as_deref(), Some(peer_email.as_str()));
        assert_eq!(peer.password_hash, "$2b$12$updated");
        assert_eq!(peer.password_salt, "updated-salt");
        assert!(peer.admin);

        // Patch path: rename, profile fields, external UID and admin flag.
        let renamed_name = unique("user-renamed");
        let patched = store
            .update_user_patch(
                &username,
                &UserPatch {
                    new_username: Some(renamed_name.clone()),
                    email: Some(format!("{renamed_name}@example.test")),
                    admin: Some(true),
                    display_name: Some(Some("Renamed User".into())),
                    first_name: Some(None),
                    external_authentication_uid: Some(Some("matrix-uid".into())),
                    ..Default::default()
                },
            )
            .await
            .expect("patch user");
        assert_eq!(patched.username, renamed_name);
        assert_eq!(patched.display_name.as_deref(), Some("Renamed User"));
        assert_eq!(patched.first_name, None);
        assert_eq!(patched.last_name.as_deref(), Some("User"));
        assert_eq!(
            patched.external_authentication_uid.as_deref(),
            Some("matrix-uid")
        );
        assert!(patched.admin);
        assert!(store.find_user(&username).await.unwrap().is_none());
        assert_eq!(
            store.find_user(&renamed_name).await.unwrap().as_ref(),
            Some(&patched)
        );

        store
            .set_recovery_authentication_enabled(&renamed_name, true)
            .await
            .expect("enable recovery authentication");
        let recovered = store.find_user(&renamed_name).await.unwrap().unwrap();
        assert!(recovered.recovery_authentication_enabled);

        // Every user ACL includes server-admins for create/read/update/delete
        // (but not grant), matching the SQLite account-service default.
        let user_acl = store
            .get_acl_entries(&patched.authz_id)
            .await
            .expect("user ACL");
        for permission in [
            Permission::Create,
            Permission::Read,
            Permission::Update,
            Permission::Delete,
        ] {
            let entry = user_acl
                .iter()
                .find(|entry| entry.permission == permission)
                .expect("user permission entry");
            assert!(
                entry.groups.contains(&"::server-admins".to_string()),
                "server-admins missing from {permission:?}: {entry:?}"
            );
            assert!(entry.users.contains(&patched.username));
        }
        let grant = user_acl
            .iter()
            .find(|entry| entry.permission == Permission::Grant)
            .expect("user grant entry");
        assert!(!grant.groups.contains(&"::server-admins".to_string()));
        assert!(grant.users.contains(&patched.username));

        // Keys are shared by all actor types.
        let user_key = store
            .create_actor_key(
                &patched.id,
                ActorType::User,
                "deploy",
                "-----BEGIN PUBLIC KEY-----\ndeploy\n-----END PUBLIC KEY-----",
                "2000-01-01T00:00:00Z",
            )
            .await
            .expect("create deploy key");
        assert!(user_key.expired);
        assert_eq!(
            store
                .find_actor_key(&patched.id, ActorType::User, "deploy")
                .await
                .unwrap()
                .as_ref(),
            Some(&user_key)
        );
        let renamed_key = store
            .update_actor_key(
                &patched.id,
                ActorType::User,
                "deploy",
                Some("deploy-2"),
                Some("-----BEGIN PUBLIC KEY-----\nrotated\n-----END PUBLIC KEY-----"),
                Some("infinity"),
            )
            .await
            .expect("rename and rotate key");
        assert_eq!(renamed_key.key_name, "deploy-2");
        assert!(!renamed_key.expired);
        assert!(renamed_key.public_key.contains("rotated"));
        assert_eq!(
            store
                .list_actor_keys(&patched.id, ActorType::User)
                .await
                .unwrap(),
            vec![renamed_key.clone()]
        );
        assert_eq!(
            store
                .find_actor_keys(&patched.id, ActorType::User)
                .await
                .unwrap(),
            vec![renamed_key.clone()]
        );

        let default_key = store
            .update_or_create_default_key(
                &patched.id,
                ActorType::User,
                "-----BEGIN PUBLIC KEY-----\ndefault\n-----END PUBLIC KEY-----",
            )
            .await
            .expect("create default user key");
        assert_eq!(default_key.key_name, "default");
        let rotated_default = store
            .update_or_create_default_key(
                &patched.id,
                ActorType::User,
                "-----BEGIN PUBLIC KEY-----\ndefault-rotated\n-----END PUBLIC KEY-----",
            )
            .await
            .expect("rotate default user key");
        assert!(rotated_default.public_key.contains("default-rotated"));

        // Clients have their own key namespace and ACL-bearing authz id.
        let client_name = unique("client");
        let client = store
            .create_client(&org.id, &client_name, true, false)
            .await
            .expect("create client");
        assert!(is_32_hex(&client.authz_id));
        assert!(client.validator);
        assert!(!client.admin);
        assert_eq!(
            store
                .find_client(&org.id, &client_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&client)
        );
        assert!(store.list_clients(&org.id).await.unwrap().contains(&client));

        let acl_client_name = unique("acl-client");
        let acl_client = store
            .create_client_with_acl(
                &org.id,
                &acl_client_name,
                false,
                false,
                Some(&patched.authz_id),
            )
            .await
            .expect("create client with ACL");
        assert!(!acl_client.validator);
        let acl = store
            .get_acl_entries(&acl_client.authz_id)
            .await
            .expect("read client ACL");
        let read = acl
            .iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("client read entry");
        assert_eq!(read.users, vec![patched.username.clone()]);
        // Non-validator clients also grant themselves every permission.
        assert_eq!(read.clients, vec![acl_client_name.clone()]);

        let renamed_client_name = unique("client-renamed");
        let renamed_client = store
            .update_client(&org.id, &client_name, &renamed_client_name, Some(false))
            .await
            .expect("rename client");
        assert_eq!(renamed_client.name, renamed_client_name);
        assert!(!renamed_client.validator);
        assert!(store
            .find_client(&org.id, &client_name)
            .await
            .unwrap()
            .is_none());

        let client_key = store
            .update_or_create_default_key(
                &renamed_client.id,
                ActorType::Client,
                "-----BEGIN PUBLIC KEY-----\nclient\n-----END PUBLIC KEY-----",
            )
            .await
            .expect("create client key");
        assert_eq!(client_key.actor_id, renamed_client.id);
        assert_eq!(client_key.key_name, "default");
        assert_eq!(
            store
                .list_actor_keys(&renamed_client.id, ActorType::Client)
                .await
                .unwrap()
                .len(),
            1
        );

        // Principals resolve users and org-scoped clients by name.
        let principals = store
            .find_principals(Some(&org.id), &renamed_name)
            .await
            .expect("find user principal");
        assert_eq!(principals.len(), 1);
        assert_eq!(principals[0].actor_type, ActorType::User);
        assert_eq!(principals[0].actor_id, patched.id);
        assert_eq!(principals[0].org_id.as_deref(), Some(org.id.as_str()));
        let principal_keys = store
            .find_principal_keys(&principals[0])
            .await
            .expect("find principal keys");
        assert!(principal_keys.contains(&renamed_key));
        assert!(principal_keys.iter().any(|key| key.key_name == "default"));

        let client_principals = store
            .find_principals(Some(&org.id), &renamed_client_name)
            .await
            .expect("find client principal");
        assert_eq!(client_principals.len(), 1);
        assert_eq!(client_principals[0].actor_type, ActorType::Client);
        assert_eq!(client_principals[0].authz_id, renamed_client.authz_id);

        store
            .delete_actor_key(&patched.id, ActorType::User, "deploy-2")
            .await
            .expect("delete deploy key");
        assert!(store
            .find_actor_key(&patched.id, ActorType::User, "deploy-2")
            .await
            .unwrap()
            .is_none());

        store
            .delete_client(&org.id, &renamed_client_name)
            .await
            .expect("delete renamed client");
        store
            .delete_client(&org.id, &acl_client_name)
            .await
            .expect("delete ACL client");
        assert!(store
            .find_client(&org.id, &renamed_client_name)
            .await
            .unwrap()
            .is_none());
        store.delete_user(&renamed_name).await.expect("delete user");
        store
            .delete_user(&peer.username)
            .await
            .expect("delete peer");
        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }

    pub async fn authz(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;

        // Entity id primitives.
        let actor = store.create_actor().await.expect("create actor");
        let object = store.create_object().await.expect("create object");
        let raw_group = store.create_group("raw").await.expect("create raw group");
        let raw_container = store
            .create_container()
            .await
            .expect("create raw container");
        for id in [&actor, &object, &raw_group, &raw_container] {
            assert!(is_32_hex(id));
        }

        // Group CRUD, including create-with-ACL, rename and delete.
        let group_name = unique("group");
        let group = store
            .create_group_for_org_with_acl(&org.id, &group_name, &fixture.creator_authz_id)
            .await
            .expect("create group with ACL");
        assert_eq!(
            store
                .find_group(&org.id, &group_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&group)
        );
        assert!(store.list_groups(&org.id).await.unwrap().contains(&group));
        let renamed_group_name = unique("group-renamed");
        let renamed_group = store
            .rename_group(&org.id, &group_name, &renamed_group_name)
            .await
            .expect("rename group");
        assert_eq!(renamed_group.name, renamed_group_name);
        assert!(store
            .find_group(&org.id, &group_name)
            .await
            .unwrap()
            .is_none());

        // Container CRUD.
        let container_name = unique("container");
        let container = store
            .create_container_for_org_with_acl(&org.id, &container_name, &fixture.creator_authz_id)
            .await
            .expect("create container with ACL");
        assert_eq!(
            store
                .find_container(&org.id, &container_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&container)
        );
        assert!(store
            .list_containers(&org.id)
            .await
            .unwrap()
            .contains(&container));

        // ACL target with no inherited permission of its own.
        let target_name = unique("acl-target");
        let target = store
            .create_container_for_org(&org.id, &target_name)
            .await
            .expect("create ACL target");
        let user = create_user(store.as_ref(), "acl-user").await;
        let client_name = unique("acl-client");
        let client = store
            .create_client(&org.id, &client_name, false, false)
            .await
            .expect("create ACL client");
        assert!(is_32_hex(&client.authz_id));
        let child_name = unique("child-group");
        let child = store
            .create_group_for_org(&org.id, &child_name)
            .await
            .expect("create child group");
        store
            .add_group_member(&renamed_group.authz_id, &actor)
            .await
            .expect("add actor to group");
        store
            .add_group_child(&renamed_group.authz_id, &child.authz_id)
            .await
            .expect("nest child group");

        store
            .set_acl(
                &target.authz_id,
                Permission::Read,
                Some(&actor),
                Some(&child.authz_id),
            )
            .await
            .expect("grant read to actor and child group");
        assert!(store
            .check_permission(&actor, &target.authz_id, Permission::Read)
            .await
            .expect("recursive read check"));
        assert!(!store
            .check_permission(&actor, &target.authz_id, Permission::Update)
            .await
            .expect("denied update check"));
        assert!(matches!(
            store
                .ensure_acyclic(&child.authz_id, &renamed_group.authz_id)
                .await,
            Err(StoreError::Invalid(_))
        ));

        let entries = store
            .get_acl_entries(&target.authz_id)
            .await
            .expect("read ACL entries");
        assert_eq!(entries.len(), Permission::ALL.len());
        let read = entries
            .iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("read entry");
        assert_eq!(read.groups, vec![child_name.clone()]);
        let acl = store.get_acl(&target.authz_id).await.expect("read ACL");
        assert_eq!(acl.len(), Permission::ALL.len());

        // Replace a single permission from names.
        store
            .set_acl_for_permission(
                &target.authz_id,
                Permission::Update,
                std::slice::from_ref(&user.username),
                std::slice::from_ref(&renamed_group_name),
            )
            .await
            .expect("set update ACL from names");
        assert!(store
            .check_permission(&user.authz_id, &target.authz_id, Permission::Update)
            .await
            .expect("direct user update"));
        assert!(store
            .check_permission(&actor, &target.authz_id, Permission::Update)
            .await
            .expect("group update"));
        assert!(matches!(
            store
                .set_acl_for_permission(
                    &target.authz_id,
                    Permission::Update,
                    &[unique("missing-user")],
                    &[],
                )
                .await,
            Err(StoreError::Invalid(_))
        ));

        // Split user/client/group replacement.
        store
            .set_acl_for_permission_split(
                &target.authz_id,
                Permission::Delete,
                std::slice::from_ref(&user.username),
                std::slice::from_ref(&client_name),
                std::slice::from_ref(&child_name),
            )
            .await
            .expect("set split delete ACL");
        let entries = store
            .get_acl_entries(&target.authz_id)
            .await
            .expect("read split ACL");
        let delete = entries
            .iter()
            .find(|entry| entry.permission == Permission::Delete)
            .expect("delete entry");
        assert_eq!(delete.users, vec![user.username.clone()]);
        assert_eq!(delete.clients, vec![client_name.clone()]);
        assert_eq!(delete.groups, vec![child_name.clone()]);

        // Grant-all is visible for every permission.
        let grant_target = store
            .create_container_for_org(&org.id, &unique("grant-target"))
            .await
            .expect("create grant target");
        store
            .grant_actor_all(&grant_target.authz_id, &user.authz_id)
            .await
            .expect("grant all");
        for permission in Permission::ALL {
            assert!(store
                .check_permission(&user.authz_id, &grant_target.authz_id, permission)
                .await
                .unwrap_or_else(|error| panic!("check {permission:?}: {error}")));
        }

        // Replace group membership by name.
        store
            .replace_group_members(
                &org.id,
                &renamed_group.authz_id,
                Some(std::slice::from_ref(&user.username)),
                Some(std::slice::from_ref(&client_name)),
                Some(std::slice::from_ref(&child_name)),
            )
            .await
            .expect("replace group members");
        assert_eq!(
            store
                .list_group_members(&org.id, &renamed_group.authz_id)
                .await
                .expect("list replaced members"),
            GroupMembers {
                users: vec![user.username.clone()],
                clients: vec![client_name.clone()],
                groups: vec![child_name.clone()],
            }
        );

        // Global groups are addressable as `::name` and nested into org groups.
        // Concurrent bootstrap calls must converge on one row. Eight real
        // Tokio tasks are released by a barrier; reverting the atomic-insert
        // implementation in either backend makes at least one caller observe
        // a spurious conflict.
        let concurrent_group = unique("concurrent-global-group");
        let results = run_concurrently(8, {
            let store = Arc::clone(store);
            let name = concurrent_group.clone();
            move |_| {
                let store = Arc::clone(&store);
                let name = name.clone();
                async move { store.ensure_global_group(&name).await }
            }
        })
        .await;
        let groups: Vec<_> = results
            .into_iter()
            .map(|result| result.expect("concurrent ensure_global_group"))
            .collect();
        let first = groups.first().expect("at least one global group");
        assert!(
            groups.iter().all(|group| group == first),
            "all concurrent calls must return the same global group"
        );
        let global = store
            .ensure_global_group(&unique("global-group"))
            .await
            .expect("ensure global group");
        assert_eq!(
            store
                .find_global_group(&global.name)
                .await
                .unwrap()
                .as_ref(),
            Some(&global)
        );
        store
            .add_group_member(&global.authz_id, &actor)
            .await
            .expect("add actor to global group");
        let global_acl_name = format!("::{}", global.name);
        store
            .set_acl_for_permission(
                &target.authz_id,
                Permission::Create,
                &[],
                std::slice::from_ref(&global_acl_name),
            )
            .await
            .expect("grant create to global group");
        assert!(store
            .check_permission(&actor, &target.authz_id, Permission::Create)
            .await
            .expect("global group create check"));
        let entries = store
            .get_acl_entries(&target.authz_id)
            .await
            .expect("read global ACL");
        let create = entries
            .iter()
            .find(|entry| entry.permission == Permission::Create)
            .expect("create entry");
        assert_eq!(create.groups, vec![global_acl_name.clone()]);
        store
            .add_group_child(&renamed_group.authz_id, &global.authz_id)
            .await
            .expect("nest global group");
        assert!(store
            .list_group_members(&org.id, &renamed_group.authz_id)
            .await
            .expect("list nested global group")
            .groups
            .contains(&global_acl_name));

        // ACL target lookup covers org-scoped object types.
        assert_eq!(
            store
                .find_acl_target(&org.id, "containers", &target_name)
                .await
                .expect("find container ACL target"),
            Some(target.authz_id.clone())
        );
        assert!(store
            .find_acl_target(&org.id, "nodes", &unique("missing"))
            .await
            .expect("missing ACL target")
            .is_none());

        // Direct (non-invitation) association also lands in the users group
        // and receives the org read ACE when the read group exists.
        let direct = create_user(store.as_ref(), "direct").await;
        let read_group = format!("{}_read_access_group", org.name);
        store
            .ensure_global_group(&read_group)
            .await
            .expect("ensure read access group");
        store
            .associate_user_org(&org.id, &direct.id)
            .await
            .expect("associate user directly");
        assert!(store
            .is_user_in_org(&org.id, &direct.id)
            .await
            .expect("direct association"));
        let direct_acl = store
            .get_acl_entries(&direct.authz_id)
            .await
            .expect("direct user ACL");
        assert!(direct_acl
            .iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("direct read entry")
            .groups
            .contains(&format!("::{read_group}")));

        // Adding opposite edges concurrently must not produce a cycle. Both
        // callers lock/check/insert in one transaction; exactly one succeeds
        // and the other observes the new edge as a cycle.
        let concurrent_parent = store
            .create_group_for_org(&org.id, &unique("cycle-parent"))
            .await
            .expect("create cycle parent");
        let concurrent_child = store
            .create_group_for_org(&org.id, &unique("cycle-child"))
            .await
            .expect("create cycle child");
        let results = run_concurrently(2, {
            let store = Arc::clone(store);
            let parent = concurrent_parent.authz_id.clone();
            let child = concurrent_child.authz_id.clone();
            move |index| {
                let store = Arc::clone(&store);
                let (from, to) = if index % 2 == 0 {
                    (parent.clone(), child.clone())
                } else {
                    (child.clone(), parent.clone())
                };
                async move { store.add_group_child(&from, &to).await }
            }
        })
        .await;
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            1,
            "exactly one concurrent edge insertion must succeed: {results:?}"
        );
        assert!(
            results
                .iter()
                .any(|result| matches!(result, Err(StoreError::Invalid(_)))),
            "the losing insertion must report a cycle: {results:?}"
        );

        // Cleanup per-org objects, then the org itself.
        store
            .delete_container(&org.id, &container_name)
            .await
            .expect("delete container");
        store
            .delete_group(&org.id, &renamed_group_name)
            .await
            .expect("delete group");
        store
            .delete_group(&org.id, &child_name)
            .await
            .expect("delete child group");
        assert!(store
            .find_container(&org.id, &container_name)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .find_group(&org.id, &renamed_group_name)
            .await
            .unwrap()
            .is_none());
        store
            .delete_client(&org.id, &client_name)
            .await
            .expect("delete ACL client");
        store
            .delete_user(&user.username)
            .await
            .expect("delete ACL user");
        store
            .delete_user(&direct.username)
            .await
            .expect("delete direct user");
        cleanup_org(store.as_ref(), &org.id).await;
        vec![concurrent_group, global.name, read_group]
    }

    pub async fn objects(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = &fixture.creator_authz_id;

        let node_name = unique("node");
        let node_body = format!(r#"{{"name":"{node_name}","run_list":[]}}"#);
        let node = store
            .create_node(
                &org.id,
                NewNode {
                    name: &node_name,
                    body: &node_body,
                    environment: "_default",
                    policy_name: None,
                    policy_group: None,
                    creator_authz_id: creator,
                },
            )
            .await
            .expect("create node");
        assert!(is_32_hex(&node.id));
        assert!(is_32_hex(&node.authz_id));
        assert_eq!(
            store.find_node(&org.id, &node_name).await.unwrap().as_ref(),
            Some(&node)
        );
        assert!(store.list_nodes(&org.id).await.unwrap().contains(&node));
        assert_eq!(
            store
                .list_nodes(&org.id)
                .await
                .expect("list org nodes")
                .len(),
            1
        );
        assert!(matches!(
            store
                .create_node(
                    &org.id,
                    NewNode {
                        name: &node_name,
                        body: &node_body,
                        environment: "_default",
                        policy_name: None,
                        policy_group: None,
                        creator_authz_id: creator,
                    },
                )
                .await,
            Err(StoreError::Conflict(_))
        ));

        let updated_body = format!(r#"{{"name":"{node_name}","run_list":["role[web]"]}}"#);
        let updated_node = store
            .update_node(
                &org.id,
                &node_name,
                &updated_body,
                "production",
                Some("base"),
                Some("prod"),
            )
            .await
            .expect("update node");
        assert_eq!(updated_node.environment, "production");
        assert_eq!(updated_node.policy_name.as_deref(), Some("base"));
        assert_eq!(updated_node.policy_group.as_deref(), Some("prod"));
        assert_eq!(updated_node.serialized_object, updated_body);
        assert_eq!(
            store
                .list_nodes_in_environment(&org.id, "production")
                .await
                .expect("list nodes by environment"),
            vec![updated_node.clone()]
        );
        assert!(store
            .list_nodes_in_environment(&org.id, "_default")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .find_acl_target(&org.id, "nodes", &node_name)
                .await
                .expect("node ACL target"),
            Some(updated_node.authz_id.clone())
        );

        let role_name = unique("role");
        let role_body = format!(r#"{{"name":"{role_name}","run_list":[]}}"#);
        let role = store
            .create_role(&org.id, &role_name, &role_body, creator)
            .await
            .expect("create role");
        assert_eq!(
            store.find_role(&org.id, &role_name).await.unwrap().as_ref(),
            Some(&role)
        );
        assert!(store.list_roles(&org.id).await.unwrap().contains(&role));
        let role_updated = format!(r#"{{"name":"{role_name}","run_list":["recipe[apache]"]}}"#);
        let role = store
            .update_role(&org.id, &role_name, &role_updated)
            .await
            .expect("update role");
        assert_eq!(role.serialized_object, role_updated);
        assert_eq!(
            store
                .find_acl_target(&org.id, "roles", &role_name)
                .await
                .unwrap(),
            Some(role.authz_id.clone())
        );

        let environment_name = unique("env");
        let environment_body = format!(r#"{{"name":"{environment_name}"}}"#);
        let environment = store
            .create_environment(&org.id, &environment_name, &environment_body, creator)
            .await
            .expect("create environment");
        assert_eq!(
            store
                .find_environment(&org.id, &environment_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&environment)
        );
        assert!(store
            .list_environments(&org.id)
            .await
            .unwrap()
            .contains(&environment));
        let environment_updated =
            format!(r#"{{"name":"{environment_name}","description":"matrix"}}"#);
        let environment = store
            .update_environment(&org.id, &environment_name, &environment_updated)
            .await
            .expect("update environment");
        assert_eq!(environment.serialized_object, environment_updated);
        assert_eq!(
            store
                .find_acl_target(&org.id, "environments", &environment_name)
                .await
                .unwrap(),
            Some(environment.authz_id.clone())
        );

        let bag_name = unique("bag");
        let bag = store
            .create_data_bag(&org.id, &bag_name, creator)
            .await
            .expect("create data bag");
        assert_eq!(
            store
                .find_data_bag(&org.id, &bag_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&bag)
        );
        assert!(store.list_data_bags(&org.id).await.unwrap().contains(&bag));
        assert_eq!(
            store
                .find_acl_target(&org.id, "data", &bag_name)
                .await
                .unwrap(),
            Some(bag.authz_id.clone())
        );

        let item_name = unique("item");
        let item_body = format!(r#"{{"id":"{item_name}","password":"hunter2"}}"#);
        let item = store
            .create_data_bag_item(&org.id, &bag_name, &item_name, &item_body)
            .await
            .expect("create data bag item");
        assert_eq!(
            item.serialized_object, item_body,
            "data bag item should be stored as written"
        );
        assert_eq!(
            store
                .find_data_bag_item(&org.id, &bag_name, &item_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&item)
        );
        assert_eq!(
            store.list_data_bag_items(&org.id, &bag_name).await.unwrap(),
            vec![item.clone()]
        );
        let item_updated = format!(r#"{{"id":"{item_name}","password":"rotated"}}"#);
        let item = store
            .update_data_bag_item(&org.id, &bag_name, &item_name, &item_updated)
            .await
            .expect("update data bag item");
        assert_eq!(item.serialized_object, item_updated);

        store
            .delete_data_bag_item(&org.id, &bag_name, &item_name)
            .await
            .expect("delete data bag item");
        assert!(store
            .find_data_bag_item(&org.id, &bag_name, &item_name)
            .await
            .unwrap()
            .is_none());
        store
            .delete_data_bag(&org.id, &bag_name)
            .await
            .expect("delete data bag");
        store
            .delete_role(&org.id, &role_name)
            .await
            .expect("delete role");
        store
            .delete_environment(&org.id, &environment_name)
            .await
            .expect("delete environment");
        store
            .delete_node(&org.id, &node_name)
            .await
            .expect("delete node");
        assert!(store
            .find_node(&org.id, &node_name)
            .await
            .unwrap()
            .is_none());
        assert!(store.list_data_bags(&org.id).await.unwrap().is_empty());
        assert!(store.list_roles(&org.id).await.unwrap().is_empty());
        assert!(store.list_environments(&org.id).await.unwrap().is_empty());
        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }

    pub async fn cookbooks(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = &fixture.creator_authz_id;
        let cookbook_name = unique("cookbook");
        let first = checksum();
        let second = checksum();
        // Checksum vectors are returned in database order; sort the input so
        // the create result and the subsequent finds compare directly.
        let mut checksums = vec![first.clone(), second.clone()];
        checksums.sort();

        let (sandbox_id, checks) = store
            .create_sandbox(&org.id, &checksums)
            .await
            .expect("create sandbox");
        assert!(is_32_hex(&sandbox_id));
        assert_eq!(checks.len(), 2);
        assert!(checks.iter().all(|check| check.needs_upload));
        assert!(store
            .sandbox_exists(&org.id, &sandbox_id)
            .await
            .expect("sandbox exists"));
        assert!(store
            .sandbox_created_at(&org.id, &sandbox_id)
            .await
            .expect("sandbox created at")
            .is_some());
        let mut sorted = checksums.clone();
        sorted.sort();
        assert_eq!(
            store
                .sandbox_checksums(&org.id, &sandbox_id)
                .await
                .expect("sandbox checksums"),
            sorted
        );
        assert!(!store
            .checksum_exists(&org.id, &first)
            .await
            .expect("checksum not committed"));
        store
            .commit_sandbox(&org.id, &sandbox_id)
            .await
            .expect("commit sandbox");
        assert!(!store
            .sandbox_exists(&org.id, &sandbox_id)
            .await
            .expect("sandbox consumed"));
        for checksum in &checksums {
            assert!(store
                .checksum_exists(&org.id, checksum)
                .await
                .expect("checksum committed"));
            assert_eq!(
                store
                    .find_checksum(&org.id, checksum)
                    .await
                    .expect("find checksum")
                    .as_deref(),
                Some(checksum.as_str())
            );
        }
        let (_, recheck) = store
            .create_sandbox(&org.id, &checksums)
            .await
            .expect("create second sandbox");
        assert!(recheck.iter().all(|check| !check.needs_upload));

        // Empty sandboxes are real and can be committed.
        let (empty_sandbox, empty_checks) = store
            .create_sandbox(&org.id, &[])
            .await
            .expect("create empty sandbox");
        assert!(empty_checks.is_empty());
        assert!(store
            .sandbox_exists(&org.id, &empty_sandbox)
            .await
            .expect("empty sandbox exists"));
        assert!(store
            .sandbox_checksums(&org.id, &empty_sandbox)
            .await
            .expect("empty sandbox checksums")
            .is_empty());
        store
            .commit_sandbox(&org.id, &empty_sandbox)
            .await
            .expect("commit empty sandbox");

        let manifest = format!(r#"{{"name":"{cookbook_name}","version":"1.0.0"}}"#);
        let version = store
            .create_cookbook_version(
                &org.id,
                &cookbook_name,
                "1.0.0",
                &manifest,
                &checksums,
                true,
                creator,
            )
            .await
            .expect("create cookbook version");
        assert!(is_32_hex(&version.id));
        assert!(is_32_hex(&version.authz_id));
        assert!(version.frozen);
        assert_eq!(version.checksums, checksums);
        assert_eq!(
            store
                .find_cookbook_version(&org.id, &cookbook_name, "1.0.0")
                .await
                .unwrap()
                .as_ref(),
            Some(&version)
        );
        assert_eq!(
            store
                .list_cookbook_versions(&org.id)
                .await
                .expect("list cookbook versions"),
            vec![version.clone()]
        );
        assert!(matches!(
            store
                .create_cookbook_version(
                    &org.id,
                    &cookbook_name,
                    "1.0.0",
                    &manifest,
                    &checksums,
                    false,
                    creator,
                )
                .await,
            Err(StoreError::Conflict(_))
        ));

        let updated_manifest =
            format!(r#"{{"name":"{cookbook_name}","version":"1.0.0","description":"updated"}}"#);
        let (updated, purged) = store
            .update_cookbook_version(
                &org.id,
                &cookbook_name,
                "1.0.0",
                &updated_manifest,
                &checksums,
                true,
                creator,
            )
            .await
            .expect("update cookbook version");
        assert_eq!(updated.manifest, updated_manifest);
        assert!(updated.frozen);
        assert!(purged.is_empty());
        assert_eq!(
            store
                .find_cookbook_version(&org.id, &cookbook_name, "1.0.0")
                .await
                .unwrap()
                .unwrap()
                .manifest,
            updated_manifest
        );

        // A second version that keeps sharing the first checksum.
        let second_manifest = format!(r#"{{"name":"{cookbook_name}","version":"2.0.0"}}"#);
        let second_version = store
            .create_cookbook_version(
                &org.id,
                &cookbook_name,
                "2.0.0",
                &second_manifest,
                std::slice::from_ref(&first),
                false,
                creator,
            )
            .await
            .expect("create second cookbook version");
        assert_eq!(
            store
                .find_cookbook_version(&org.id, &cookbook_name, "2.0.0")
                .await
                .unwrap()
                .as_ref(),
            Some(&second_version)
        );

        let (deleted, purged) = store
            .delete_cookbook_version(&org.id, &cookbook_name, "1.0.0")
            .await
            .expect("delete first cookbook version");
        assert_eq!(deleted.id, version.id);
        // The first checksum is still referenced by 2.0.0; the second is not.
        assert_eq!(purged, vec![second.clone()]);
        assert!(store
            .checksum_exists(&org.id, &first)
            .await
            .expect("shared checksum retained"));
        assert!(!store
            .checksum_exists(&org.id, &second)
            .await
            .expect("unreferenced checksum purged"));

        let (deleted, purged) = store
            .delete_cookbook_version(&org.id, &cookbook_name, "2.0.0")
            .await
            .expect("delete second cookbook version");
        assert_eq!(deleted.id, second_version.id);
        assert_eq!(purged, vec![first.clone()]);
        assert!(!store
            .checksum_exists(&org.id, &first)
            .await
            .expect("last checksum purged"));
        assert!(matches!(
            store
                .delete_cookbook_version(&org.id, &cookbook_name, "2.0.0")
                .await,
            Err(StoreError::NotFound(_))
        ));

        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }

    pub async fn policies(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = &fixture.creator_authz_id;
        let policy_name = unique("policy");
        let group_name = unique("policy-group");
        let first_body =
            format!(r#"{{"name":"{policy_name}","revision_id":"1.0.0","run_list":[]}}"#);
        let second_body =
            format!(r#"{{"name":"{policy_name}","revision_id":"2.0.0","run_list":[]}}"#);
        let group_body = format!(r#"{{"name":"{group_name}"}}"#);

        let revision = store
            .create_policy_revision(&org.id, &policy_name, "1.0.0", &first_body, creator)
            .await
            .expect("create policy revision");
        assert!(is_32_hex(&revision.id));
        assert!(is_32_hex(&revision.policy_authz_id));
        assert_eq!(revision.serialized_object, first_body);
        assert_eq!(
            store
                .find_policy_revision(&org.id, &policy_name, "1.0.0")
                .await
                .unwrap()
                .as_ref(),
            Some(&revision)
        );
        assert!(store
            .find_policy(&org.id, &policy_name)
            .await
            .expect("find policy")
            .is_some());
        assert!(store
            .list_policies(&org.id)
            .await
            .expect("list policies")
            .iter()
            .any(|policy| policy.name == policy_name));
        assert_eq!(
            store
                .list_policy_revisions(&org.id, &policy_name)
                .await
                .expect("list policy revisions"),
            vec![revision.clone()]
        );
        assert!(store
            .list_all_policy_revisions(&org.id)
            .await
            .expect("list all revisions")
            .contains(&revision));

        let second_revision = store
            .create_policy_revision(&org.id, &policy_name, "2.0.0", &second_body, creator)
            .await
            .expect("create second policy revision");
        assert!(is_32_hex(&second_revision.id));
        assert_eq!(
            store
                .list_policy_revisions(&org.id, &policy_name)
                .await
                .expect("list two revisions")
                .len(),
            2
        );

        let policy_group = store
            .create_policy_group(&org.id, &group_name, &group_body, creator)
            .await
            .expect("create policy group");
        assert_eq!(policy_group.serialized_object, group_body);
        assert_eq!(
            store
                .find_policy_group(&org.id, &group_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&policy_group)
        );
        assert!(store
            .list_policy_groups(&org.id)
            .await
            .expect("list policy groups")
            .contains(&policy_group));

        let (association, created) = store
            .associate_policy_revision(
                &org.id,
                &group_name,
                &policy_name,
                "1.0.0",
                &first_body,
                &group_body,
                creator,
            )
            .await
            .expect("associate policy revision");
        assert!(created);
        assert_eq!(association.policy_group_name, group_name);
        assert_eq!(association.policy_name, policy_name);
        assert_eq!(association.revision_id, "1.0.0");
        assert_eq!(association.serialized_object, first_body);
        assert_eq!(
            store
                .find_policy_group_association(&org.id, &group_name, &policy_name)
                .await
                .unwrap()
                .as_ref(),
            Some(&association)
        );
        assert_eq!(
            store
                .list_policy_group_associations(&org.id, &group_name)
                .await
                .expect("list associations"),
            vec![association.clone()]
        );
        assert_eq!(
            store
                .list_policy_groups_for_revision(&org.id, &policy_name, "1.0.0")
                .await
                .expect("groups for revision"),
            vec![group_name.clone()]
        );

        // Re-association moves the group and reports `created = false`.
        let (reassociated, created) = store
            .associate_policy_revision(
                &org.id,
                &group_name,
                &policy_name,
                "2.0.0",
                &second_body,
                &group_body,
                creator,
            )
            .await
            .expect("re-associate policy revision");
        assert!(!created);
        assert_eq!(reassociated.id, association.id);
        assert_eq!(reassociated.revision_id, "2.0.0");
        assert_eq!(
            store
                .list_policy_groups_for_revision(&org.id, &policy_name, "2.0.0")
                .await
                .expect("groups for second revision"),
            vec![group_name.clone()]
        );
        assert!(store
            .list_policy_groups_for_revision(&org.id, &policy_name, "1.0.0")
            .await
            .expect("groups for first revision")
            .is_empty());

        store
            .delete_policy_group_association(&org.id, &group_name, &policy_name)
            .await
            .expect("delete association");
        assert!(store
            .find_policy_group_association(&org.id, &group_name, &policy_name)
            .await
            .unwrap()
            .is_none());
        store
            .delete_policy_group(&org.id, &group_name)
            .await
            .expect("delete policy group");
        store
            .delete_policy_revision(&org.id, &policy_name, "2.0.0")
            .await
            .expect("delete second revision");
        assert!(store
            .find_policy_revision(&org.id, &policy_name, "2.0.0")
            .await
            .unwrap()
            .is_none());
        store
            .delete_policy(&org.id, &policy_name)
            .await
            .expect("delete policy");
        assert!(store
            .find_policy(&org.id, &policy_name)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .list_policy_revisions(&org.id, &policy_name)
            .await
            .expect("revisions removed with policy")
            .is_empty());
        assert!(matches!(
            store
                .delete_policy_revision(&org.id, &policy_name, "1.0.0")
                .await,
            Err(StoreError::NotFound(_))
        ));

        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }

    pub async fn artifacts(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = &fixture.creator_authz_id;
        let name = unique("artifact");
        let artifact_checksum = checksum();
        let checksums = vec![artifact_checksum.clone()];

        let (sandbox_id, _) = store
            .create_sandbox(&org.id, &checksums)
            .await
            .expect("create artifact sandbox");
        store
            .commit_sandbox(&org.id, &sandbox_id)
            .await
            .expect("commit artifact checksum");

        let first_manifest = format!(r#"{{"name":"{name}","version":"1.0.0"}}"#);
        let first = store
            .create_cookbook_artifact_version(
                &org.id,
                &name,
                "1.0.0",
                &first_manifest,
                &checksums,
                creator,
            )
            .await
            .expect("create artifact version");
        assert!(is_32_hex(&first.authz_id));
        assert_eq!(first.identifier, "1.0.0");
        assert_eq!(first.created_by, *creator);
        assert_eq!(first.checksums, checksums);
        assert_eq!(
            store
                .find_cookbook_artifact_version(&org.id, &name, "1.0.0")
                .await
                .unwrap()
                .as_ref(),
            Some(&first)
        );
        assert_eq!(
            store
                .find_cookbook_artifact(&org.id, &name)
                .await
                .expect("find artifact")
                .expect("artifact exists")
                .identifiers,
            vec!["1.0.0".to_string()]
        );

        let second_manifest = format!(r#"{{"name":"{name}","version":"2.0.0"}}"#);
        let second = store
            .create_cookbook_artifact_version(
                &org.id,
                &name,
                "2.0.0",
                &second_manifest,
                &[],
                creator,
            )
            .await
            .expect("create second artifact version");
        assert_eq!(
            store
                .find_cookbook_artifact(&org.id, &name)
                .await
                .expect("find artifact with two versions")
                .expect("artifact exists")
                .identifiers,
            vec!["1.0.0".to_string(), "2.0.0".to_string()]
        );
        assert!(store
            .list_cookbook_artifacts(&org.id)
            .await
            .expect("list artifacts")
            .iter()
            .any(|artifact| artifact.name == name));
        assert!(matches!(
            store
                .create_cookbook_artifact_version(
                    &org.id,
                    &name,
                    "1.0.0",
                    &first_manifest,
                    &[],
                    creator,
                )
                .await,
            Err(StoreError::Conflict(_))
        ));

        let (deleted, purged) = store
            .delete_cookbook_artifact_version(&org.id, &name, "1.0.0")
            .await
            .expect("delete first artifact version");
        assert_eq!(deleted.id, first.id);
        assert_eq!(purged, checksums);
        assert_eq!(
            store
                .find_cookbook_artifact(&org.id, &name)
                .await
                .expect("find remaining artifact")
                .expect("artifact remains")
                .identifiers,
            vec!["2.0.0".to_string()]
        );
        assert!(!store
            .checksum_exists(&org.id, &artifact_checksum)
            .await
            .expect("artifact checksum purged"));

        let (deleted, purged) = store
            .delete_cookbook_artifact_version(&org.id, &name, "2.0.0")
            .await
            .expect("delete last artifact version");
        assert_eq!(deleted.id, second.id);
        assert!(purged.is_empty());
        assert!(store
            .find_cookbook_artifact(&org.id, &name)
            .await
            .expect("artifact removed")
            .is_none());
        assert!(matches!(
            store
                .delete_cookbook_artifact_version(&org.id, &name, "2.0.0")
                .await,
            Err(StoreError::NotFound(_))
        ));

        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }

    pub async fn membership(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let users_group = store
            .find_group(&org.id, "users")
            .await
            .expect("find users group")
            .expect("users group exists");
        let admins_group = store
            .find_group(&org.id, "admins")
            .await
            .expect("find admins group")
            .expect("admins group exists");
        let read_group = format!("{}_read_access_group", org.name);
        store
            .ensure_global_group(&read_group)
            .await
            .expect("ensure org read access group");

        let user = create_user(store.as_ref(), "member").await;
        let other = create_user(store.as_ref(), "invitee").await;
        let third = create_user(store.as_ref(), "rejected").await;

        store
            .add_user_to_org(&org.id, &user.id)
            .await
            .expect("add member to org");
        assert!(store
            .is_user_in_org(&org.id, &user.id)
            .await
            .expect("member in org"));
        assert!(store
            .user_in_group(&users_group.authz_id, &user.authz_id)
            .await
            .expect("member in users group"));
        let usag = store
            .find_group(&org.id, &user.id)
            .await
            .expect("find user-specific group")
            .expect("user-specific group exists");
        assert!(store
            .user_in_group(&usag.authz_id, &user.authz_id)
            .await
            .expect("member in own user-specific group"));
        let users_members = store
            .list_group_members(&org.id, &users_group.authz_id)
            .await
            .expect("list users group members");
        assert!(users_members.users.is_empty());
        assert_eq!(users_members.groups, vec![user.id.clone()]);
        assert_eq!(
            store.list_org_users(&org.id).await.expect("list org users"),
            vec![user.clone()]
        );
        assert!(store
            .list_user_orgs(&user.id)
            .await
            .expect("list user orgs")
            .contains(org));
        let read_ace = store
            .get_acl_entries(&user.authz_id)
            .await
            .expect("member ACL")
            .into_iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("read ACE");
        assert!(read_ace.groups.contains(&format!("::{read_group}")));

        assert!(!store
            .is_org_admin_for_user(&user.authz_id, &user.id)
            .await
            .expect("not an org admin"));
        store
            .add_group_member(&admins_group.authz_id, &user.authz_id)
            .await
            .expect("add member to admins");
        assert!(store
            .is_org_admin_for_user(&user.authz_id, &user.id)
            .await
            .expect("org admin"));
        assert!(matches!(
            store
                .remove_user_from_org(&org.id, &user.id, &user.username)
                .await,
            Err(StoreError::Forbidden(_))
        ));
        store
            .remove_group_member(&admins_group.authz_id, &user.authz_id)
            .await
            .expect("remove member from admins");

        // Unrelated ACL entries survive org removal.
        store
            .set_acl(
                &user.authz_id,
                Permission::Update,
                Some(&other.authz_id),
                None,
            )
            .await
            .expect("grant unrelated update ACE");
        store
            .remove_user_from_org(&org.id, &user.id, &user.username)
            .await
            .expect("remove member from org");
        assert!(!store
            .is_user_in_org(&org.id, &user.id)
            .await
            .expect("member left org"));
        assert!(!store
            .user_in_group(&users_group.authz_id, &user.authz_id)
            .await
            .expect("member left users group"));
        assert!(store
            .find_group(&org.id, &user.id)
            .await
            .expect("find removed user-specific group")
            .is_none());
        assert!(store
            .list_group_members(&org.id, &users_group.authz_id)
            .await
            .expect("list users group after removal")
            .groups
            .is_empty());
        let after_removal = store
            .get_acl_entries(&user.authz_id)
            .await
            .expect("member ACL after removal");
        assert!(!after_removal
            .iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("read ACE after removal")
            .groups
            .contains(&format!("::{read_group}")));
        let update_users = &after_removal
            .iter()
            .find(|entry| entry.permission == Permission::Update)
            .expect("update ACE after removal")
            .users;
        // The user's own ACE survives removal; the unrelated grant is kept too.
        assert!(update_users.contains(&user.username));
        assert!(update_users.contains(&other.username));

        // Invitations: accept associates and consumes.
        let invite = store
            .create_invitation(&org.id, &other.id, &user.id)
            .await
            .expect("create invitation");
        assert_eq!(
            store
                .find_invitation(&invite.id)
                .await
                .expect("find invite"),
            Some(invite.clone())
        );
        assert_eq!(
            store
                .list_org_invitations(&org.id)
                .await
                .expect("list org invitations"),
            vec![invite.clone()]
        );
        assert_eq!(
            store
                .list_user_invitations(&other.id)
                .await
                .expect("list user invitations"),
            vec![invite.clone()]
        );
        assert_eq!(
            store
                .count_user_invitations(&other.id)
                .await
                .expect("count invitations"),
            1
        );
        assert_eq!(
            store
                .accept_invitation(&invite.id)
                .await
                .expect("accept invitation"),
            invite
        );
        assert!(store
            .is_user_in_org(&org.id, &other.id)
            .await
            .expect("invitee associated"));
        assert!(store
            .user_in_group(&users_group.authz_id, &other.authz_id)
            .await
            .expect("invitee in users group"));
        assert!(store
            .find_group(&org.id, &other.id)
            .await
            .expect("find invitee user-specific group")
            .is_some());
        assert_eq!(
            store
                .list_group_members(&org.id, &users_group.authz_id)
                .await
                .expect("list users group after invite")
                .groups,
            vec![other.id.clone()]
        );
        assert_eq!(
            store
                .count_user_invitations(&other.id)
                .await
                .expect("invitation consumed"),
            0
        );
        assert!(!store
            .users_share_org(&user.id, &other.id)
            .await
            .expect("removed member no longer shares the org"));

        // Reject leaves no association.
        let rejected = store
            .create_invitation(&org.id, &third.id, &user.id)
            .await
            .expect("create rejected invitation");
        assert_eq!(
            store
                .reject_invitation(&rejected.id)
                .await
                .expect("reject invitation"),
            rejected
        );
        assert!(!store
            .is_user_in_org(&org.id, &third.id)
            .await
            .expect("rejected user not associated"));
        assert!(store
            .find_invitation(&rejected.id)
            .await
            .expect("rejected invitation consumed")
            .is_none());

        // Explicit delete and missing-invitation errors.
        let deleted = store
            .create_invitation(&org.id, &third.id, &user.id)
            .await
            .expect("create deletable invitation");
        store
            .delete_invitation(&deleted.id)
            .await
            .expect("delete invitation");
        assert!(store
            .find_invitation(&deleted.id)
            .await
            .expect("deleted invitation")
            .is_none());
        assert!(matches!(
            store.delete_invitation(&deleted.id).await,
            Err(StoreError::NotFound(_))
        ));

        // Direct association helper also grants the read ACE.
        let direct = create_user(store.as_ref(), "direct-member").await;
        store
            .associate_user_org(&org.id, &direct.id)
            .await
            .expect("associate user directly");
        assert!(store
            .is_user_in_org(&org.id, &direct.id)
            .await
            .expect("direct association"));
        assert!(store
            .get_acl_entries(&direct.authz_id)
            .await
            .expect("direct member ACL")
            .iter()
            .find(|entry| entry.permission == Permission::Read)
            .expect("direct read ACE")
            .groups
            .contains(&format!("::{read_group}")));

        cleanup_org(store.as_ref(), &org.id).await;
        for username in [
            &user.username,
            &other.username,
            &third.username,
            &direct.username,
        ] {
            store
                .delete_user(username)
                .await
                .expect("delete matrix member");
        }
        vec![read_group]
    }

    pub async fn oauth(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let owner = create_user(store.as_ref(), "oauth-owner").await;
        let app_name = unique("oauth-app");
        let app = store
            .create_oauth_application(
                &app_name,
                "https://example.test/oauth/callback",
                "read profile",
                false,
            )
            .await
            .expect("create oauth application");
        assert!(!app.confidential);
        assert!(!app.uid.is_empty());
        assert!(!app.secret.is_empty());
        assert_eq!(
            store
                .find_oauth_application(app.id)
                .await
                .expect("find oauth application")
                .as_ref(),
            Some(&app)
        );
        assert_eq!(
            store
                .find_oauth_application_by_uid(&app.uid)
                .await
                .expect("find oauth application by uid")
                .as_ref(),
            Some(&app)
        );
        assert!(store
            .list_oauth_applications()
            .await
            .expect("list oauth applications")
            .contains(&app));

        let updated = store
            .update_oauth_application(
                app.id,
                Some(&format!("{app_name}-renamed")),
                Some("https://example.test/oauth/renamed"),
            )
            .await
            .expect("update oauth application");
        assert_eq!(updated.name, format!("{app_name}-renamed"));
        assert_eq!(updated.redirect_uri, "https://example.test/oauth/renamed");
        assert_eq!(updated.uid, app.uid);
        assert_eq!(updated.secret, app.secret);

        let grant = store
            .create_oauth_access_grant(
                &owner.id,
                app.id,
                "https://example.test/oauth/callback",
                "read",
                600,
            )
            .await
            .expect("create oauth grant");
        assert_eq!(grant.resource_owner_id, owner.id);
        assert_eq!(grant.application_id, app.id);
        assert_eq!(grant.expires_in, 600);
        assert_eq!(grant.scopes, "read");
        assert!(grant.revoked_at.is_none());
        assert_eq!(
            store
                .find_oauth_access_grant(&grant.token)
                .await
                .expect("find oauth grant")
                .as_ref(),
            Some(&grant)
        );
        store
            .revoke_oauth_access_grant(&grant.token)
            .await
            .expect("revoke oauth grant");
        assert!(store
            .find_oauth_access_grant(&grant.token)
            .await
            .expect("find revoked grant")
            .expect("grant still exists")
            .revoked_at
            .is_some());

        let token = store
            .create_oauth_access_token(&owner.id, app.id, "read", Some(3600), true)
            .await
            .expect("create oauth token");
        assert_eq!(token.resource_owner_id.as_deref(), Some(owner.id.as_str()));
        assert_eq!(token.application_id, Some(app.id));
        assert_eq!(token.expires_in, Some(3600));
        assert_eq!(token.scopes, "read");
        assert!(token.revoked_at.is_none());
        let refresh = token.refresh_token.clone().expect("refresh token");
        assert_eq!(
            store
                .find_oauth_access_token(&token.token)
                .await
                .expect("find oauth token")
                .as_ref(),
            Some(&token)
        );
        assert_eq!(
            store
                .find_oauth_access_token_by_refresh(&refresh)
                .await
                .expect("find oauth token by refresh")
                .as_ref(),
            Some(&token)
        );
        store
            .revoke_oauth_access_token(&token.token)
            .await
            .expect("revoke oauth token");
        assert!(store
            .find_oauth_access_token(&token.token)
            .await
            .expect("find revoked oauth token")
            .expect("token still exists")
            .revoked_at
            .is_some());

        let second = store
            .create_oauth_access_token(&owner.id, app.id, "read", None, false)
            .await
            .expect("create token without refresh");
        assert!(second.refresh_token.is_none());
        assert!(second.expires_in.is_none());
        store
            .revoke_oauth_access_token_by_refresh(&refresh)
            .await
            .expect("revoke token by refresh");
        assert!(store
            .find_oauth_access_token_by_refresh(&refresh)
            .await
            .expect("find token revoked by refresh")
            .expect("token still exists")
            .revoked_at
            .is_some());

        // Browser sessions.
        let session = store
            .create_id_session(&owner.username)
            .await
            .expect("create id session");
        assert_eq!(
            store.find_id_session(&session).await.expect("find session"),
            Some(owner.username.clone())
        );
        store
            .delete_id_session(&session)
            .await
            .expect("delete id session");
        assert!(store
            .find_id_session(&session)
            .await
            .expect("deleted session")
            .is_none());

        // Password resets are single-use; concurrent consumers cannot both win.
        let reset = store
            .create_password_reset_token(&owner.username)
            .await
            .expect("create password reset token");
        assert_eq!(
            store
                .find_password_reset_token(&reset, 3600)
                .await
                .expect("find password reset token"),
            Some(owner.username.clone())
        );
        assert_eq!(
            store
                .consume_password_reset_token(&reset, 3600)
                .await
                .expect("consume password reset token"),
            Some(owner.username.clone())
        );
        assert_eq!(
            store
                .consume_password_reset_token(&reset, 3600)
                .await
                .expect("consume password reset token twice"),
            None
        );

        let concurrent = store
            .create_password_reset_token(&owner.username)
            .await
            .expect("create concurrent reset token");
        let results = run_concurrently(8, {
            let store = Arc::clone(store);
            let token = concurrent.clone();
            move |_| {
                let store = Arc::clone(&store);
                let token = token.clone();
                async move { store.consume_password_reset_token(&token, 3600).await }
            }
        })
        .await;
        let winners = results
            .into_iter()
            .map(|result| result.expect("concurrent reset consumer"))
            .filter(Option::is_some)
            .count();
        assert_eq!(winners, 1, "exactly one reset consumer must win");

        // Deleting the application cascades grants and tokens.
        store
            .delete_oauth_application(app.id)
            .await
            .expect("delete oauth application");
        assert!(store
            .find_oauth_application(app.id)
            .await
            .expect("find deleted application")
            .is_none());
        assert!(store
            .find_oauth_access_grant(&grant.token)
            .await
            .expect("deleted application grant")
            .is_none());
        assert!(store
            .find_oauth_access_token(&token.token)
            .await
            .expect("deleted application token")
            .is_none());
        assert!(store
            .find_oauth_access_token(&second.token)
            .await
            .expect("deleted application second token")
            .is_none());

        store
            .delete_user(&owner.username)
            .await
            .expect("delete oauth owner");
        Vec::new()
    }

    pub async fn kv_secrets(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        // The maintenance flag is global to the database. Serialize the test
        // inside this binary; separate processes must not run their
        // maintenance assertions against the same database concurrently.
        static MAINTENANCE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _maintenance_lock = MAINTENANCE_LOCK.lock().await;

        let key = unique("matrix:kv");
        assert_eq!(store.get_kv(&key).await.expect("missing kv"), None);
        store.set_kv(&key, "one").await.expect("set kv");
        assert_eq!(
            store.get_kv(&key).await.expect("get kv"),
            Some("one".into())
        );
        store.set_kv(&key, "two").await.expect("overwrite kv");
        assert_eq!(
            store.get_kv(&key).await.expect("get updated kv"),
            Some("two".into())
        );
        assert!(store.remove_kv(&key).await.expect("remove kv"));
        assert!(!store.remove_kv(&key).await.expect("remove missing kv"));

        let maintenance_was = store
            .maintenance_enabled()
            .await
            .expect("read maintenance flag");
        // Run the toggled assertions in a child task so the parent can restore
        // the flag before propagating a panic. If the toggle fails before it
        // takes effect, restoring the original value is harmless.
        let maintenance_store = Arc::clone(store);
        let toggled = tokio::spawn(async move {
            maintenance_store
                .set_maintenance_enabled(!maintenance_was)
                .await
                .expect("toggle maintenance");
            assert_eq!(
                maintenance_store
                    .maintenance_enabled()
                    .await
                    .expect("read toggled maintenance"),
                !maintenance_was
            );
        })
        .await;
        store
            .set_maintenance_enabled(maintenance_was)
            .await
            .expect("restore maintenance flag");
        match toggled {
            Ok(()) => {}
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => panic!("maintenance worker failed: {error}"),
        }
        assert_eq!(
            store
                .maintenance_enabled()
                .await
                .expect("read restored maintenance"),
            maintenance_was,
            "maintenance flag must be restored"
        );

        let group = "matrix-secrets";
        let name = unique("secret");
        assert_eq!(
            store
                .get_secret(group, &name)
                .await
                .expect("missing secret"),
            None
        );
        store
            .set_secret(group, &name, "s3cr3t")
            .await
            .expect("set secret");
        assert_eq!(
            store.get_secret(group, &name).await.expect("get secret"),
            Some("s3cr3t".into())
        );
        store
            .set_secret(group, &name, "rotated")
            .await
            .expect("rotate secret");
        assert_eq!(
            store
                .get_secret(group, &name)
                .await
                .expect("get rotated secret"),
            Some("rotated".into())
        );
        store
            .remove_secret(group, &name)
            .await
            .expect("remove secret");
        assert!(matches!(
            store.remove_secret(group, &name).await,
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store.set_secret("bad group", &name, "x").await,
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            store.set_secret(group, &name, "").await,
            Err(StoreError::Invalid(_))
        ));

        let absent = unique("bootstrap");
        assert_eq!(
            store
                .set_secret_if_absent(group, &absent, "first")
                .await
                .expect("first bootstrap"),
            "first"
        );
        assert_eq!(
            store
                .set_secret_if_absent(group, &absent, "second")
                .await
                .expect("second bootstrap"),
            "first"
        );
        assert_eq!(
            store
                .get_secret(group, &absent)
                .await
                .expect("bootstrap secret"),
            Some("first".into())
        );
        let race_name = format!("{absent}-race");
        let results = run_concurrently(8, {
            let store = Arc::clone(store);
            let race_name = race_name.clone();
            move |index| {
                let store = Arc::clone(&store);
                let race_name = race_name.clone();
                let value = format!("value-{index}");
                async move { store.set_secret_if_absent(group, &race_name, &value).await }
            }
        })
        .await;
        let values: Vec<_> = results
            .into_iter()
            .map(|result| result.expect("concurrent bootstrap insert"))
            .collect();
        assert!(
            values.iter().all(|value| value == &values[0]),
            "concurrent bootstrap must converge: {values:?}"
        );
        assert_eq!(
            store
                .get_secret(group, &race_name)
                .await
                .expect("raced bootstrap secret"),
            Some(values[0].clone())
        );
        store
            .remove_secret(group, &race_name)
            .await
            .expect("remove raced secret");
        store
            .remove_secret(group, &absent)
            .await
            .expect("remove bootstrap secret");
        Vec::new()
    }

    pub async fn bulk_lookups(store: &Arc<dyn MetadataStore>) -> Vec<String> {
        let fixture = fixture(store.as_ref()).await;
        let org = &fixture.org;
        let creator = &fixture.creator_authz_id;

        let mut node_names = vec![unique("bulk-node"), unique("bulk-node")];
        node_names.sort();
        let mut node_ids = Vec::new();
        for (index, name) in node_names.iter().enumerate() {
            let body = format!(r#"{{"name":"{name}"}}"#);
            let node = store
                .create_node(
                    &org.id,
                    NewNode {
                        name,
                        body: &body,
                        environment: "_default",
                        policy_name: None,
                        policy_group: None,
                        creator_authz_id: creator,
                    },
                )
                .await
                .expect("create node for bulk lookup");
            if index == 0 {
                node_ids.push(node.id);
            }
        }
        assert_eq!(
            store
                .list_node_names(&org.id)
                .await
                .expect("list node names"),
            node_names
        );
        let nodes = store
            .find_nodes_by_ids(&org.id, &node_ids)
            .await
            .expect("find nodes by id");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, node_names[0]);
        assert!(store
            .find_nodes_by_ids(&org.id, &[])
            .await
            .expect("empty node id lookup")
            .is_empty());

        let role_name = unique("bulk-role");
        let role_body = format!(r#"{{"name":"{role_name}","run_list":[]}}"#);
        let role = store
            .create_role(&org.id, &role_name, &role_body, creator)
            .await
            .expect("create role for bulk lookup");
        let roles = store
            .find_roles_by_ids(&org.id, std::slice::from_ref(&role.id))
            .await
            .expect("find roles by id");
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, role_name);
        assert!(store
            .list_role_names(&org.id)
            .await
            .expect("list role names")
            .contains(&role_name));

        let environment_name = unique("bulk-env");
        let environment_body = format!(r#"{{"name":"{environment_name}"}}"#);
        let environment = store
            .create_environment(&org.id, &environment_name, &environment_body, creator)
            .await
            .expect("create environment for bulk lookup");
        let environments = store
            .find_environments_by_ids(&org.id, std::slice::from_ref(&environment.id))
            .await
            .expect("find environments by id");
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].name, environment_name);
        assert!(store
            .list_environment_names(&org.id)
            .await
            .expect("list environment names")
            .contains(&environment_name));

        let data_bag_name = unique("bulk-bag");
        store
            .create_data_bag(&org.id, &data_bag_name, creator)
            .await
            .expect("create data bag for bulk lookup");
        let item_name = unique("bulk-item");
        let item = store
            .create_data_bag_item(&org.id, &data_bag_name, &item_name, "{\"value\":1}")
            .await
            .expect("create data bag item for bulk lookup");
        let items = store
            .find_data_bag_items_by_ids(&org.id, &data_bag_name, std::slice::from_ref(&item.id))
            .await
            .expect("find data bag items by id");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].item_name, item_name);
        assert!(store
            .list_data_bag_names(&org.id)
            .await
            .expect("list data bag names")
            .contains(&data_bag_name));

        let client_name = unique("bulk-client");
        let client = store
            .create_client(&org.id, &client_name, false, false)
            .await
            .expect("create client for bulk lookup");
        let clients = store
            .find_clients_by_ids(&org.id, std::slice::from_ref(&client.id))
            .await
            .expect("find clients by id");
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].name, client_name);

        let policy_name = unique("bulk-policy");
        let policy_body =
            format!(r#"{{"name":"{policy_name}","revision_id":"1.0.0","run_list":[]}}"#);
        let policy = store
            .create_policy_revision(&org.id, &policy_name, "1.0.0", &policy_body, creator)
            .await
            .expect("create policy for bulk lookup");
        assert!(store
            .list_policy_revision_refs(&org.id)
            .await
            .expect("list policy revision refs")
            .contains(&(policy.name.clone(), policy.revision_id.clone())));

        let checksums = vec![checksum(), checksum()];
        let (sandbox_id, _) = store
            .create_sandbox(&org.id, &checksums)
            .await
            .expect("create sandbox for checksum lookup");
        store
            .commit_sandbox(&org.id, &sandbox_id)
            .await
            .expect("commit sandbox for checksum lookup");
        assert_eq!(
            store
                .existing_checksums(
                    &org.id,
                    &[checksums[0].clone(), "missing-checksum".to_string()]
                )
                .await
                .expect("existing checksums"),
            vec![checksums[0].clone()]
        );
        assert!(store
            .existing_checksums(&org.id, &[])
            .await
            .expect("empty checksum lookup")
            .is_empty());

        cleanup_org(store.as_ref(), &org.id).await;
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Test registration
// ---------------------------------------------------------------------------

macro_rules! matrix {
    ($backend:ident, $make:path, [$($test:ident),+ $(,)?]) => {
        mod $backend {
            use super::*;

            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
                async fn $test() {
                    let Some(backend) = $make().await else {
                        return;
                    };
                    let global_groups = suite::$test(&backend.metadata).await;
                    backend.cleanup_global_groups(&global_groups).await;
                }
            )+
        }
    };
}

matrix!(
    sqlite,
    sqlite_store,
    [
        org_deletion,
        identity,
        authz,
        objects,
        cookbooks,
        policies,
        artifacts,
        membership,
        oauth,
        kv_secrets,
        bulk_lookups,
    ]
);

matrix!(
    postgres,
    postgres_store,
    [
        org_deletion,
        identity,
        authz,
        objects,
        cookbooks,
        policies,
        artifacts,
        membership,
        oauth,
        kv_secrets,
        bulk_lookups,
    ]
);
