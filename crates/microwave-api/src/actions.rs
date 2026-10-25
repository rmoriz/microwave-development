//! Server-side `oc_chef_action` mutation notifications for the configured
//! data collector.
//!
//! Upstream sends a JSON action message for successful node, role,
//! environment, data bag, client, key, policy, ACL, membership, cookbook, and
//! organization writes. The message shape follows
//! `chef-server/src/oc_erchef/apps/oc_chef_wm/src/oc_chef_action.erl`:
//!
//! ```json
//! {
//!   "message_type": "action",
//!   "message_version": "0.1.1",
//!   "organization_name": "acme",
//!   "service_hostname": "chef.example.com",
//!   "recorded_at": "2026-10-25T12:00:00Z",
//!   "remote_hostname": "10.0.0.5",
//!   "request_id": "...",
//!   "requestor_name": "pivotal",
//!   "requestor_type": "user",
//!   "user_agent": "knife/18.0",
//!   "id": "<uuid>",
//!   "task": "create",
//!   "entity_type": "node",
//!   "entity_name": "web1",
//!   "data": { ... }
//! }
//! ```
//!
//! Dispatch is deliberately off the request path: successful mutations build a
//! small metadata record, drop it on a bounded channel, and return. A single
//! worker per application state performs the outbound request with the
//! configured timeout. When the queue is full the notification is dropped and
//! logged; the API response is never blocked or turned into an error by the
//! collector. Request bodies above 256 KiB are reported without `data` so the
//! bounded queue also has a bounded memory footprint.

use crate::AppState;
use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode};
use microwave_store::{ActorType, Org, Principal};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

const MESSAGE_VERSION: &str = "0.1.1";
/// Bounded queue. A burst of mutations may overflow this; excess messages are
/// dropped rather than growing memory or spawning tasks.
const QUEUE_CAPACITY: usize = 256;
/// Request payloads larger than this are reported without their `data`
/// member. Together with the bounded queue this keeps worst-case memory use
/// predictable without dropping the mutation event itself.
const MAX_ACTION_DATA_BYTES: usize = 256 * 1024;
const REDACTED_PASSWORD: &str = "*******";

#[derive(Debug, Clone)]
struct ActionEnvelope {
    payload: Value,
    token: Option<String>,
}

/// Owns the optional background worker and its bounded queue.
pub(crate) struct ActionDispatcher {
    root_url: Option<String>,
    timeout: Duration,
    service_hostname: Option<String>,
    client: reqwest::Client,
    sender: Mutex<Option<mpsc::Sender<ActionEnvelope>>>,
    dropped: AtomicU64,
}

impl ActionDispatcher {
    pub(crate) fn new(config: &microwave_config::Config, client: reqwest::Client) -> Arc<Self> {
        let root_url = config
            .data_collector
            .root_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty() && config.data_collector.actions_enabled())
            .map(ToString::to_string);
        Arc::new(Self {
            root_url,
            timeout: Duration::from_millis(config.data_collector.timeout.max(1)),
            service_hostname: config
                .data_collector
                .service_hostname
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(ToString::to_string),
            client,
            sender: Mutex::new(None),
            dropped: AtomicU64::new(0),
        })
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.root_url.is_some()
    }

    /// Queue a notification. Returns without waiting for the collector. The
    /// only work on the request path is payload construction, the optional
    /// secret lookup, and a non-blocking channel send.
    pub(crate) async fn notify(&self, state: &AppState, meta: RequestMeta, status: StatusCode) {
        if !self.is_enabled() || !status.is_success() {
            return;
        }
        let Some(payload) = build_payload(&meta, status, self.service_hostname.as_deref()) else {
            return;
        };
        let token = crate::data_collector::configured_token(state).await;

        let mut guard = self.sender.lock().await;
        if guard.is_none() {
            let Some(root_url) = self.root_url.clone() else {
                return;
            };
            let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
            tokio::spawn(run_worker(
                receiver,
                self.client.clone(),
                root_url,
                self.timeout,
            ));
            *guard = Some(sender);
        }
        let sender = guard.as_ref().expect("sender initialized above");
        match sender.try_send(ActionEnvelope { payload, token }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    dropped,
                    capacity = QUEUE_CAPACITY,
                    "data collector action queue is full; dropping notification"
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // The worker stopped unexpectedly. Drop this notification and
                // let the next mutation start a fresh worker.
                *guard = None;
                tracing::warn!("data collector action worker stopped; queue reset");
            }
        }
    }
}

async fn run_worker(
    mut receiver: mpsc::Receiver<ActionEnvelope>,
    client: reqwest::Client,
    root_url: String,
    timeout: Duration,
) {
    while let Some(envelope) = receiver.recv().await {
        let mut request = client
            .post(&root_url)
            .timeout(timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(token) = envelope.token.as_deref() {
            request = request.header("x-data-collector-token", token);
        }
        match request.json(&envelope.payload).send().await {
            Ok(response) if response.status().is_success() => {}
            Ok(response) => {
                tracing::warn!(
                    url = root_url,
                    status = response.status().as_u16(),
                    "data collector rejected action notification"
                );
            }
            Err(error) if error.is_timeout() => {
                tracing::warn!(
                    url = root_url,
                    "data collector action notification timed out"
                );
            }
            Err(error) => {
                tracing::warn!(%error, url = root_url, "data collector action notification failed");
            }
        }
    }
}

/// Request information captured before the handler consumes the body.
pub(crate) struct RequestMeta {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
    requestor_name: String,
    requestor_type: &'static str,
    organization_name: Option<String>,
}

impl RequestMeta {
    /// Capture metadata only for methods that can mutate. GET/HEAD requests
    /// never allocate the extra body handle.
    pub(crate) fn capture(
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        body: Bytes,
        principal: &Principal,
        org: Option<&Org>,
    ) -> Option<Self> {
        if !matches!(method, &Method::POST | &Method::PUT | &Method::DELETE) {
            return None;
        }
        Some(Self {
            method: method.clone(),
            path: path.to_string(),
            headers: headers.clone(),
            body,
            requestor_name: principal.name.clone(),
            requestor_type: match principal.actor_type {
                ActorType::Client => "client",
                ActorType::User => "user",
            },
            organization_name: org.map(|org| org.name.clone()),
        })
    }
}

struct Entity {
    entity_type: &'static str,
    entity_name: String,
    parent_type: Option<&'static str>,
    parent_name: Option<String>,
}

impl Entity {
    fn simple(entity_type: &'static str, entity_name: impl Into<String>) -> Self {
        Self {
            entity_type,
            entity_name: entity_name.into(),
            parent_type: None,
            parent_name: None,
        }
    }

    fn child(
        entity_type: &'static str,
        entity_name: impl Into<String>,
        parent_type: &'static str,
        parent_name: impl Into<String>,
    ) -> Self {
        Self {
            entity_type,
            entity_name: entity_name.into(),
            parent_type: Some(parent_type),
            parent_name: Some(parent_name.into()),
        }
    }
}

fn build_payload(
    meta: &RequestMeta,
    status: StatusCode,
    configured_hostname: Option<&str>,
) -> Option<Value> {
    let data = request_data(&meta.body);
    let entity = entity_for(meta, data.as_ref())?;
    let task = task_for(meta, status, data.as_ref());

    let mut payload = Map::new();
    payload.insert("message_type".into(), json!("action"));
    payload.insert("message_version".into(), json!(MESSAGE_VERSION));
    payload.insert(
        "organization_name".into(),
        meta.organization_name
            .as_ref()
            .map(|name| json!(name))
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "service_hostname".into(),
        json!(configured_hostname
            .map(ToString::to_string)
            .or_else(|| header(&meta.headers, "host").map(strip_port))
            .unwrap_or_else(|| "localhost".to_string())),
    );
    payload.insert(
        "recorded_at".into(),
        json!(header(&meta.headers, "x-ops-timestamp").unwrap_or_else(
            || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        )),
    );
    payload.insert(
        "remote_hostname".into(),
        header(&meta.headers, "x-forwarded-for")
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "request_id".into(),
        json!(header(&meta.headers, "x-request-id")
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())),
    );
    payload.insert("requestor_name".into(), json!(meta.requestor_name));
    payload.insert("requestor_type".into(), json!(meta.requestor_type));
    payload.insert(
        "user_agent".into(),
        header(&meta.headers, "user-agent")
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    payload.insert("id".into(), json!(uuid::Uuid::new_v4().to_string()));
    payload.insert("task".into(), json!(task));
    payload.insert("entity_type".into(), json!(entity.entity_type));
    payload.insert("entity_name".into(), json!(entity.entity_name));
    if let Some(parent_type) = entity.parent_type {
        payload.insert("parent_type".into(), json!(parent_type));
    }
    if let Some(parent_name) = entity.parent_name {
        payload.insert("parent_name".into(), json!(parent_name));
    }
    if let Some(remote_request_id) = header(&meta.headers, "x-remote-request-id") {
        payload.insert("remote_request_id".into(), json!(remote_request_id));
    }
    if let Some(data) = data.filter(|_| meta.body.len() <= MAX_ACTION_DATA_BYTES) {
        payload.insert("data".into(), scrub_data(data));
    }

    Some(Value::Object(payload))
}

fn request_data(body: &[u8]) -> Option<Value> {
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice(body).ok()
}

fn scrub_data(mut data: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        // Upstream redacts top-level password values. Private keys are also
        // credentials and must not be copied to an external collector.
        for key in ["password", "private_key"] {
            if object.contains_key(key) {
                object.insert(key.into(), json!(REDACTED_PASSWORD));
            }
        }
    }
    data
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string)
}

fn strip_port(host: String) -> String {
    if let Some(rest) = host.strip_prefix('[') {
        // IPv6 literal, optionally followed by :port.
        return rest
            .split_once(']')
            .map(|(address, _)| format!("[{address}]"))
            .unwrap_or(host);
    }
    host.split_once(':')
        .map(|(name, _)| name.to_string())
        .unwrap_or(host)
}

fn task_for(meta: &RequestMeta, status: StatusCode, data: Option<&Value>) -> String {
    let parts = path_parts(&meta.path);
    // Association endpoints use upstream's verbs rather than create/delete.
    if parts.first() == Some(&"organizations") {
        match (meta.method.as_str(), parts.get(2).copied(), parts.len()) {
            ("POST", Some("users"), 3) | ("PUT", Some("users"), 4) => {
                return "associate".to_string();
            }
            ("DELETE", Some("users"), 4) => return "dissociate".to_string(),
            ("POST", Some("association_requests"), 3) => return "invite".to_string(),
            ("DELETE", Some("association_requests"), 4) => return "reject".to_string(),
            _ => {}
        }
    }
    if parts.first() == Some(&"users")
        && parts.get(2) == Some(&"association_requests")
        && meta.method == Method::PUT
    {
        return match data
            .and_then(|data| data.get("response"))
            .and_then(Value::as_str)
        {
            Some("accept") => "associate".to_string(),
            Some("reject") => "reject".to_string(),
            _ => "update".to_string(),
        };
    }

    match meta.method {
        Method::POST => "create".to_string(),
        Method::PUT if status == StatusCode::CREATED => "create".to_string(),
        Method::PUT => "update".to_string(),
        Method::DELETE => "delete".to_string(),
        _ => "update".to_string(),
    }
}

fn entity_for(meta: &RequestMeta, data: Option<&Value>) -> Option<Entity> {
    let parts = path_parts(&meta.path);
    match parts.first().copied()? {
        "organizations" => organization_entity(meta, data, &parts),
        "users" => user_entity(data, &parts),
        _ => None,
    }
}

fn organization_entity(meta: &RequestMeta, data: Option<&Value>, parts: &[&str]) -> Option<Entity> {
    if parts.len() == 1 {
        return body_name(data, &["name"]).map(|name| Entity::simple("organization", name));
    }

    let org = *parts.get(1)?;
    if parts.len() == 2 {
        return Some(Entity::simple("organization", org));
    }

    // ACL updates are reported as permission entities, matching
    // `acl_state` in `oc_chef_action.erl`.
    if let Some(position) = parts.iter().position(|part| *part == "_acl") {
        let permission = parts.get(position + 1)?;
        let parent = if parts.get(2) == Some(&"organizations") {
            ("organization", org.to_string())
        } else {
            let object_type = parts.get(2)?;
            let object_name = parts.get(3)?;
            (parent_type(object_type), (*object_name).to_string())
        };
        let mut entity = Entity::simple("permission", *permission);
        entity.parent_type = Some(parent.0);
        entity.parent_name = Some(parent.1);
        return Some(entity);
    }

    let resource = *parts.get(2)?;
    let name = parts.get(3).copied();
    match resource {
        "nodes" => named(data, name, &["name"]).map(|name| Entity::simple("node", name)),
        "roles" => named(data, name, &["name"]).map(|name| Entity::simple("role", name)),
        "environments" => {
            named(data, name, &["name"]).map(|name| Entity::simple("environment", name))
        }
        "clients" => {
            if parts.get(4) == Some(&"keys") {
                let parent = name?;
                let key = parts
                    .get(5)
                    .map(|key| key.to_string())
                    .or_else(|| body_name(data, &["name"]))
                    .or_else(|| (meta.method == Method::POST).then_some("default".to_string()))?;
                Some(Entity::child("key", key, "client", parent))
            } else {
                named(data, name, &["name"]).map(|name| Entity::simple("client", name))
            }
        }
        "users" => {
            named(data, name, &["username", "name"]).map(|name| Entity::simple("user", name))
        }
        "groups" => {
            named(data, name, &["groupname", "name"]).map(|name| Entity::simple("group", name))
        }
        "data" => data_bag_entity(meta, data, parts),
        "cookbooks" => {
            if parts.len() >= 5 {
                Some(Entity::child("version", parts[4], "cookbook", parts[3]))
            } else {
                None
            }
        }
        "cookbook_artifacts" => {
            if parts.len() >= 5 {
                Some(Entity::child(
                    "cookbook_artifact_version",
                    parts[4],
                    "cookbook_artifact",
                    parts[3],
                ))
            } else {
                None
            }
        }
        "policies" => {
            let policy = name
                .map(ToString::to_string)
                .or_else(|| body_name(data, &["name"]))?;
            Some(Entity::simple("policy", policy))
        }
        "policy_groups" => {
            if parts.get(4) == Some(&"policies") {
                let group = *parts.get(3)?;
                let policy = *parts.get(5)?;
                Some(Entity::child("policy", policy, "policy_group", group))
            } else {
                named(data, name, &["name"]).map(|name| Entity::simple("policy_group", name))
            }
        }
        "association_requests" => {
            let username = if let Some(path_name) = name {
                path_name.to_string()
            } else {
                body_name(data, &["username", "user", "name"])?
            };
            Some(Entity::simple("user", username))
        }
        // Sandboxes, search, principal reads, data-collector forwarding and
        // surface endpoints are not reported by upstream.
        _ => None,
    }
}

fn data_bag_entity(meta: &RequestMeta, data: Option<&Value>, parts: &[&str]) -> Option<Entity> {
    match parts.len() {
        // POST /organizations/:org/data creates a bag.
        3 => body_name(data, &["name"]).map(|name| Entity::simple("bag", name)),
        // POST /organizations/:org/data/:bag creates an item; other methods
        // address the bag itself.
        4 => {
            let bag = *parts.get(3)?;
            if meta.method == Method::POST {
                let item = body_name(data, &["id", "name"])?;
                Some(Entity::child("item", item, "bag", bag))
            } else {
                Some(Entity::simple("bag", bag))
            }
        }
        // /organizations/:org/data/:bag/:item
        5 => Some(Entity::child("item", *parts.get(4)?, "bag", *parts.get(3)?)),
        _ => None,
    }
}

fn user_entity(data: Option<&Value>, parts: &[&str]) -> Option<Entity> {
    // /users/:username/_acl/:permission
    if let Some(position) = parts.iter().position(|part| *part == "_acl") {
        let permission = parts.get(position + 1)?;
        let username = *parts.get(1)?;
        let mut entity = Entity::simple("permission", *permission);
        entity.parent_type = Some("user");
        entity.parent_name = Some(username.to_string());
        return Some(entity);
    }
    // /users/:username/keys[/:key]
    if parts.get(2) == Some(&"keys") {
        let username = *parts.get(1)?;
        let key = parts
            .get(3)
            .map(|key| key.to_string())
            .or_else(|| body_name(data, &["name"]))
            .or_else(|| (parts.len() == 3).then_some("default".to_string()))?;
        return Some(Entity::child("key", key, "user", username));
    }
    if parts.len() == 1 {
        return body_name(data, &["username", "name"]).map(|name| Entity::simple("user", name));
    }
    Some(Entity::simple("user", *parts.get(1)?))
}

fn named(data: Option<&Value>, path_name: Option<&str>, keys: &[&str]) -> Option<String> {
    path_name
        .map(ToString::to_string)
        .or_else(|| body_name(data, keys))
}

fn body_name(data: Option<&Value>, keys: &[&str]) -> Option<String> {
    let object = data?.as_object()?;
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn path_parts(path: &str) -> Vec<&str> {
    path.trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect()
}

fn parent_type(resource: &str) -> &'static str {
    match resource {
        "nodes" => "node",
        "roles" => "role",
        "environments" => "environment",
        "clients" => "client",
        "groups" => "group",
        "data" => "bag",
        "cookbooks" => "cookbook",
        "cookbook_artifacts" => "cookbook_artifact",
        "policies" => "policy",
        "policy_groups" => "policy_group",
        "containers" => "container",
        _ => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> Principal {
        Principal {
            actor_id: "1".into(),
            authz_id: "2".into(),
            name: "pivotal".into(),
            actor_type: ActorType::User,
            org_id: None,
        }
    }

    fn meta(method: Method, path: &str, body: &'static [u8]) -> RequestMeta {
        RequestMeta {
            method,
            path: path.into(),
            headers: HeaderMap::new(),
            body: Bytes::from_static(body),
            requestor_name: principal().name,
            requestor_type: "user",
            organization_name: Some("acme".into()),
        }
    }

    #[test]
    fn builds_upstream_shaped_node_create_payload() {
        let meta = meta(
            Method::POST,
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        );
        let payload = build_payload(&meta, StatusCode::CREATED, Some("chef.example.com")).unwrap();
        assert_eq!(payload["message_type"], "action");
        assert_eq!(payload["message_version"], "0.1.1");
        assert_eq!(payload["organization_name"], "acme");
        assert_eq!(payload["service_hostname"], "chef.example.com");
        assert_eq!(payload["requestor_name"], "pivotal");
        assert_eq!(payload["requestor_type"], "user");
        assert_eq!(payload["task"], "create");
        assert_eq!(payload["entity_type"], "node");
        assert_eq!(payload["entity_name"], "web1");
        assert_eq!(payload["data"]["name"], "web1");
        assert!(payload["id"].as_str().is_some_and(|id| !id.is_empty()));
    }

    #[test]
    fn reports_cookbook_put_201_as_create() {
        let meta = meta(
            Method::PUT,
            "/organizations/acme/cookbooks/nginx/1.2.3",
            b"{}",
        );
        let created = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(created["task"], "create");
        assert_eq!(created["entity_type"], "version");
        assert_eq!(created["entity_name"], "1.2.3");
        assert_eq!(created["parent_type"], "cookbook");
        assert_eq!(created["parent_name"], "nginx");

        let updated = build_payload(&meta, StatusCode::OK, None).unwrap();
        assert_eq!(updated["task"], "update");
    }

    #[test]
    fn redacts_passwords_in_action_data() {
        let meta = meta(
            Method::POST,
            "/users",
            br#"{"username":"alice","password":"hunter2","private_key":"-----BEGIN"}"#,
        );
        let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["entity_type"], "user");
        assert_eq!(payload["entity_name"], "alice");
        assert_eq!(payload["data"]["password"], REDACTED_PASSWORD);
        assert_eq!(payload["data"]["private_key"], REDACTED_PASSWORD);
    }

    #[test]
    fn ignores_read_only_and_forwarding_paths() {
        for (method, path) in [
            (Method::GET, "/organizations/acme/nodes"),
            (Method::POST, "/organizations/acme/data-collector"),
            (Method::POST, "/organizations/acme/search"),
            (Method::POST, "/organizations/acme/sandboxes"),
            (Method::POST, "/authenticate_user"),
        ] {
            let meta = meta(method.clone(), path, b"{}");
            assert!(
                build_payload(&meta, StatusCode::CREATED, None).is_none(),
                "unexpected notification for {method} {path}"
            );
        }
    }

    #[test]
    fn reports_data_bag_items_and_keys_with_parents() {
        let item = meta(
            Method::POST,
            "/organizations/acme/data/users",
            br#"{"id":"alice"}"#,
        );
        let payload = build_payload(&item, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["entity_type"], "item");
        assert_eq!(payload["entity_name"], "alice");
        assert_eq!(payload["parent_type"], "bag");
        assert_eq!(payload["parent_name"], "users");

        let key = meta(
            Method::POST,
            "/organizations/acme/clients/web1/keys",
            br#"{"name":"rotate"}"#,
        );
        let payload = build_payload(&key, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["entity_type"], "key");
        assert_eq!(payload["entity_name"], "rotate");
        assert_eq!(payload["parent_type"], "client");
        assert_eq!(payload["parent_name"], "web1");
    }

    #[test]
    fn reports_acl_and_membership_entities() {
        let acl = meta(
            Method::PUT,
            "/organizations/acme/nodes/web1/_acl/grant",
            br#"{"grant":{"actors":["pivotal"]}}"#,
        );
        let payload = build_payload(&acl, StatusCode::OK, None).unwrap();
        assert_eq!(payload["entity_type"], "permission");
        assert_eq!(payload["entity_name"], "grant");
        assert_eq!(payload["parent_type"], "node");
        assert_eq!(payload["parent_name"], "web1");

        let user_acl = meta(Method::PUT, "/users/alice/_acl/grant", b"{}");
        let payload = build_payload(&user_acl, StatusCode::OK, None).unwrap();
        assert_eq!(payload["parent_type"], "user");
        assert_eq!(payload["parent_name"], "alice");

        let invite = meta(
            Method::POST,
            "/organizations/acme/association_requests",
            br#"{"username":"alice"}"#,
        );
        let payload = build_payload(&invite, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["task"], "invite");
        assert_eq!(payload["entity_type"], "user");
        assert_eq!(payload["entity_name"], "alice");
    }

    #[test]
    fn large_request_bodies_are_reported_without_data() {
        let mut body = br#"{"name":"web1","blob":""#.to_vec();
        body.extend(std::iter::repeat_n(b'x', MAX_ACTION_DATA_BYTES));
        body.extend_from_slice(br#""}"#);
        let mut meta = meta(
            Method::POST,
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        );
        meta.body = Bytes::from(body);
        let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["entity_type"], "node");
        assert_eq!(payload["entity_name"], "web1");
        assert!(payload.get("data").is_none());
    }

    #[test]
    fn strips_ports_from_host_header() {
        assert_eq!(
            strip_port("chef.example.com:8000".into()),
            "chef.example.com"
        );
        assert_eq!(strip_port("[::1]:8000".into()), "[::1]");
    }
}
