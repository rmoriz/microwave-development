mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_config::{Config, OrgQuotas};
use microwave_crypto::SignVersion;
use microwave_store::{Org, QuotaRepository};
use serde_json::{json, Value};
use tower::ServiceExt;

fn config() -> Config {
    let mut config = Config::default();
    config.limits.source.rate_per_second = 1_000;
    config.limits.source.burst = 1_000;
    config
}

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

async fn org_with(app: &axum::Router, state: &microwave_api::AppState, name: &str) -> Org {
    match name {
        "acme" => {
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
        other => {
            support::add_org(state, other).await;
        }
    }
    state.sqlite().find_org(name).unwrap().unwrap()
}

const NODE: &[u8] = br#"{"name":"web1","run_list":["recipe[apache]"]}"#;

fn node(name: &str) -> Vec<u8> {
    format!(r#"{{"name":"{name}","run_list":["recipe[apache]"]}}"#).into_bytes()
}

#[tokio::test]
async fn node_creation_over_the_ceiling_is_forbidden() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_nodes: Some(1),
            ..Default::default()
        }),
    );

    let (status, _) = request(&app, "POST", "/organizations/acme/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        &node("web2"),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": ["node limit of 1 reached"]}));

    // An update of the existing row is allowed at the ceiling.
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1",
        NODE,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/nodes/web1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        &node("web2"),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// md5("hello"), the body `commit_checksum` uploads.
const CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";

fn cookbook_manifest(name: &str, version: &str, checksum: &str) -> String {
    format!(
        r#"{{"name":"{name}-{version}","cookbook_name":"{name}","version":"{version}","json_class":"Chef::CookbookVersion","chef_type":"cookbook_version","frozen?":false,"metadata":{{"version":"{version}"}},"all_files":[{{"name":"recipes/default.rb","path":"recipes/default.rb","checksum":"{checksum}","specificity":"default"}}]}}"#
    )
}

async fn commit_checksum(app: &axum::Router, checksum: &str) {
    let (status, created) = request(
        app,
        "POST",
        "/organizations/acme/sandboxes",
        format!(r#"{{"checksums":{{"{checksum}":null}}}}"#).as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    if let Some(url) = created["checksums"][checksum]["url"].as_str() {
        let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(path)
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let (status, _) = request(
        app,
        "PUT",
        &format!("/organizations/acme/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn upload_cookbook(app: &axum::Router, name: &str, version: &str) -> (StatusCode, Value) {
    commit_checksum(app, CHECKSUM).await;
    request(
        app,
        "PUT",
        &format!("/organizations/acme/cookbooks/{name}/{version}"),
        cookbook_manifest(name, version, CHECKSUM).as_bytes(),
        "pivotal",
    )
    .await
}

#[tokio::test]
async fn cookbook_version_over_the_ceiling_is_forbidden() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_cookbook_versions: Some(1),
            ..Default::default()
        }),
    );

    let (status, _) = upload_cookbook(&app, "apache", "1.0.0").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = upload_cookbook(&app, "apache", "2.0.0").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"error": ["cookbook version limit of 1 reached"]})
    );

    // A re-upload of the existing version stays an update.
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/apache/1.0.0",
        cookbook_manifest("apache", "1.0.0", CHECKSUM).as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn environment_over_the_ceiling_is_forbidden() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_environments: Some(1),
            ..Default::default()
        }),
    );

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"first"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"second"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": ["environment limit of 1 reached"]}));

    // `_default` exists from org creation and never counts: at a ceiling of
    // one the first custom environment above was still the whole allowance.
    assert!(state
        .sqlite()
        .find_environment(&org.id, "_default")
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn role_over_the_ceiling_is_forbidden() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_roles: Some(1),
            ..Default::default()
        }),
    );

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"first"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"second"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": ["role limit of 1 reached"]}));
}

const REVISION: &[u8] =
    br#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;
const REVISION_TWO: &[u8] =
    br#"{"name":"app","revision_id":"rev2","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;

#[tokio::test]
async fn policy_revision_over_the_ceiling_is_forbidden() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_policy_revisions: Some(1),
            ..Default::default()
        }),
    );

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
        "POST",
        "/organizations/acme/policies/app/revisions",
        REVISION_TWO,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"error": ["policy revision limit of 1 reached"]})
    );

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/policy_groups",
        br#"{"name":"prod"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The existing revision is not a new row; rev2 is.
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
        "PUT",
        "/organizations/acme/policy_groups/prod/policies/app",
        REVISION_TWO,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({"error": ["policy revision limit of 1 reached"]})
    );

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
        "PUT",
        "/organizations/acme/policy_groups/prod/policies/app",
        REVISION_TWO,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn no_cross_organization_bleed() {
    let state = support::state_with_config(config());
    let app = support::app(state.clone());
    let acme = org_with(&app, &state, "acme").await;
    org_with(&app, &state, "beta").await;
    state.sqlite().set_org_quotas(
        &acme.id,
        Some(OrgQuotas {
            max_nodes: Some(1),
            ..Default::default()
        }),
    );

    let (status, _) = request(&app, "POST", "/organizations/acme/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        &node("web2"),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(&app, "POST", "/organizations/beta/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn the_rejection_is_counted() {
    let state = support::state_with_config(config());
    let metrics = state.metrics.clone();
    state.sqlite().set_metrics(metrics.clone());
    let app = support::app(state.clone());
    let org = org_with(&app, &state, "acme").await;
    state.sqlite().set_org_quotas(
        &org.id,
        Some(OrgQuotas {
            max_nodes: Some(0),
            ..Default::default()
        }),
    );

    let (status, _) = request(&app, "POST", "/organizations/acme/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let series = metrics.limit_series();
    assert!(series.iter().any(|series| {
        series.scope == "quota:node" && series.outcome == "rejected" && series.org == org.id
    }));
}

#[tokio::test]
async fn org_create_reads_the_live_quota_snapshot() {
    let state = support::state_with_config(config());
    let mut quotas = Config::default().quotas;
    quotas.organizations.insert(
        "acme".into(),
        OrgQuotas {
            max_nodes: Some(0),
            ..Default::default()
        },
    );
    state.quotas.store(std::sync::Arc::new(quotas));
    let app = support::app(state);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(&app, "POST", "/organizations/acme/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": ["node limit of 0 reached"]}));
}

#[tokio::test]
async fn org_create_installs_the_entry_immediately() {
    let mut config = config();
    config.quotas.organizations.insert(
        "acme".into(),
        OrgQuotas {
            max_nodes: Some(0),
            max_environments: Some(0),
            ..Default::default()
        },
    );
    let state = support::state_with_config(config);
    let app = support::app(state);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(&app, "POST", "/organizations/acme/nodes", NODE, "pivotal").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, json!({"error": ["node limit of 0 reached"]}));
}
