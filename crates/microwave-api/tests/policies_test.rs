mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::ActorType;
use serde_json::Value;
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
    assert_eq!(
        body["uri"],
        "http://0.0.0.0:8000/organizations/acme/policies/app"
    );

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
    assert_eq!(body, serde_json::json!({}));

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
    assert_eq!(body, serde_json::json!({}));

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
    assert_eq!(body, serde_json::json!({}));

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

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/policies/app/revisions/rev1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

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
        .store
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
        .store
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
