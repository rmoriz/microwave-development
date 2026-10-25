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
//! collector. Request bodies above 256 KiB are not retained at all, and the
//! queue also has an 8 MiB retained-data budget; smaller JSON bodies are parsed
//! and scrubbed before enqueue. Together these bound queue memory. Dropping the
//! application state closes the channel; the worker drains already-queued jobs
//! before it exits, although abrupt process termination may truncate the drain.

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode};
use microwave_store::{ActorType, MetadataStore, Org, Principal};
use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};

const MESSAGE_VERSION: &str = "0.1.1";
/// Bounded queue. A burst of mutations may overflow this; excess messages are
/// dropped rather than growing memory or spawning tasks.
const QUEUE_CAPACITY: usize = 256;
/// Total retained `data` bytes allowed across queued jobs. Jobs that do not
/// fit are dropped rather than allowing the queue to retain hundreds of
/// megabytes of request documents.
const QUEUE_BYTE_BUDGET: usize = 8 * 1024 * 1024;
/// Request payloads larger than this are not retained at all. Together with
/// the byte budget this keeps queue memory predictable.
const MAX_ACTION_DATA_BYTES: usize = 256 * 1024;
const REDACTED_PASSWORD: &str = "*******";

struct ActionJob {
    meta: RequestMeta,
    status: StatusCode,
    /// Holds the queue byte budget until the worker finishes (or drops) the
    /// job.
    _permit: OwnedSemaphorePermit,
}

/// Owns the optional background worker and its bounded queue.
pub(crate) struct ActionDispatcher {
    root_url: Option<String>,
    timeout: Duration,
    service_hostname: Option<String>,
    /// Configured token, if any. When absent the worker resolves the
    /// `data_collector.token` secret once, off the request path.
    configured_token: Option<String>,
    store: Arc<dyn MetadataStore>,
    client: reqwest::Client,
    sender: Mutex<Option<mpsc::Sender<ActionJob>>>,
    budget: Arc<Semaphore>,
    dropped: AtomicU64,
}

impl ActionDispatcher {
    pub(crate) fn new(
        config: &microwave_config::Config,
        client: reqwest::Client,
        store: Arc<dyn MetadataStore>,
    ) -> Arc<Self> {
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
            configured_token: config
                .data_collector
                .token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(ToString::to_string),
            store,
            client,
            sender: Mutex::new(None),
            budget: Arc::new(Semaphore::new(QUEUE_BYTE_BUDGET)),
            dropped: AtomicU64::new(0),
        })
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.root_url.is_some()
    }

    /// Queue a notification. Returns without parsing large bodies, reading
    /// secrets, or waiting for the collector. Payload construction happens in
    /// the worker, so only already-bounded request metadata capture and a
    /// non-blocking enqueue run on the request path.
    pub(crate) async fn notify(&self, meta: RequestMeta, status: StatusCode) {
        if !self.is_enabled()
            || !status.is_success()
            || !is_candidate(meta.method.as_str(), &meta.path)
        {
            return;
        }

        let retained = meta.retained_bytes();
        let bytes = u32::try_from(retained).unwrap_or(u32::MAX);
        if bytes as usize > QUEUE_BYTE_BUDGET {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                dropped,
                bytes = retained,
                budget = QUEUE_BYTE_BUDGET,
                "data collector action exceeds the queue byte budget; dropping notification"
            );
            return;
        }
        let permit = match self.budget.clone().try_acquire_many_owned(bytes) {
            Ok(permit) => permit,
            Err(_) => {
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::warn!(
                    dropped,
                    bytes = retained,
                    budget = QUEUE_BYTE_BUDGET,
                    "data collector action queue byte budget is full; dropping notification"
                );
                return;
            }
        };

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
                self.store.clone(),
                self.configured_token.clone(),
                self.service_hostname.clone(),
            ));
            *guard = Some(sender);
        }
        let sender = guard.as_ref().expect("sender initialized above");
        match sender.try_send(ActionJob {
            meta,
            status,
            _permit: permit,
        }) {
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
                let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                // The worker stopped unexpectedly. Drop this notification and
                // let the next mutation start a fresh worker.
                *guard = None;
                tracing::warn!(dropped, "data collector action worker stopped; queue reset");
            }
        }
    }
}

async fn run_worker(
    mut receiver: mpsc::Receiver<ActionJob>,
    client: reqwest::Client,
    root_url: String,
    timeout: Duration,
    store: Arc<dyn MetadataStore>,
    configured_token: Option<String>,
    service_hostname: Option<String>,
) {
    let mut token = configured_token;
    let mut token_warning_emitted = false;

    while let Some(job) = receiver.recv().await {
        let Some(payload) = build_payload(&job.meta, job.status, service_hostname.as_deref())
        else {
            continue;
        };

        if token.is_none() {
            token = resolve_token(store.as_ref()).await;
            if token.is_none() && !token_warning_emitted {
                token_warning_emitted = true;
                tracing::warn!(
                    "data collector token is not configured; action notifications will be sent without a token and retried on the next job"
                );
            }
        }

        let mut request = client
            .post(&root_url)
            .timeout(timeout)
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(token) = token.as_deref() {
            request = request.header("x-data-collector-token", token);
        }
        match request.json(&payload).send().await {
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

/// Resolve and cache the action token in the worker. Configuration wins;
/// otherwise read the `data_collector.token` secret once.
async fn resolve_token(store: &dyn MetadataStore) -> Option<String> {
    store
        .get_secret("data_collector", "token")
        .await
        .ok()
        .flatten()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Keep only the headers used in the action payload. In particular, do not
/// retain client authorization headers in the queue.
fn audit_headers(headers: &HeaderMap) -> HeaderMap {
    const AUDIT_HEADERS: &[&str] = &[
        "host",
        "x-forwarded-host",
        "x-forwarded-for",
        "user-agent",
        "x-ops-timestamp",
        "x-request-id",
        "x-remote-request-id",
    ];
    let mut retained = HeaderMap::new();
    for name in AUDIT_HEADERS {
        if let Some(value) = headers.get(*name) {
            retained.insert(*name, value.clone());
        }
    }
    retained
}

/// Request information captured before the handler consumes the body. The raw
/// body is never retained: JSON at or below `MAX_ACTION_DATA_BYTES` is parsed
/// and scrubbed immediately. Larger bodies are parsed only to extract a small
/// entity-name hint and their `data` member is discarded.
pub(crate) struct RequestMeta {
    method: Method,
    path: String,
    headers: HeaderMap,
    data: Option<Value>,
    entity_data: Option<Value>,
    data_bytes: usize,
    requestor_name: String,
    requestor_type: &'static str,
    organization_name: Option<String>,
    /// Username resolved before a delete for endpoints whose path contains an
    /// opaque id rather than a name.
    resolved_username: Option<String>,
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
        let (data, entity_data, data_bytes) = prepare_action_data(&body);
        Some(Self {
            method: method.clone(),
            path: path.to_string(),
            headers: audit_headers(headers),
            data,
            entity_data,
            data_bytes,
            requestor_name: principal.name.clone(),
            requestor_type: match principal.actor_type {
                ActorType::Client => "client",
                ActorType::User => "user",
            },
            organization_name: org.map(|org| org.name.clone()),
            resolved_username: None,
        })
    }

    /// Approximate memory retained by this job after capture. The raw request
    /// body is intentionally absent; only bounded scrubbed data, a small
    /// entity hint, and metadata remain.
    fn retained_bytes(&self) -> usize {
        let entity_bytes = self
            .entity_data
            .as_ref()
            .and_then(|value| serde_json::to_vec(value).ok())
            .map_or(0, |encoded| encoded.len());
        self.data_bytes
            .saturating_add(entity_bytes)
            .saturating_add(self.path.len())
            .saturating_add(self.requestor_name.len())
            .saturating_add(self.organization_name.as_deref().map_or(0, str::len))
            .saturating_add(256)
    }

    pub(crate) fn set_resolved_username(&mut self, username: String) {
        self.resolved_username = Some(username);
    }
}

/// Parse and scrub request data before it can be retained by the queue. Bodies
/// over the cap keep only an entity-name hint and omit their `data` member.
fn prepare_action_data(body: &[u8]) -> (Option<Value>, Option<Value>, usize) {
    if body.is_empty() {
        return (None, None, 0);
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return (None, None, 0);
    };
    if body.len() > MAX_ACTION_DATA_BYTES {
        return (None, entity_hint(&value), 0);
    }
    let value = scrub_data(value);
    let bytes = serde_json::to_vec(&value)
        .map(|encoded| encoded.len())
        .unwrap_or(0);
    (Some(value), None, bytes)
}

/// Minimal top-level fields used to derive the entity name without retaining
/// an oversized request document. Values are scalar strings and are bounded;
/// object-valued `user` bodies keep only `username`.
fn entity_hint(value: &Value) -> Option<Value> {
    const MAX_HINT_VALUE_BYTES: usize = 256;
    let object = value.as_object()?;
    let mut hint = Map::new();
    for key in ["name", "id", "username", "groupname", "policy_name"] {
        if let Some(value) = object.get(key).and_then(Value::as_str) {
            hint.insert(
                key.into(),
                json!(truncate_hint(value, MAX_HINT_VALUE_BYTES)),
            );
        }
    }
    if let Some(user) = object.get("user") {
        if let Some(username) = user
            .as_str()
            .or_else(|| user.get("username").and_then(Value::as_str))
        {
            hint.insert(
                "user".into(),
                json!(truncate_hint(username, MAX_HINT_VALUE_BYTES)),
            );
        }
    }
    (!hint.is_empty()).then_some(Value::Object(hint))
}

fn truncate_hint(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// Cheap path/method prefilter used before capturing a request body. It must
/// stay conservative: unsupported paths simply produce no notification.
pub(crate) fn is_candidate(method: &str, path: &str) -> bool {
    if !matches!(method, "POST" | "PUT" | "DELETE") {
        return false;
    }
    let parts = path_parts(path);
    // These are intentionally omitted by the payload builder; skip the body
    // capture as well.
    if parts.get(2) == Some(&"policies") && parts.get(4) == Some(&"revisions") {
        return false;
    }
    if parts.contains(&"_acl") {
        return true;
    }
    match parts.first().copied() {
        Some("organizations") => match parts.get(2).copied() {
            None => true,
            Some(resource) => matches!(
                resource,
                "nodes"
                    | "roles"
                    | "environments"
                    | "clients"
                    | "groups"
                    | "users"
                    | "data"
                    | "cookbooks"
                    | "cookbook_artifacts"
                    | "policies"
                    | "policy_groups"
                    | "association_requests"
            ),
        },
        Some("users") => true,
        _ => false,
    }
}

/// Resolve the invited username for a DELETE association request before the
/// handler removes it. Returns `None` for other paths or when the invitation
/// no longer exists.
pub(crate) async fn resolve_invitation_username(
    store: &dyn MetadataStore,
    path: &str,
) -> Option<String> {
    let parts = path_parts(path);
    if parts.first() != Some(&"organizations")
        || parts.get(2) != Some(&"association_requests")
        || parts.len() != 4
    {
        return None;
    }
    store
        .find_invitation(parts[3])
        .await
        .ok()
        .flatten()
        .map(|invitation| invitation.username)
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
    let data = meta.data.as_ref();
    let entity_data = meta.entity_data.as_ref().or(data);
    let entity = entity_for(meta, entity_data)?;
    let task = task_for(meta, status, entity_data);

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
            .or_else(|| header(&meta.headers, "x-forwarded-host").map(strip_port))
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
    if let Some(data) = data {
        payload.insert("data".into(), data.clone());
    }

    Some(Value::Object(payload))
}

fn scrub_data(data: Value) -> Value {
    match data {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    if is_sensitive_key(&key) {
                        (key, json!(REDACTED_PASSWORD))
                    } else {
                        (key, scrub_data(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(scrub_data).collect()),
        other => other,
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower == "password"
        || lower.ends_with("_password")
        || lower == "private_key"
        || lower == "secret"
        || lower.ends_with("_secret")
        || lower == "token"
        || lower.ends_with("_token")
        || lower == "credentials"
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
        // Upstream only treats cookbook `PUT` 201 responses as creates; other
        // resources use `PUT` for updates even when they return 201.
        Method::PUT if status == StatusCode::CREATED && is_cookbook_version_path(&parts) => {
            "create".to_string()
        }
        Method::PUT => "update".to_string(),
        Method::DELETE => "delete".to_string(),
        _ => "update".to_string(),
    }
}

fn is_cookbook_version_path(parts: &[&str]) -> bool {
    parts.first() == Some(&"organizations")
        && parts.get(2) == Some(&"cookbooks")
        && parts.len() >= 5
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
            // Item endpoints use the path name; a request body must not spoof
            // the user being associated/dissociated. Collection POSTs have no
            // path name and fall back to the body.
            let username = name
                .map(ToString::to_string)
                .or_else(|| username_from_data(data))?;
            Some(Entity::simple("user", username))
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
            // Revision writes are not reported by upstream; only the policy
            // resource itself is.
            if parts.get(4) == Some(&"revisions") {
                return None;
            }
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
            // A DELETE addresses an invitation UUID, not a username. The auth
            // middleware resolves the invited user before the handler deletes
            // the invitation; if it could not be resolved, omit the action.
            if meta.method == Method::DELETE && parts.len() >= 4 {
                return meta
                    .resolved_username
                    .clone()
                    .map(|username| Entity::simple("user", username));
            }
            let username = if let Some(path_name) = name {
                path_name.to_string()
            } else {
                username_from_data(data)?
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

/// Membership bodies accept `username`, `user` as a string, or `user` as an
/// object with a `username` member. `name` remains a fallback for older
/// clients.
fn username_from_data(data: Option<&Value>) -> Option<String> {
    let object = data?.as_object()?;
    object
        .get("username")
        .and_then(Value::as_str)
        .or_else(|| {
            object.get("user").and_then(|user| {
                user.as_str()
                    .or_else(|| user.get("username").and_then(Value::as_str))
            })
        })
        .or_else(|| object.get("name").and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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
        "users" => "user",
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
    use axum::http::HeaderValue;

    fn principal() -> Principal {
        Principal {
            actor_id: "1".into(),
            authz_id: "2".into(),
            name: "pivotal".into(),
            actor_type: ActorType::User,
            org_id: None,
        }
    }

    fn meta_with_body(method: Method, path: &str, body: &[u8]) -> RequestMeta {
        let body = Bytes::copy_from_slice(body);
        let (data, entity_data, data_bytes) = prepare_action_data(&body);
        RequestMeta {
            method,
            path: path.into(),
            headers: HeaderMap::new(),
            data,
            entity_data,
            data_bytes,
            requestor_name: principal().name,
            requestor_type: "user",
            organization_name: Some("acme".into()),
            resolved_username: None,
        }
    }

    fn meta(method: Method, path: &str, body: &'static [u8]) -> RequestMeta {
        meta_with_body(method, path, body)
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
    fn oversized_entity_hints_do_not_retain_credentials_or_large_values() {
        let mut body = br#"{"user":{"username":"alice","password":"hunter2"},"name":""#.to_vec();
        body.extend(std::iter::repeat_n(b'n', 1024));
        body.extend_from_slice(br#"","blob":""#);
        body.extend(std::iter::repeat_n(b'x', MAX_ACTION_DATA_BYTES));
        body.extend_from_slice(br#""}"#);
        let meta = meta_with_body(Method::POST, "/organizations/acme/users", &body);
        assert!(meta.data.is_none());
        let hint = meta.entity_data.as_ref().unwrap();
        assert_eq!(hint["user"], "alice");
        assert!(hint.get("password").is_none());
        assert!(hint["name"].as_str().unwrap().len() <= 256);
        assert!(meta.retained_bytes() < 4096);
    }

    #[test]
    fn queue_budget_is_below_the_naive_capacity_product() {
        const {
            assert!(
                QUEUE_BYTE_BUDGET < QUEUE_CAPACITY * MAX_ACTION_DATA_BYTES,
                "queue budget must be smaller than capacity times the per-body cap"
            );
        }
        let meta = meta_with_body(
            Method::POST,
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        );
        assert!(meta.retained_bytes() <= MAX_ACTION_DATA_BYTES + 2048);
        assert!(meta.retained_bytes() >= 1);
    }

    #[test]
    fn large_request_bodies_are_not_retained() {
        let mut body = br#"{"name":"web1","normal":{"blob":""#.to_vec();
        body.extend(std::iter::repeat_n(b'x', MAX_ACTION_DATA_BYTES));
        body.extend_from_slice(br#""}}"#);
        let meta = meta_with_body(Method::POST, "/organizations/acme/nodes", &body);
        assert!(meta.data.is_none(), "oversized body was retained");
        assert!(
            meta.retained_bytes() < 2048,
            "job retained too much metadata: {}",
            meta.retained_bytes()
        );
        let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["entity_type"], "node");
        assert_eq!(payload["entity_name"], "web1");
        assert!(payload.get("data").is_none());
    }

    #[test]
    fn membership_bodies_accept_user_strings_and_objects() {
        for body in [
            &br#"{"username":"alice"}"#[..],
            &br#"{"user":"alice"}"#[..],
            &br#"{"user":{"username":"alice"}}"#[..],
        ] {
            let meta = meta(Method::POST, "/organizations/acme/users", body);
            let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
            assert_eq!(payload["task"], "associate");
            assert_eq!(payload["entity_type"], "user");
            assert_eq!(payload["entity_name"], "alice");
        }
    }

    #[test]
    fn invitation_deletes_use_the_resolved_username_or_are_omitted() {
        let mut meta = meta(
            Method::DELETE,
            "/organizations/acme/association_requests/11111111-1111-1111-1111-111111111111",
            b"",
        );
        assert!(build_payload(&meta, StatusCode::OK, None).is_none());
        meta.set_resolved_username("alice".into());
        let payload = build_payload(&meta, StatusCode::OK, None).unwrap();
        assert_eq!(payload["task"], "reject");
        assert_eq!(payload["entity_type"], "user");
        assert_eq!(payload["entity_name"], "alice");
    }

    #[test]
    fn item_scoped_org_user_actions_prefer_the_path_name() {
        let meta = meta(
            Method::DELETE,
            "/organizations/acme/users/alice",
            br#"{"user":"mallory"}"#,
        );
        let payload = build_payload(&meta, StatusCode::OK, None).unwrap();
        assert_eq!(payload["task"], "dissociate");
        assert_eq!(payload["entity_type"], "user");
        assert_eq!(payload["entity_name"], "alice");
    }

    #[test]
    fn org_user_acl_parent_type_is_user() {
        let meta = meta(
            Method::PUT,
            "/organizations/acme/users/alice/_acl/grant",
            b"{}",
        );
        let payload = build_payload(&meta, StatusCode::OK, None).unwrap();
        assert_eq!(payload["entity_type"], "permission");
        assert_eq!(payload["parent_type"], "user");
        assert_eq!(payload["parent_name"], "alice");
    }

    #[test]
    fn only_cookbook_put_201_is_a_create() {
        let key = meta(
            Method::PUT,
            "/organizations/acme/clients/web1/keys/rotate",
            br#"{"name":"rotate"}"#,
        );
        assert_eq!(
            build_payload(&key, StatusCode::CREATED, None).unwrap()["task"],
            "update"
        );

        let policy_group = meta(
            Method::PUT,
            "/organizations/acme/policy_groups/production/policies/base",
            br#"{"revision_id":"abc"}"#,
        );
        assert_eq!(
            build_payload(&policy_group, StatusCode::CREATED, None).unwrap()["task"],
            "update"
        );

        let cookbook = meta(
            Method::PUT,
            "/organizations/acme/cookbooks/nginx/1.2.3",
            b"{}",
        );
        assert_eq!(
            build_payload(&cookbook, StatusCode::CREATED, None).unwrap()["task"],
            "create"
        );
    }

    #[test]
    fn policy_revision_writes_are_not_reported() {
        let create = meta(
            Method::POST,
            "/organizations/acme/policies/base/revisions",
            br#"{"name":"base","revision_id":"abc"}"#,
        );
        assert!(build_payload(&create, StatusCode::CREATED, None).is_none());

        let delete = meta(
            Method::DELETE,
            "/organizations/acme/policies/base/revisions/abc",
            b"",
        );
        assert!(build_payload(&delete, StatusCode::OK, None).is_none());

        let delete_policy = meta(Method::DELETE, "/organizations/acme/policies/base", b"");
        assert_eq!(
            build_payload(&delete_policy, StatusCode::OK, None).unwrap()["task"],
            "delete"
        );
    }

    #[test]
    fn audit_headers_drop_client_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ops-authorization-1", HeaderValue::from_static("secret"));
        headers.insert("x-ops-sign", HeaderValue::from_static("version=1.3"));
        headers.insert("user-agent", HeaderValue::from_static("knife/18.0"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.5"));
        let retained = audit_headers(&headers);
        assert!(!retained.contains_key("x-ops-authorization-1"));
        assert!(!retained.contains_key("x-ops-sign"));
        assert_eq!(retained.get("user-agent").unwrap(), "knife/18.0");
        assert_eq!(retained.get("x-forwarded-for").unwrap(), "10.0.0.5");
    }

    #[test]
    fn nested_secrets_are_redacted_before_retention() {
        let meta = meta(
            Method::POST,
            "/organizations/acme/nodes",
            br#"{"name":"web1","nested":{"password":"p","token":"t","safe":"ok"},"list":[{"secret":"s"}]}"#,
        );
        // The prepared data, which is what the queue retains, is already
        // scrubbed.
        assert_eq!(
            meta.data.as_ref().unwrap()["nested"]["password"],
            REDACTED_PASSWORD
        );
        assert_eq!(
            meta.data.as_ref().unwrap()["nested"]["token"],
            REDACTED_PASSWORD
        );
        let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["data"]["nested"]["password"], REDACTED_PASSWORD);
        assert_eq!(payload["data"]["nested"]["token"], REDACTED_PASSWORD);
        assert_eq!(payload["data"]["nested"]["safe"], "ok");
        assert_eq!(payload["data"]["list"][0]["secret"], REDACTED_PASSWORD);
    }

    #[test]
    fn forwarded_host_is_used_for_service_hostname() {
        let mut meta = meta(
            Method::POST,
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        );
        meta.headers.insert(
            "host",
            HeaderValue::from_static("internal.example.com:8000"),
        );
        meta.headers.insert(
            "x-forwarded-host",
            HeaderValue::from_static("chef.example.com"),
        );
        let payload = build_payload(&meta, StatusCode::CREATED, None).unwrap();
        assert_eq!(payload["service_hostname"], "chef.example.com");
    }

    #[test]
    fn prefilters_unsupported_paths_without_reading_the_body() {
        assert!(is_candidate("POST", "/organizations/acme/nodes"));
        assert!(is_candidate("PUT", "/users/alice/keys/default"));
        assert!(is_candidate(
            "PUT",
            "/organizations/acme/nodes/web1/_acl/grant"
        ));
        assert!(!is_candidate("POST", "/organizations/acme/data-collector"));
        assert!(!is_candidate("POST", "/organizations/acme/search"));
        assert!(!is_candidate(
            "POST",
            "/organizations/acme/policies/base/revisions"
        ));
        assert!(is_candidate(
            "DELETE",
            "/organizations/acme/association_requests/123"
        ));
        assert!(!is_candidate("GET", "/organizations/acme/nodes"));
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
