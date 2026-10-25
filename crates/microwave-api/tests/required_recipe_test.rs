mod support;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use microwave_config::Config;
use microwave_crypto::SignVersion;
use std::io::Write;
use tower::ServiceExt;

fn enabled_config(path: &str) -> Config {
    let mut config = Config::default();
    config.required_recipe.enabled = true;
    config.required_recipe.path = Some(path.to_string());
    config
}

fn signed(method: &str, path: &str, user_id: &str) -> Request<Body> {
    support::signed_request(method, path, b"", SignVersion::V1_3, user_id)
}

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn disabled_required_recipe_returns_404_for_every_method_and_auth_state() {
    let app = support::app(support::state_with_pivotal());
    let path = "/organizations/acme/required_recipe";

    for request in [
        signed("GET", path, "pivotal"),
        signed("POST", path, "pivotal"),
        Request::get(path).body(Body::empty()).unwrap(),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn enabled_required_recipe_serves_the_recipe_as_plain_text() {
    let mut recipe = tempfile::NamedTempFile::new().unwrap();
    writeln!(recipe, "# required recipe").unwrap();
    writeln!(recipe, "file '/tmp/required'").unwrap();
    recipe.flush().unwrap();

    let state = support::state_with_config(enabled_config(recipe.path().to_str().unwrap()));
    support::add_org(&state, "acme").await;
    let app = support::app(state);
    let path = "/organizations/acme/required_recipe";

    let response = app
        .clone()
        .oneshot(signed("GET", path, "pivotal"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(
        body_text(response).await,
        "# required recipe\nfile '/tmp/required'\n"
    );
}

#[tokio::test]
async fn enabled_required_recipe_rejects_non_get_before_authentication() {
    let recipe = tempfile::NamedTempFile::new().unwrap();
    let state = support::state_with_config(enabled_config(recipe.path().to_str().unwrap()));
    support::add_org(&state, "acme").await;
    let app = support::app(state);
    let path = "/organizations/acme/required_recipe";

    for request in [
        signed("POST", path, "pivotal"),
        Request::post(path).body(Body::empty()).unwrap(),
    ] {
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers().get("allow").unwrap(), "GET");
    }
}

#[tokio::test]
async fn enabled_required_recipe_distinguishes_bad_requests_from_bad_clients() {
    let recipe = tempfile::NamedTempFile::new().unwrap();
    let state = support::state_with_config(enabled_config(recipe.path().to_str().unwrap()));
    support::add_org(&state, "acme").await;
    let app = support::app(state);
    let path = "/organizations/acme/required_recipe";

    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .oneshot(signed("GET", path, "unknown-client"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
