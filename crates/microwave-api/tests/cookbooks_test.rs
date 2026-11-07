mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::AppState;
use microwave_crypto::SignVersion;
use microwave_objectstore::{ObjectMetadata, ObjectStore, ObjectStoreError};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

/// Captures formatted log lines so a test can pin their fields.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Delegates every blob operation to an inner store but fails to presign.
struct FailingPresignStore(Arc<dyn ObjectStore + Send + Sync>);

impl ObjectStore for FailingPresignStore {
    fn put(
        &self,
        organization_id: &str,
        checksum: &str,
        data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        self.0.put(organization_id, checksum, data)
    }

    fn get(&self, organization_id: &str, checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        self.0.get(organization_id, checksum)
    }

    fn exists(&self, organization_id: &str, checksum: &str) -> bool {
        self.0.exists(organization_id, checksum)
    }

    fn delete(&self, organization_id: &str, checksum: &str) -> Result<bool, ObjectStoreError> {
        self.0.delete(organization_id, checksum)
    }

    fn metadata(
        &self,
        organization_id: &str,
        checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        self.0.metadata(organization_id, checksum)
    }

    fn presigned_url(
        &self,
        _method: &str,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<String, ObjectStoreError> {
        Err(ObjectStoreError::NotFound)
    }
}

async fn create_org(app: &axum::Router) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            br#"{"name":"acme","full_name":"Acme Corp"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn uploads_and_reads_a_cookbook_version() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let checksum = "5d41402abc4b2a76b9719d911017c592";
    let body = format!(r#"{{"checksums":{{"{checksum}":null}}}}"#);
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/sandboxes",
            body.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
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

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let manifest = format!(
        r#"{{"name":"apache-1.0.0","cookbook_name":"apache","version":"1.0.0","json_class":"Chef::CookbookVersion","chef_type":"cookbook_version","frozen?":false,"metadata":{{"version":"1.0.0"}},"all_files":[{{"name":"recipes/default.rb","path":"recipes/default.rb","checksum":"{checksum}","specificity":"default"}}]}}"#
    );
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            "/organizations/acme/cookbooks/apache/1.0.0",
            manifest.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/cookbooks",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(listed["apache"]["versions"][0]["version"], "1.0.0");

    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "GET",
            "/organizations/acme/cookbooks/apache/1.0.0",
            b"",
            SignVersion::V1_3,
            "pivotal",
            "2",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    let url = fetched["all_files"][0]["url"].as_str().unwrap();
    assert!(url.contains("/bookshelf/organizations/"));
    assert!(url.contains("/checksums/"));
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"hello");

    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "POST",
            "/organizations/acme/environments/_default/cookbook_versions",
            br#"{"run_list":["recipe[apache]"]}"#,
            SignVersion::V1_3,
            "pivotal",
            "1",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let solved = support::response_json(response).await;
    assert_eq!(solved["apache"]["cookbook_name"], "apache");

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/environments/_default/cookbook_versions",
            br#"{"run_list":["recipe[missing]"]}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PRECONDITION_FAILED);
}

/// Upload `contents` through a sandbox and return its checksum.
async fn upload_blob(app: &axum::Router, checksum: &str, contents: &str) {
    let body = format!(r#"{{"checksums":{{"{checksum}":null}}}}"#);
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/sandboxes",
            body.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .body(Body::from(contents.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn put_manifest(
    app: &axum::Router,
    name: &str,
    version: &str,
    api_version: &str,
    manifest: &Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "PUT",
            &format!("/organizations/acme/cookbooks/{name}/{version}"),
            &serde_json::to_vec(manifest).unwrap(),
            SignVersion::V1_3,
            "pivotal",
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

async fn get_manifest(app: &axum::Router, name: &str, version: &str, api_version: &str) -> Value {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "GET",
            &format!("/organizations/acme/cookbooks/{name}/{version}"),
            b"",
            SignVersion::V1_3,
            "pivotal",
            api_version,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    support::response_json(response).await
}

#[tokio::test]
async fn returns_v0_segments_and_v2_all_files_for_the_same_cookbook() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;
    let checksum = "5d41402abc4b2a76b9719d911017c592";
    upload_blob(&app, checksum, "hello").await;

    let manifest = json!({
        "name": "apache-1.0.0",
        "cookbook_name": "apache",
        "version": "1.0.0",
        "metadata": { "name": "apache", "version": "1.0.0" },
        "recipes": [{
            "name": "default.rb",
            "path": "recipes/default.rb",
            "checksum": checksum,
            "specificity": "default",
        }],
        "root_files": [{
            "name": "metadata.rb",
            "path": "metadata.rb",
            "checksum": checksum,
            "specificity": "default",
        }],
    });
    let (status, _) = put_manifest(&app, "apache", "1.0.0", "1", &manifest).await;
    assert_eq!(status, StatusCode::CREATED);

    for api_version in ["0", "1"] {
        let v0 = get_manifest(&app, "apache", "1.0.0", api_version).await;
        assert!(v0.get("all_files").is_none(), "api v{api_version}");
        assert_eq!(v0["recipes"][0]["name"], "default.rb");
        assert_eq!(v0["root_files"][0]["name"], "metadata.rb");
        assert!(v0["recipes"][0]["url"]
            .as_str()
            .unwrap()
            .contains("/bookshelf/organizations/"));
    }

    let v2 = get_manifest(&app, "apache", "1.0.0", "2").await;
    assert!(v2.get("recipes").is_none());
    assert!(v2.get("root_files").is_none());
    assert_eq!(v2["cookbook_name"], "apache");
    assert_eq!(v2["name"], "apache-1.0.0");
    assert_eq!(v2["version"], "1.0.0");
    assert_eq!(v2["metadata"]["version"], "1.0.0");
    assert_eq!(v2["chef_type"], "cookbook_version");
    assert_eq!(v2["json_class"], "Chef::CookbookVersion");

    let files = v2["all_files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let recipe = files
        .iter()
        .find(|file| file["path"] == "recipes/default.rb")
        .unwrap();
    assert_eq!(recipe["name"], "recipes/default.rb");
    assert_eq!(recipe["checksum"], checksum);
    assert_eq!(recipe["specificity"], "default");
    assert!(recipe["url"]
        .as_str()
        .unwrap()
        .contains("/bookshelf/organizations/"));
    assert!(recipe["url"].as_str().unwrap().contains("/checksums/"));
    let root = files
        .iter()
        .find(|file| file["path"] == "metadata.rb")
        .unwrap();
    assert_eq!(root["name"], "root_files/metadata.rb");
    assert!(root["url"].as_str().unwrap().contains("/checksums/"));
}

#[tokio::test]
async fn converts_a_v2_upload_back_to_v0_segments() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;
    let checksum = "5d41402abc4b2a76b9719d911017c592";
    upload_blob(&app, checksum, "hello").await;

    let manifest = json!({
        "name": "apache-1.0.0",
        "cookbook_name": "apache",
        "version": "1.0.0",
        "metadata": { "name": "apache", "version": "1.0.0" },
        "all_files": [
            {
                "name": "recipes/default.rb",
                "path": "recipes/default.rb",
                "checksum": checksum,
                "specificity": "default",
            },
            {
                "name": "templates/default/motd.erb",
                "path": "templates/default/motd.erb",
                "checksum": checksum,
                "specificity": "default",
            },
            {
                "name": "root_files/metadata.rb",
                "path": "metadata.rb",
                "checksum": checksum,
                "specificity": "default",
            },
        ],
    });
    let (status, _) = put_manifest(&app, "apache", "1.0.0", "2", &manifest).await;
    assert_eq!(status, StatusCode::CREATED);

    let v0 = get_manifest(&app, "apache", "1.0.0", "1").await;
    assert!(v0.get("all_files").is_none());
    assert_eq!(v0["recipes"][0]["name"], "default.rb");
    assert_eq!(v0["templates"][0]["name"], "motd.erb");
    assert_eq!(v0["templates"][0]["path"], "templates/default/motd.erb");
    assert_eq!(v0["root_files"][0]["name"], "metadata.rb");
    for segment in [
        "attributes",
        "definitions",
        "files",
        "libraries",
        "providers",
        "resources",
    ] {
        assert_eq!(v0[segment], json!([]), "segment {segment}");
    }
    assert!(v0["recipes"][0]["url"]
        .as_str()
        .unwrap()
        .contains("/checksums/"));

    let v2 = get_manifest(&app, "apache", "1.0.0", "2").await;
    assert_eq!(v2["all_files"].as_array().unwrap().len(), 3);
    assert_eq!(v2["cookbook_name"], "apache");
}

#[tokio::test]
async fn v2_upload_rejects_a_missing_all_files_checksum() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let manifest = json!({
        "name": "apache-1.0.0",
        "cookbook_name": "apache",
        "version": "1.0.0",
        "metadata": { "name": "apache", "version": "1.0.0" },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": "00000000000000000000000000000000",
            "specificity": "default",
        }],
    });
    let (status, body) = put_manifest(&app, "apache", "1.0.0", "2", &manifest).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({
            "error": ["Manifest has a checksum that hasn't been uploaded."]
        })
    );
}

#[tokio::test]
async fn a_presign_failure_answers_500() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let checksum = "5d41402abc4b2a76b9719d911017c592";
    upload_blob(&app, checksum, "hello").await;

    let manifest = json!({
        "name": "apache-1.0.0",
        "cookbook_name": "apache",
        "version": "1.0.0",
        "metadata": { "name": "apache", "version": "1.0.0" },
        "recipes": [{
            "name": "default.rb",
            "path": "recipes/default.rb",
            "checksum": checksum,
            "specificity": "default",
        }],
    });
    let (status, _) = put_manifest(&app, "apache", "1.0.0", "1", &manifest).await;
    assert_eq!(status, StatusCode::CREATED);

    let app = support::app(
        AppState::new(
            Arc::clone(&state.store),
            state.config.clone(),
            Arc::new(FailingPresignStore(Arc::clone(&state.object_store))),
        )
        .unwrap(),
    );

    let capture = LogCapture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .ok();

    let response = app
        .oneshot(support::signed_request_with_api_version(
            "GET",
            "/organizations/acme/cookbooks/apache/1.0.0",
            b"",
            SignVersion::V1_3,
            "pivotal",
            "2",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    let org = state.store.find_org("acme").await.unwrap().unwrap();
    let line = logs
        .lines()
        .find(|line| line.contains("presigned download URL failed"))
        .unwrap_or_else(|| panic!("presign failure log line missing from {logs:?}"));
    assert!(
        line.contains(&format!("org_id={}", org.id)),
        "log line: {line}"
    );
    assert!(
        line.contains("operation=\"presigned_url\""),
        "log line: {line}"
    );
    assert!(!line.contains(checksum), "log line: {line}");
}
