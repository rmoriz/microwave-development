use microwave_store::{NewNode, SqliteStore};

#[test]
fn counts_nodes_across_every_organization() {
    let store = SqliteStore::open_in_memory().unwrap();
    let acme = store.create_org("acme", "Acme").unwrap();
    let beta = store.create_org("beta", "Beta").unwrap();
    store.create_container_for_org(&acme.id, "nodes").unwrap();
    store.create_container_for_org(&beta.id, "nodes").unwrap();

    assert_eq!(store.count_nodes().unwrap(), 0);

    for (org, name) in [(&acme, "web1"), (&acme, "web2"), (&beta, "db1")] {
        store
            .create_node(
                &org.id,
                NewNode {
                    name,
                    body: r#"{"name":"node","run_list":[]}"#,
                    environment: "_default",
                    policy_name: None,
                    policy_group: None,
                    creator_authz_id: &org.authz_id,
                },
            )
            .unwrap();
    }

    assert_eq!(store.count_nodes().unwrap(), 3);
}
