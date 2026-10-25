//! Search API, query parsing and pluggable search backends.

mod backend;
mod expand;
pub(crate) mod index;
mod opensearch;
mod query;
mod reindex;

use crate::helpers::{base_url, container_authz, is_superuser, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Org, Permission};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

use backend::SearchRequest as BackendSearchRequest;
use query::Query as SearchQuery;

pub use backend::{
    build_search_backend, EmbeddedSearch, IndexKind, SearchBackend, SearchDocument, SearchError,
    SearchHits, SearchRequest,
};
pub use opensearch::{OpenSearchBackend, SearchProvider};
pub use reindex::{reindex, ReindexReport};

const BUILTIN_INDEXES: [&str; 4] = ["client", "environment", "node", "role"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/organizations/{organization}/search", get(list_indexes))
        .route(
            "/organizations/{organization}/search/{index}",
            get(search).post(partial_search),
        )
        // Upstream accepts a trailing slash on search index URLs.
        .route(
            "/organizations/{organization}/search/{index}/",
            get(search).post(partial_search),
        )
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    start: Option<String>,
    rows: Option<String>,
}

async fn list_indexes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let base = base_url(&headers, &state);
    let mut indexes = Map::new();

    for index in BUILTIN_INDEXES {
        indexes.insert(
            index.to_string(),
            json!(format!("{base}/organizations/{}/search/{index}", org.name)),
        );
    }

    for data_bag in state
        .store
        .list_data_bags(&org.id)
        .await
        .map_err(ApiError::from)?
    {
        indexes.insert(
            data_bag.name.clone(),
            json!(format!(
                "{base}/organizations/{}/search/{}",
                org.name, data_bag.name
            )),
        );
    }

    Ok(Json(Value::Object(indexes)))
}

async fn search(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(api_version): Extension<ApiVersion>,
    Path((_organization, index)): Path<(String, String)>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let hits = execute_search(&state, &auth, org, &index, &params).await?;

    let entries = hydrate(&state, org, &index, &hits.ids).await?;
    let rows: Vec<Value> = entries
        .into_iter()
        .map(|mut entry| {
            strip_client_public_key(&index, api_version.0, &mut entry.output);
            entry.output
        })
        .collect();

    Ok(Json(json!({
        "total": hits.total,
        "start": hits.start,
        "rows": rows
    })))
}

async fn partial_search(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(api_version): Extension<ApiVersion>,
    Path((_organization, index)): Path<(String, String)>,
    Query(params): Query<SearchParams>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let paths = parse_partial_paths(body.map_err(json_rejection)?)?;
    let hits = execute_search(&state, &auth, org, &index, &params).await?;

    let entries = hydrate(&state, org, &index, &hits.ids).await?;
    let base = base_url(&headers, &state);
    let rows: Vec<Value> = entries
        .into_iter()
        .map(|mut entry| {
            strip_client_public_key(&index, api_version.0, &mut entry.output);
            for source in &mut entry.partial_sources {
                strip_client_public_key(&index, api_version.0, source);
            }
            let mut data = Map::new();
            for (name, path) in &paths {
                let value = entry
                    .partial_sources
                    .iter()
                    .find_map(|source| expand::lookup_path(source, path))
                    .cloned()
                    .unwrap_or(Value::Null);
                data.insert(name.clone(), value);
            }
            json!({
                "url": format!("{base}/organizations/{}{}", org.name, entry.url),
                "data": Value::Object(data)
            })
        })
        .collect();

    Ok(Json(json!({
        "total": hits.total,
        "start": hits.start,
        "rows": rows
    })))
}

/// Upstream builds client rows with `assemble_client_ejson/2`, which includes
/// the public key only for API v0. Key management moved to `/keys` in v1.
fn strip_client_public_key(index: &str, api_version: u8, value: &mut Value) {
    if index == "client" && api_version >= 1 {
        if let Some(object) = value.as_object_mut() {
            object.remove("public_key");
        }
    }
}

/// Result of a backend search, including the echoed page offset.
struct ExecutedSearch {
    total: usize,
    start: usize,
    ids: Vec<String>,
}

async fn execute_search(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    index: &str,
    params: &SearchParams,
) -> Result<ExecutedSearch, ApiError> {
    let kind = IndexKind::from_index(index);
    authorize_index(state, auth, org, &kind).await?;
    let query = parse_query(params.q.as_deref())?;
    let start = parse_non_negative("start", params.start.as_deref(), 0)?;
    let rows = parse_non_negative("rows", params.rows.as_deref(), 1000)?;

    let request = BackendSearchRequest {
        org_id: org.id.clone(),
        kind,
        query,
        start,
        rows,
        actor_authz_id: (!is_superuser(auth)).then(|| auth.principal.authz_id.clone()),
    };
    let hits = state
        .search
        .search(&request)
        .await
        .map_err(ApiError::from)?;

    Ok(ExecutedSearch {
        total: hits.total,
        start,
        ids: hits.ids,
    })
}

async fn authorize_index(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    kind: &IndexKind,
) -> Result<(), ApiError> {
    let container_name = match kind {
        IndexKind::Node => "nodes",
        IndexKind::Role => "roles",
        IndexKind::Environment => "environments",
        IndexKind::Client => "clients",
        IndexKind::DataBag(_) => "data",
    };
    let container = container_authz(state, org, container_name).await?;
    require_permission(state, auth, &container, Permission::Read).await?;

    if let IndexKind::DataBag(name) = kind {
        if state
            .store
            .find_data_bag(&org.id, name)
            .await
            .map_err(ApiError::from)?
            .is_none()
        {
            return Err(index::unknown_index(name));
        }
    }

    Ok(())
}

async fn hydrate(
    state: &AppState,
    org: &Org,
    index: &str,
    ids: &[String],
) -> Result<Vec<index::SearchEntry>, ApiError> {
    let kind = IndexKind::from_index(index);
    let mut entries = index::load_entries(state.store.as_ref(), &org.id, &kind).await?;
    let mut by_id: HashMap<String, index::SearchEntry> = entries
        .drain(..)
        .map(|entry| (entry.id.clone(), entry))
        .collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

fn parse_query(raw: Option<&str>) -> Result<SearchQuery, ApiError> {
    let raw = raw.unwrap_or("*:*");
    if raw.is_empty() {
        return Err(ApiError::bad_request(
            "invalid query: query string cannot be empty",
        ));
    }
    SearchQuery::parse(raw)
        .map_err(|error| ApiError::bad_request(format!("invalid query: {error}")))
}

fn parse_non_negative(name: &str, raw: Option<&str>, default: usize) -> Result<usize, ApiError> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    raw.parse::<usize>().map_err(|_| {
        ApiError::bad_request(format!("invalid {name}: expected a non-negative integer"))
    })
}

fn parse_partial_paths(body: Json<Value>) -> Result<Vec<(String, Vec<String>)>, ApiError> {
    let object = body
        .0
        .as_object()
        .ok_or_else(|| ApiError::bad_request("partial search body must be a JSON object"))?;

    let mut paths = Vec::with_capacity(object.len());
    for (name, value) in object {
        let segments = value.as_array().ok_or_else(|| {
            ApiError::bad_request(format!(
                "partial search path for {name} must be an array of strings"
            ))
        })?;
        let mut path = Vec::with_capacity(segments.len());
        for segment in segments {
            let segment = segment.as_str().ok_or_else(|| {
                ApiError::bad_request(format!(
                    "partial search path for {name} must be an array of strings"
                ))
            })?;
            path.push(segment.to_string());
        }
        paths.push((name.clone(), path));
    }

    Ok(paths)
}

fn json_rejection(rejection: JsonRejection) -> ApiError {
    ApiError::bad_request(rejection.body_text())
}
