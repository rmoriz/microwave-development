mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::ActorType;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap();
    let status = response.status();
    let value = support::response_json(response).await;
    (status, value)
}

async fn create_org(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

const REVISION: &[u8] = br#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;

#[tokio::test]
async fn manages_policies_revisions_groups_and_associations() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, body) = request(&app, "GET", "/organizations/acme/policies", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body, serde_json::from_slice::<Value>(REVISION).unwrap());

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, body) = request(&app, "GET", "/organizations/acme/policies", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["app"]["uri"],
        "http://0.0.0.0:8000/organizations/acme/policies/app"
    );
    assert_eq!(body["app"]["revisions"]["rev1"], serde_json::json!({}));

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revisions"]["rev1"], serde_json::json!({}));

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revisions"]["rev1"], serde_json::json!({}));

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "app");
    assert_eq!(body["revision_id"], "rev1");
    assert_eq!(body["policy_group_list"], serde_json::json!([]));

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/policy_groups",
        br#"{"name":"prod"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["uri"],
        "http://0.0.0.0:8000/organizations/acme/policy_groups/prod"
    );

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/policy_groups/prod/policies/app",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policy_groups/prod/policies/app",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revision_id"], "rev1");

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policy_group_list"], serde_json::json!(["prod"]));

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policy_groups",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["prod"]["uri"],
        "http://0.0.0.0:8000/organizations/acme/policy_groups/prod"
    );
    assert_eq!(body["prod"]["policies"]["app"]["revision_id"], "rev1");

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policy_groups/prod",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["uri"],
        "http://0.0.0.0:8000/organizations/acme/policy_groups/prod"
    );
    assert_eq!(body["policies"]["app"]["revision_id"], "rev1");

    let (status, body) = request(
        &app,
        "DELETE",
        "/organizations/acme/policy_groups/prod/policies/app",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::from_slice::<Value>(REVISION).unwrap());

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policy_groups/prod",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policies"], serde_json::json!({}));

    let (status, body) = request(
        &app,
        "DELETE",
        "/organizations/acme/policy_groups/prod",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({
            "uri": "http://0.0.0.0:8000/organizations/acme/policy_groups/prod",
            "policies": {}
        })
    );

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/policy_groups/prod",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = request(
        &app,
        "DELETE",
        "/organizations/acme/policies/app",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "revisions": { "rev1": {} } }));

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_revision_keeps_the_policy_available() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "DELETE",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::from_slice::<Value>(REVISION).unwrap());

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The policy object and its authz id survive, so the revision can be
    // posted again under the same policy name.
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn creates_a_policy_through_the_collection_route() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/policies",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        body["uri"],
        "http://0.0.0.0:8000/organizations/acme/policies/app"
    );

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revision_id"], "rev1");
}

#[tokio::test]
async fn rejects_invalid_revision_documents() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        br#"{"name":"app","run_list":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        br#"{"name":"other","revision_id":"rev1"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        br#"{"name":"app","revision_id":"has space"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn requires_permission_on_the_policy_container() {
    let state = support::state_with_pivotal();
    let outsider = state
        .sqlite()
        .create_user(
            "outsider",
            "outsider@example.com",
            "bcrypt",
            "h",
            "s",
            false,
        )
        .unwrap();
    state
        .sqlite()
        .add_actor_key(
            &outsider.id,
            ActorType::User,
            "default",
            support::PUBLIC_KEY,
        )
        .unwrap();
    let app = support::app(state);
    create_org(&app).await;

    let (status, _) = request(&app, "GET", "/organizations/acme/policies", b"", "outsider").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION,
        "outsider",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

fn minimal_policy() -> Value {
    serde_json::json!({
        "revision_id": "909c26701e291510eacdc6c06d626b9fa5350d25",
        "name": "some_policy_name",
        "run_list": ["recipe[policyfile_demo::default]"],
        "cookbook_locks": {
            "policyfile_demo": {
                "identifier": "f04cc40faf628253fe7d9566d66a1733fb1afbe9",
                "version": "1.2.3"
            }
        }
    })
}

#[tokio::test]
async fn rejects_invalid_policy_group_association_documents() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let url = "/organizations/acme/policy_groups/some_policy_group/policies/some_policy_name";

    let cases: Vec<(Value, &str)> = vec![
        (
            {
                let mut body = minimal_policy();
                body.as_object_mut().unwrap().remove("revision_id");
                body
            },
            "Field 'revision_id' missing",
        ),
        (
            {
                let mut body = minimal_policy();
                body["revision_id"] = json!("");
                body
            },
            "Field 'revision_id' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["revision_id"] = json!("f".repeat(256));
                body
            },
            "Field 'revision_id' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["revision_id"] = json!("invalid invalid");
                body
            },
            "Field 'revision_id' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body.as_object_mut().unwrap().remove("name");
                body
            },
            "Field 'name' missing",
        ),
        (
            {
                let mut body = minimal_policy();
                body["name"] = json!("monkeypants");
                body
            },
            "Field 'name' invalid : some_policy_name does not match monkeypants",
        ),
        (
            {
                let mut body = minimal_policy();
                body["name"] = json!("z".repeat(256));
                body
            },
            "Field 'name' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["name"] = json!("invalid invalid");
                body
            },
            "Field 'name' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body.as_object_mut().unwrap().remove("run_list");
                body
            },
            "Field 'run_list' missing",
        ),
        (
            {
                let mut body = minimal_policy();
                body["run_list"] = json!({});
                body
            },
            "Field 'run_list' is not a valid run list",
        ),
        (
            {
                let mut body = minimal_policy();
                body["run_list"] = json!([123]);
                body
            },
            "Field 'run_list' is not a valid run list",
        ),
        (
            {
                let mut body = minimal_policy();
                body["run_list"] = json!(["recipe["]);
                body
            },
            "Field 'run_list' is not a valid run list",
        ),
        (
            {
                let mut body = minimal_policy();
                body["run_list"] = json!(["role[foo]"]);
                body
            },
            "Field 'run_list' is not a valid run list",
        ),
        (
            {
                let mut body = minimal_policy();
                body["run_list"] = json!(["recipe[foo]"]);
                body
            },
            "Field 'run_list' is not a valid run list",
        ),
        (
            {
                let mut body = minimal_policy();
                body.as_object_mut().unwrap().remove("cookbook_locks");
                body
            },
            "Field 'cookbook_locks' missing",
        ),
        (
            {
                let mut body = minimal_policy();
                body["cookbook_locks"] = json!([]);
                body
            },
            "Field 'cookbook_locks' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["cookbook_locks"]["invalid_member"] = json!([]);
                body
            },
            "Field 'cookbook_locks' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["cookbook_locks"]["invalid_member"] =
                    json!({"dotted_decimal_identifier": "1.2.3"});
                body
            },
            "Field 'identifier' missing",
        ),
        (
            {
                let mut body = minimal_policy();
                body["cookbook_locks"]["invalid_member"] =
                    json!({"identifier": "a".repeat(256), "version": "1.2.3"});
                body
            },
            "Field 'identifier' invalid",
        ),
        (
            {
                let mut body = minimal_policy();
                body["cookbook_locks"]["invalid_member"] = json!({
                    "identifier": "123def",
                    "version": "1.2.3",
                    "dotted_decimal_identifier": "foo"
                });
                body
            },
            "Field 'dotted_decimal_identifier' is not a valid version",
        ),
    ];

    for (body, message) in cases {
        let payload = serde_json::to_vec(&body).unwrap();
        let (status, error) = request(&app, "PUT", url, &payload, "pivotal").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body {body}");
        assert_eq!(error, json!({ "error": [message] }), "body {body}");
    }
}

#[tokio::test]
async fn accepts_valid_association_documents() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let mut body = minimal_policy();
    body["name"] = json!("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqurstuvwxyz0123456789-_:.");
    body["revision_id"] =
        json!("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqurstuvwxyz0123456789-_:.");
    body["cookbook_locks"]["edge_case"] = json!({
        "identifier": "a".repeat(255),
        "version": "1.2.3"
    });

    let name = body["name"].as_str().unwrap();
    let (status, response) = request(
        &app,
        "PUT",
        &format!("/organizations/acme/policy_groups/some_policy_group/policies/{name}"),
        &serde_json::to_vec(&body).unwrap(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response, body);
}

#[tokio::test]
async fn revision_policy_group_list_uses_association_order() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    for group in ["zeta", "alpha"] {
        let (status, _) = request(
            &app,
            "PUT",
            &format!("/organizations/acme/policy_groups/{group}/policies/app"),
            REVISION,
            "pivotal",
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["policy_group_list"], json!(["zeta", "alpha"]));
}

#[tokio::test]
async fn deleting_a_policy_group_returns_its_associations() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/policy_groups/prod/policies/app",
        REVISION,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "DELETE",
        "/organizations/acme/policy_groups/prod",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "uri": "http://0.0.0.0:8000/organizations/acme/policy_groups/prod",
            "policies": { "app": { "revision_id": "rev1" } }
        })
    );
}
