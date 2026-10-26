//! P1a routing contract: default-org compatibility rewrites and the single
//! trailing-slash tolerance upstream nginx provides.

mod support;

use axum::http::StatusCode;
use axum::response::Response;
use axum::Router;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use tower::ServiceExt;

fn default_org_config() -> Config {
    let mut config = Config::default();
    config.server.default_orgname = Some("acme".into());
    config
}

async fn create_org(app: &Router) {
    let response = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn request(app: &Router, method: &str, path: &str, body: &[u8]) -> Response {
    app.clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap()
}

async fn request_signed_for(
    app: &Router,
    method: &str,
    path: &str,
    signed_path: &str,
    body: &[u8],
) -> Response {
    app.clone()
        .oneshot(support::signed_request_with_signed_path(
            method,
            path,
            signed_path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap()
}

#[tokio::test]
async fn rewrites_top_level_endpoints_to_the_default_org() {
    let app = support::app(support::state_with_config(default_org_config()));
    create_org(&app).await;

    let response = request(&app, "POST", "/nodes", br#"{"name":"web1"}"#).await;
    let status = response.status();
    let created = support::response_json(response).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );

    let response = request(&app, "GET", "/nodes/web1", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["name"], "web1");

    // Fully-qualified organization paths are not rewritten.
    let response = request(&app, "GET", "/organizations/acme/nodes/web1", b"").await;
    assert_eq!(response.status(), StatusCode::OK);

    // Unknown top-level paths stay unknown.
    let response = request(&app, "GET", "/not-a-chef-endpoint", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Compare every compatibility endpoint with its fully-qualified spelling.
    // The short path would be a 404 without the rewrite.
    for (short, long) in [
        ("/search", "/organizations/acme/search"),
        ("/nodes", "/organizations/acme/nodes"),
        ("/cookbooks", "/organizations/acme/cookbooks"),
        ("/data", "/organizations/acme/data"),
        ("/roles", "/organizations/acme/roles"),
        ("/sandboxes", "/organizations/acme/sandboxes"),
        ("/environments", "/organizations/acme/environments"),
        ("/clients", "/organizations/acme/clients"),
        (
            "/principals/pivotal",
            "/organizations/acme/principals/pivotal",
        ),
        ("/groups", "/organizations/acme/groups"),
        ("/containers", "/organizations/acme/containers"),
    ] {
        let short_status = request(&app, "GET", short, b"").await.status();
        let long_status = request(&app, "GET", long, b"").await.status();
        assert_eq!(short_status, long_status, "{short} must route like {long}");
        assert_ne!(
            long_status,
            StatusCode::NOT_FOUND,
            "{long} must exist for the comparison to prove the rewrite"
        );
    }
}

#[tokio::test]
async fn verifies_signatures_against_the_original_short_path() {
    let app = support::app(support::state_with_config(default_org_config()));
    create_org(&app).await;

    // The valid signature covers `/nodes`, not the rewritten organization
    // path. If verification used the rewritten URI this would be a 401.
    let response = request(&app, "POST", "/nodes", br#"{"name":"web1"}"#).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let response = request(&app, "POST", "/nodes", br#"{"name":"web2"}"#).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // A signature for the rewritten path must not authorize the short path.
    let response = request_signed_for(
        &app,
        "POST",
        "/nodes",
        "/organizations/acme/nodes",
        br#"{"name":"web3"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A trailing slash is canonicalized for signature verification.
    let response = request(&app, "GET", "/organizations/acme/nodes/web1/", b"").await;
    assert_eq!(response.status(), StatusCode::OK);

    // Query strings survive the default-org rewrite: `name:web1` must match
    // exactly one of the two nodes rather than defaulting to all nodes.
    let response = request(&app, "GET", "/search/node/?q=name:web1", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let hits = support::response_json(response).await;
    assert_eq!(hits["total"], 1);
}

#[tokio::test]
async fn accepts_exactly_one_trailing_slash_on_routes() {
    let app = support::app(support::state_with_config(default_org_config()));
    create_org(&app).await;
    let response = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    for path in [
        "/organizations/acme/search/",
        "/organizations/acme/nodes/",
        "/organizations/acme/nodes/web1/",
        "/organizations/acme/cookbooks/",
        "/organizations/acme/data/",
        "/organizations/acme/roles/",
        "/organizations/acme/sandboxes/",
        "/organizations/acme/environments/",
        "/organizations/acme/clients/",
        "/organizations/acme/principals/pivotal/",
        "/organizations/acme/groups/",
        "/organizations/acme/containers/",
    ] {
        let status = request(&app, "GET", path, b"").await.status();
        assert_ne!(status, StatusCode::NOT_FOUND, "{path} should be routed");
    }

    // The query string is preserved when the trailing slash is removed.
    let response = request(&app, "GET", "/organizations/acme/nodes/web1/?foo=bar", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["name"], "web1");

    // More than one trailing slash is not normalized away.
    let response = request(&app, "GET", "/organizations/acme/nodes/web1//", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn does_not_rewrite_short_paths_when_default_org_is_disabled() {
    let config = Config::default();
    assert_eq!(config.server.default_orgname, None);
    let app = support::app(support::state_with_config(config));
    create_org(&app).await;

    for path in ["/search", "/nodes", "/clients"] {
        let response = request(&app, "GET", path, b"").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    // Organization-scoped paths are unaffected by the disabled setting.
    let response = request(&app, "GET", "/organizations/acme/nodes", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn default_orgname_must_be_a_valid_org_name() {
    let mut config = Config::default();
    assert_eq!(config.server.default_orgname, None);
    config.validate().unwrap();

    config.server.default_orgname = Some("acme-prod_1".into());
    config.validate().unwrap();

    for invalid in ["", "   ", "Acme", "acme/prod", "-acme", "acme prod"] {
        config.server.default_orgname = Some(invalid.into());
        assert!(
            config.validate().is_err(),
            "{invalid:?} must be rejected as a default organization name"
        );
    }

    let parsed = Config::from_toml_str("[server]\ndefault_orgname = \"acme\"\n").unwrap();
    assert_eq!(parsed.server.default_orgname.as_deref(), Some("acme"));
}
