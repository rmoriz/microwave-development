//! Search API and the first in-memory index implementation.

mod expand;
mod query;

use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Org, Permission, StoredDataBagItem, StoredNode, StoredObject};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use query::{Fields, Query as SearchQuery};

const BUILTIN_INDEXES: [&str; 4] = ["client", "environment", "node", "role"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/organizations/{organization}/search", get(list_indexes))
        .route(
            "/organizations/{organization}/search/{index}",
            get(search).post(partial_search),
        )
}

#[derive(Debug, Deserialize)]
struct SearchParams {
    q: Option<String>,
    start: Option<String>,
    rows: Option<String>,
}

struct SearchEntry {
    url: String,
    output: Value,
    fields: Fields,
    partial_sources: Vec<Value>,
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
    Path((_organization, index)): Path<(String, String)>,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let query = parse_query(params.q.as_deref())?;
    let start = parse_non_negative("start", params.start.as_deref(), 0)?;
    let rows = parse_non_negative("rows", params.rows.as_deref(), 1000)?;

    let entries = load_entries(&state, &auth, org, &index)?;
    let matched: Vec<SearchEntry> = entries
        .into_iter()
        .filter(|entry| query.matches(&entry.fields))
        .collect();
    let total = matched.len();
    let rows: Vec<Value> = matched
        .into_iter()
        .skip(start)
        .take(rows)
        .map(|entry| entry.output)
        .collect();

    Ok(Json(json!({
        "total": total,
        "start": start,
        "rows": rows
    })))
}

async fn partial_search(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, index)): Path<(String, String)>,
    Query(params): Query<SearchParams>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let paths = parse_partial_paths(body.map_err(json_rejection)?)?;
    let query = parse_query(params.q.as_deref())?;
    let start = parse_non_negative("start", params.start.as_deref(), 0)?;
    let rows = parse_non_negative("rows", params.rows.as_deref(), 1000)?;

    let entries = load_entries(&state, &auth, org, &index)?;
    let base = base_url(&headers, &state);
    let matched: Vec<SearchEntry> = entries
        .into_iter()
        .filter(|entry| query.matches(&entry.fields))
        .collect();
    let total = matched.len();
    let rows: Vec<Value> = matched
        .into_iter()
        .skip(start)
        .take(rows)
        .map(|entry| {
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
        "total": total,
        "start": start,
        "rows": rows
    })))
}

fn load_entries(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    index: &str,
) -> Result<Vec<SearchEntry>, ApiError> {
    let container_name = match index {
        "node" => "nodes",
        "role" => "roles",
        "environment" => "environments",
        "client" => "clients",
        _ => "data",
    };
    let container = container_authz(state, org, container_name)?;
    require_permission(state, auth, &container, Permission::Read)?;

    match index {
        "node" => node_entries(state, org),
        "role" => object_entries(
            state.store.list_roles(&org.id).map_err(ApiError::from)?,
            "/roles",
            true,
        ),
        "environment" => object_entries(
            state
                .store
                .list_environments(&org.id)
                .map_err(ApiError::from)?,
            "/environments",
            false,
        ),
        "client" => client_entries(state, org),
        data_bag => data_bag_entries(state, org, data_bag),
    }
}

fn node_entries(state: &AppState, org: &Org) -> Result<Vec<SearchEntry>, ApiError> {
    state
        .store
        .list_nodes(&org.id)
        .map_err(ApiError::from)?
        .into_iter()
        .map(node_entry)
        .collect()
}

fn node_entry(node: StoredNode) -> Result<SearchEntry, ApiError> {
    let raw: Value =
        serde_json::from_str(&node.serialized_object).map_err(|_| ApiError::internal())?;
    let merged = expand::merged_node(&raw);
    let mut fields = expand::flatten(&merged);
    expand::add_run_list_expansions(&raw, &mut fields);

    Ok(SearchEntry {
        url: format!("/nodes/{}", node.name),
        output: raw.clone(),
        fields,
        partial_sources: vec![merged, raw],
    })
}

fn object_entries(
    objects: Vec<StoredObject>,
    path: &str,
    expand_run_list: bool,
) -> Result<Vec<SearchEntry>, ApiError> {
    objects
        .into_iter()
        .map(|object| {
            let raw: Value = serde_json::from_str(&object.serialized_object)
                .map_err(|_| ApiError::internal())?;
            let mut fields = expand::flatten(&raw);
            if expand_run_list {
                expand::add_run_list_expansions(&raw, &mut fields);
            }
            Ok(SearchEntry {
                url: format!("{path}/{}", object.name),
                output: raw.clone(),
                fields,
                partial_sources: vec![raw],
            })
        })
        .collect()
}

fn client_entries(state: &AppState, org: &Org) -> Result<Vec<SearchEntry>, ApiError> {
    state
        .store
        .list_clients(&org.id)
        .map_err(ApiError::from)?
        .into_iter()
        .map(|client| {
            let value = json!({
                "name": client.name,
                "validator": client.validator,
                "admin": client.admin,
                "json_class": "Chef::ApiClient",
                "chef_type": "client"
            });
            let fields = expand::flatten(&value);
            Ok(SearchEntry {
                url: format!("/clients/{}", client.name),
                output: value.clone(),
                fields,
                partial_sources: vec![value],
            })
        })
        .collect()
}

fn data_bag_entries(
    state: &AppState,
    org: &Org,
    data_bag: &str,
) -> Result<Vec<SearchEntry>, ApiError> {
    state
        .store
        .find_data_bag(&org.id, data_bag)
        .map_err(ApiError::from)?
        .ok_or_else(|| unknown_index(data_bag))?;

    state
        .store
        .list_data_bag_items(&org.id, data_bag)
        .map_err(ApiError::from)?
        .into_iter()
        .map(|item| data_bag_item_entry(item, data_bag))
        .collect()
}

fn data_bag_item_entry(item: StoredDataBagItem, data_bag: &str) -> Result<SearchEntry, ApiError> {
    let raw: Value =
        serde_json::from_str(&item.serialized_object).map_err(|_| ApiError::internal())?;
    let wrapper = json!({
        "name": format!("data_bag_item_{}_{}", data_bag, item.item_name),
        "json_class": "Chef::DataBagItem",
        "chef_type": "data_bag_item",
        "data_bag": data_bag,
        "raw_data": raw
    });

    let mut fields = expand::flatten(&raw);
    for (name, value) in [
        (
            "name",
            format!("data_bag_item_{}_{}", data_bag, item.item_name),
        ),
        ("json_class", "Chef::DataBagItem".to_string()),
        ("chef_type", "data_bag_item".to_string()),
        ("data_bag", data_bag.to_string()),
    ] {
        fields.entry(name.to_string()).or_default().push(value);
    }

    Ok(SearchEntry {
        url: format!("/data/{data_bag}/{}", item.item_name),
        output: wrapper.clone(),
        fields,
        partial_sources: vec![raw, wrapper],
    })
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

fn unknown_index(index: &str) -> ApiError {
    ApiError::not_found(format!(
        "I don't know how to search for {index} data objects."
    ))
}
