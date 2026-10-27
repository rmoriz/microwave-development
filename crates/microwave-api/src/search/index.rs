//! Index document construction shared by the embedded and OpenSearch backends.
//!
//! The embedded backend searches the flattened documents in memory. The
//! OpenSearch backend sends the same flattened content as `key__=__value`
//! tokens in the `content` field, matching the upstream Chef index mapping.

use crate::search::backend::{IndexKind, SearchDocument};
use crate::search::expand;
use crate::search::query::Fields;
use crate::{ApiError, AppState};
use microwave_store::{
    ActorType, Client, MetadataStore, Org, StoredDataBagItem, StoredNode, StoredObject,
};
use serde_json::{json, Value};
use std::collections::HashMap;

/// A fully hydrated search entry. Entries are loaded from the store after the
/// backend reports matching document IDs.
#[derive(Debug)]
pub(crate) struct SearchEntry {
    /// Store identifier (GUID) used as the OpenSearch `_id`.
    pub id: String,
    /// Authorization object consulted for read filtering. Data bag items use
    /// the containing bag's ACL.
    pub authz_id: String,
    pub url: String,
    pub output: Value,
    pub fields: Fields,
    pub partial_sources: Vec<Value>,
}

/// Loads and expands every searchable document of `kind` in an organization.
pub(crate) async fn load_entries(
    store: &dyn MetadataStore,
    org_id: &str,
    kind: &IndexKind,
) -> Result<Vec<SearchEntry>, ApiError> {
    let mut entries = match kind {
        IndexKind::Node => node_entries(store, org_id).await,
        IndexKind::Role => object_entries(
            store.list_roles(org_id).await.map_err(ApiError::from)?,
            "/roles",
            true,
        ),
        IndexKind::Environment => object_entries(
            store
                .list_environments(org_id)
                .await
                .map_err(ApiError::from)?,
            "/environments",
            false,
        ),
        IndexKind::Client => client_entries(store, org_id).await,
        IndexKind::DataBag(name) => data_bag_entries(store, org_id, name).await,
    }?;

    // The OpenSearch backend stores these as dedicated keyword fields. The
    // embedded backend must expose the same names in its flattened view so
    // queries such as `X_CHEF_type_CHEF_X:node` behave identically.
    add_entry_metadata(org_id, kind, &mut entries);

    Ok(entries)
}

/// Loads and expands only the searchable documents whose ids are in `ids`.
///
/// Search backends return document ids, so hydration does not need to expand
/// the whole organization index. Entries are returned in `ids` order.
pub(crate) async fn load_entries_by_ids(
    store: &dyn MetadataStore,
    org_id: &str,
    kind: &IndexKind,
    ids: &[String],
) -> Result<Vec<SearchEntry>, ApiError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries = match kind {
        IndexKind::Node => store
            .find_nodes_by_ids(org_id, ids)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(node_entry)
            .collect::<Result<Vec<_>, _>>(),
        IndexKind::Role => object_entries(
            store
                .find_roles_by_ids(org_id, ids)
                .await
                .map_err(ApiError::from)?,
            "/roles",
            true,
        ),
        IndexKind::Environment => object_entries(
            store
                .find_environments_by_ids(org_id, ids)
                .await
                .map_err(ApiError::from)?,
            "/environments",
            false,
        ),
        IndexKind::Client => client_entries_by_ids(store, org_id, ids).await,
        IndexKind::DataBag(name) => data_bag_entries_by_ids(store, org_id, name, ids).await,
    }?;

    add_entry_metadata(org_id, kind, &mut entries);

    let mut by_id: HashMap<String, SearchEntry> = entries
        .drain(..)
        .map(|entry| (entry.id.clone(), entry))
        .collect();
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

fn add_entry_metadata(org_id: &str, kind: &IndexKind, entries: &mut [SearchEntry]) {
    let database = format!("chef_{org_id}");
    let data_bag = match kind {
        IndexKind::DataBag(name) => Some(name.as_str()),
        _ => None,
    };
    for entry in entries {
        add_metadata_field(&mut entry.fields, "X_CHEF_id_CHEF_X", &entry.id);
        add_metadata_field(&mut entry.fields, "X_CHEF_database_CHEF_X", &database);
        add_metadata_field(&mut entry.fields, "X_CHEF_type_CHEF_X", kind.object_type());
        if let Some(data_bag) = data_bag {
            add_metadata_field(&mut entry.fields, "data_bag", data_bag);
        }
    }
}

fn add_metadata_field(fields: &mut Fields, name: &str, value: &str) {
    let values = fields.entry(name.to_string()).or_default();
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

/// Builds the document that the OpenSearch backend indexes for a node.
pub(crate) fn node_document(org_id: &str, node: &StoredNode) -> Result<SearchDocument, ApiError> {
    let entry = node_entry(node.clone())?;
    Ok(document_for_entry(
        org_id,
        &IndexKind::Node,
        "node",
        None,
        entry,
    ))
}

/// Builds the document that the OpenSearch backend indexes for a role or
/// environment.
pub(crate) fn object_document(
    org_id: &str,
    kind: IndexKind,
    object: &StoredObject,
) -> Result<SearchDocument, ApiError> {
    let expand_run_list = kind == IndexKind::Role;
    let path = match kind {
        IndexKind::Role => "/roles",
        IndexKind::Environment => "/environments",
        _ => return Err(ApiError::internal()),
    };
    let entry = object_entry(object.clone(), path, expand_run_list)?;
    let object_type = kind.object_type();
    Ok(document_for_entry(org_id, &kind, object_type, None, entry))
}

/// Builds the document that the OpenSearch backend indexes for a client.
pub(crate) fn client_document(
    org_id: &str,
    org_name: &str,
    client: &Client,
    public_key: Option<&str>,
) -> SearchDocument {
    let entry = client_entry(client.clone(), org_name, public_key);
    document_for_entry(org_id, &IndexKind::Client, "client", None, entry)
}

/// Builds the document that the OpenSearch backend indexes for a data bag
/// item.
pub(crate) fn data_bag_item_document(
    org_id: &str,
    item: &StoredDataBagItem,
) -> Result<SearchDocument, ApiError> {
    let data_bag = item.data_bag_name.clone();
    let entry = data_bag_item_entry(item.clone(), &data_bag, String::new())?;
    Ok(document_for_entry(
        org_id,
        &IndexKind::DataBag(data_bag.clone()),
        "data_bag_item",
        Some(&data_bag),
        entry,
    ))
}

pub(crate) fn document_for_entry(
    org_id: &str,
    kind: &IndexKind,
    object_type: &str,
    data_bag: Option<&str>,
    mut entry: SearchEntry,
) -> SearchDocument {
    add_metadata_field(&mut entry.fields, "X_CHEF_id_CHEF_X", &entry.id);
    add_metadata_field(
        &mut entry.fields,
        "X_CHEF_database_CHEF_X",
        &format!("chef_{org_id}"),
    );
    add_metadata_field(&mut entry.fields, "X_CHEF_type_CHEF_X", object_type);
    if let Some(data_bag) = data_bag {
        add_metadata_field(&mut entry.fields, "data_bag", data_bag);
    }

    let mut content = String::new();
    for (name, values) in &entry.fields {
        for value in values {
            push_pair(&mut content, name, value);
        }
    }

    SearchDocument {
        id: entry.id,
        org_id: org_id.to_string(),
        kind: kind.clone(),
        content,
    }
}

fn push_pair(content: &mut String, name: &str, value: &str) {
    content.push_str(name);
    content.push_str("__=__");
    content.push_str(value);
    content.push(' ');
}

async fn node_entries(
    store: &dyn MetadataStore,
    org_id: &str,
) -> Result<Vec<SearchEntry>, ApiError> {
    store
        .list_nodes(org_id)
        .await
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
        id: node.id,
        authz_id: node.authz_id,
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
        .map(|object| object_entry(object, path, expand_run_list))
        .collect()
}

fn object_entry(
    object: StoredObject,
    path: &str,
    expand_run_list: bool,
) -> Result<SearchEntry, ApiError> {
    let raw: Value =
        serde_json::from_str(&object.serialized_object).map_err(|_| ApiError::internal())?;
    let mut fields = expand::flatten(&raw);
    if expand_run_list {
        expand::add_run_list_expansions(&raw, &mut fields);
    }
    Ok(SearchEntry {
        id: object.id,
        authz_id: object.authz_id,
        url: format!("{path}/{}", object.name),
        output: raw.clone(),
        fields,
        partial_sources: vec![raw],
    })
}

async fn client_entries(
    store: &dyn MetadataStore,
    org_id: &str,
) -> Result<Vec<SearchEntry>, ApiError> {
    let org = store
        .find_org_by_id(org_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {org_id}")))?;
    let mut entries = Vec::new();
    for client in store.list_clients(org_id).await.map_err(ApiError::from)? {
        let keys = store
            .find_actor_keys(&client.id, ActorType::Client)
            .await
            .map_err(ApiError::from)?;
        let public_key = keys
            .iter()
            .find(|key| key.key_name == "default")
            .or_else(|| keys.first())
            .map(|key| key.public_key.clone());
        entries.push(client_entry(client, &org.name, public_key.as_deref()));
    }
    Ok(entries)
}

async fn client_entries_by_ids(
    store: &dyn MetadataStore,
    org_id: &str,
    ids: &[String],
) -> Result<Vec<SearchEntry>, ApiError> {
    let org = store
        .find_org_by_id(org_id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {org_id}")))?;
    let mut entries = Vec::new();
    for client in store
        .find_clients_by_ids(org_id, ids)
        .await
        .map_err(ApiError::from)?
    {
        let keys = store
            .find_actor_keys(&client.id, ActorType::Client)
            .await
            .map_err(ApiError::from)?;
        let public_key = keys
            .iter()
            .find(|key| key.key_name == "default")
            .or_else(|| keys.first())
            .map(|key| key.public_key.clone());
        entries.push(client_entry(client, &org.name, public_key.as_deref()));
    }
    Ok(entries)
}

fn client_entry(client: Client, org_name: &str, public_key: Option<&str>) -> SearchEntry {
    let mut value = json!({
        "name": client.name,
        "clientname": client.name,
        "validator": client.validator,
        "orgname": org_name,
        "admin": client.admin,
        "json_class": "Chef::ApiClient",
        "chef_type": "client"
    });
    if let Some(public_key) = public_key {
        value["public_key"] = json!(public_key);
    }
    let fields = expand::flatten(&value);
    SearchEntry {
        id: client.id,
        authz_id: client.authz_id,
        url: format!("/clients/{}", client.name),
        output: value.clone(),
        fields,
        partial_sources: vec![value],
    }
}

async fn data_bag_entries(
    store: &dyn MetadataStore,
    org_id: &str,
    data_bag: &str,
) -> Result<Vec<SearchEntry>, ApiError> {
    let bag = store
        .find_data_bag(org_id, data_bag)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| unknown_index(data_bag))?;

    store
        .list_data_bag_items(org_id, data_bag)
        .await
        .map_err(ApiError::from)?
        .into_iter()
        .map(|item| data_bag_item_entry(item, data_bag, bag.authz_id.clone()))
        .collect()
}

async fn data_bag_entries_by_ids(
    store: &dyn MetadataStore,
    org_id: &str,
    data_bag: &str,
    ids: &[String],
) -> Result<Vec<SearchEntry>, ApiError> {
    let bag = store
        .find_data_bag(org_id, data_bag)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| unknown_index(data_bag))?;

    store
        .find_data_bag_items_by_ids(org_id, data_bag, ids)
        .await
        .map_err(ApiError::from)?
        .into_iter()
        .map(|item| data_bag_item_entry(item, data_bag, bag.authz_id.clone()))
        .collect()
}

fn data_bag_item_entry(
    item: StoredDataBagItem,
    data_bag: &str,
    authz_id: String,
) -> Result<SearchEntry, ApiError> {
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
        id: item.id,
        authz_id,
        url: format!("/data/{data_bag}/{}", item.item_name),
        output: wrapper.clone(),
        fields,
        partial_sources: vec![raw, wrapper],
    })
}

pub(crate) fn unknown_index(index: &str) -> ApiError {
    ApiError::not_found(format!(
        "I don't know how to search for {index} data objects."
    ))
}

// ---------------------------------------------------------------------------
// Mutation hooks
// ---------------------------------------------------------------------------

/// Best-effort index update for a node. Indexing failures are logged rather
/// than failing the write, matching the upstream asynchronous indexer.
pub(crate) async fn index_node(state: &AppState, org: &Org, node: &StoredNode) {
    match node_document(&org.id, node) {
        Ok(document) => log_error(
            state.search.index(&document).await.map(|_| ()),
            "node index",
        ),
        Err(error) => tracing::warn!(%error.message, "node index document"),
    }
}

pub(crate) async fn index_object(
    state: &AppState,
    org: &Org,
    kind: IndexKind,
    object: &StoredObject,
) {
    match object_document(&org.id, kind, object) {
        Ok(document) => log_error(
            state.search.index(&document).await.map(|_| ()),
            "object index",
        ),
        Err(error) => tracing::warn!(%error.message, "object index document"),
    }
}

pub(crate) async fn index_client(state: &AppState, org: &Org, client: &Client) {
    let public_key = state
        .store
        .find_actor_keys(&client.id, ActorType::Client)
        .await
        .ok()
        .and_then(|keys| {
            keys.iter()
                .find(|key| key.key_name == "default")
                .or_else(|| keys.first())
                .map(|key| key.public_key.clone())
        });
    let document = client_document(&org.id, &org.name, client, public_key.as_deref());
    log_error(
        state.search.index(&document).await.map(|_| ()),
        "client index",
    );
}

pub(crate) async fn index_data_bag_item(state: &AppState, org: &Org, item: &StoredDataBagItem) {
    match data_bag_item_document(&org.id, item) {
        Ok(document) => log_error(
            state.search.index(&document).await.map(|_| ()),
            "data bag index",
        ),
        Err(error) => tracing::warn!(%error.message, "data bag index document"),
    }
}

pub(crate) async fn delete_document(state: &AppState, org_id: &str, id: &str) {
    log_error(state.search.delete(org_id, id).await, "search delete");
}

pub(crate) async fn delete_organization(state: &AppState, org_id: &str) {
    log_error(
        state.search.delete_index(org_id).await,
        "search delete index",
    );
}

fn log_error(result: Result<(), crate::search::SearchError>, operation: &str) {
    if let Err(error) = result {
        tracing::warn!(%error, operation, "search backend update failed");
    }
}
