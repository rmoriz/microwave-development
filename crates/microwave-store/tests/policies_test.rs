use microwave_store::{SqliteStore, StoreError};

fn setup() -> (SqliteStore, String, String) {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("pivotal", "pivotal@example.com", "bcrypt", "h", "s", true)
        .unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    store.create_container_for_org(&org.id, "policies").unwrap();
    store
        .create_container_for_org(&org.id, "policy_groups")
        .unwrap();
    (store, org.id, creator.authz_id)
}

const POLICY: &str = r#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{}}"#;

#[test]
fn creates_policies_implicitly_and_creates_revisions() {
    let (store, org_id, creator) = setup();

    let revision = store
        .create_policy_revision(&org_id, "app", "rev1", POLICY, &creator)
        .unwrap();
    assert_eq!(revision.name, "app");
    assert_eq!(revision.revision_id, "rev1");
    assert_eq!(revision.serialized_object, POLICY);

    let policy = store.find_policy(&org_id, "app").unwrap().unwrap();
    assert_eq!(policy.name, "app");
    assert_eq!(policy.authz_id, revision.policy_authz_id);

    assert_eq!(store.list_policies(&org_id).unwrap().len(), 1);
    assert_eq!(
        store.list_policy_revisions(&org_id, "app").unwrap().len(),
        1
    );

    let second = r#"{"name":"app","revision_id":"rev2","run_list":[],"cookbook_locks":{}}"#;
    store
        .create_policy_revision(&org_id, "app", "rev2", second, &creator)
        .unwrap();
    assert_eq!(
        store.list_policy_revisions(&org_id, "app").unwrap().len(),
        2
    );

    let duplicate = store.create_policy_revision(&org_id, "app", "rev1", POLICY, &creator);
    assert!(matches!(duplicate, Err(StoreError::Conflict(_))));
}

#[test]
fn rejects_invalid_revisions() {
    let (store, org_id, creator) = setup();

    assert!(matches!(
        store.create_policy_revision(&org_id, "app", "", POLICY, &creator),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.create_policy_revision(&org_id, "app", "not valid", POLICY, &creator),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.create_policy_revision(&org_id, "bad name", "rev1", POLICY, &creator),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.create_policy_revision(&org_id, "app", "rev1", "not json", &creator),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.create_policy_revision(&org_id, "app", "rev1", "[]", &creator),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn associates_revisions_with_policy_groups() {
    let (store, org_id, creator) = setup();

    let (association, created) = store
        .associate_policy_revision(&org_id, "dev", "app", "rev1", POLICY, "{}", &creator)
        .unwrap();
    assert!(created);
    assert_eq!(association.policy_group_name, "dev");
    assert_eq!(association.policy_name, "app");
    assert_eq!(association.revision_id, "rev1");
    assert_eq!(association.serialized_object, POLICY);

    let group = store.find_policy_group(&org_id, "dev").unwrap().unwrap();
    assert_eq!(group.authz_id, association.policy_group_authz_id);
    assert_eq!(
        store
            .list_policy_groups_for_revision(&org_id, "app", "rev1")
            .unwrap(),
        vec!["dev".to_string()]
    );

    let second = r#"{"name":"app","revision_id":"rev2","run_list":[],"cookbook_locks":{}}"#;
    let (updated, created) = store
        .associate_policy_revision(&org_id, "dev", "app", "rev2", second, "{}", &creator)
        .unwrap();
    assert!(!created);
    assert_eq!(updated.revision_id, "rev2");

    let found = store
        .find_policy_group_association(&org_id, "dev", "app")
        .unwrap()
        .unwrap();
    assert_eq!(found.revision_id, "rev2");
    assert_eq!(
        store
            .list_policy_group_associations(&org_id, "dev")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_policy_groups_for_revision(&org_id, "app", "rev1")
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn deletes_policy_groups_and_their_associations() {
    let (store, org_id, creator) = setup();
    store
        .associate_policy_revision(&org_id, "dev", "app", "rev1", POLICY, "{}", &creator)
        .unwrap();

    store.delete_policy_group(&org_id, "dev").unwrap();
    assert!(store.find_policy_group(&org_id, "dev").unwrap().is_none());
    assert!(store
        .find_policy_group_association(&org_id, "dev", "app")
        .unwrap()
        .is_none());
    // The policy and revision survive deleting the group.
    assert!(store.find_policy(&org_id, "app").unwrap().is_some());
    assert!(store
        .find_policy_revision(&org_id, "app", "rev1")
        .unwrap()
        .is_some());
}

#[test]
fn deletes_policies_revisions_and_associations() {
    let (store, org_id, creator) = setup();
    store
        .associate_policy_revision(&org_id, "dev", "app", "rev1", POLICY, "{}", &creator)
        .unwrap();

    store
        .delete_policy_revision(&org_id, "app", "rev1")
        .unwrap();
    assert!(store
        .find_policy_revision(&org_id, "app", "rev1")
        .unwrap()
        .is_none());
    // The policy authz object is left intact so the policy can be reused.
    assert!(store.find_policy(&org_id, "app").unwrap().is_some());
    assert_eq!(
        store
            .list_policy_group_associations(&org_id, "dev")
            .unwrap()
            .len(),
        0
    );

    store.delete_policy(&org_id, "app").unwrap();
    assert!(store.find_policy(&org_id, "app").unwrap().is_none());
    assert!(store
        .find_policy_group_association(&org_id, "dev", "app")
        .unwrap()
        .is_none());
}

#[test]
fn deleting_a_policy_cascades_associations() {
    let (store, org_id, creator) = setup();
    store
        .associate_policy_revision(&org_id, "dev", "app", "rev1", POLICY, "{}", &creator)
        .unwrap();

    store.delete_policy(&org_id, "app").unwrap();
    assert!(store.find_policy(&org_id, "app").unwrap().is_none());
    assert!(store
        .find_policy_group_association(&org_id, "dev", "app")
        .unwrap()
        .is_none());
    // The policy group itself is left intact.
    assert!(store.find_policy_group(&org_id, "dev").unwrap().is_some());
}

#[test]
fn creates_policy_groups_directly() {
    let (store, org_id, creator) = setup();
    let group = store
        .create_policy_group(&org_id, "prod", "{}", &creator)
        .unwrap();
    assert_eq!(group.name, "prod");
    assert_eq!(store.list_policy_groups(&org_id).unwrap().len(), 1);
    assert!(matches!(
        store.create_policy_group(&org_id, "prod", "{}", &creator),
        Err(StoreError::Conflict(_))
    ));
}
