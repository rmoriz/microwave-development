mod common;

use common::{json_route, options, raw_route, start_server};
use microwave_migrate::crawl_from_goiardi;
use serde_json::json;

const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");

const USER: &str = r#"{"username":"admin","name":"admin","email":"ops@example.com","admin":true,"public_key":"<PUBLIC_KEY>"}"#;
const CLIENT: &str =
    r#"{"name":"web1","admin":false,"validator":false,"public_key":"<PUBLIC_KEY>"}"#;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn crawls_users_and_clients_with_keys_and_marks_the_layout() {
    let dir = tempdir();
    let dir_path = dir.path();
    let routes = vec![
        ("/users", json_route(json!({"admin": "/users/admin"}))),
        ("/users/admin", raw_route(USER)),
        ("/clients", json_route(json!({"web1": "/clients/web1"}))),
        ("/clients/web1", raw_route(CLIENT)),
    ];
    let (url, records) = start_server(routes);
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir_path)).unwrap();
    assert_eq!(snapshot.source.layout.as_deref(), Some("goiardi"));
    assert_eq!(snapshot.data.users[0].username, "admin");
    assert!(snapshot.data.users[0].admin);
    assert_eq!(snapshot.data.users[0].password_hash, "");
    assert_eq!(snapshot.data.clients[0].name, "web1");
    assert_eq!(snapshot.data.actor_keys[0].public_key, PUBLIC_KEY);
    assert_eq!(summary.clients, 1);
    assert!(records
        .lock()
        .unwrap()
        .iter()
        .all(|record| record.method == "GET"));
}
