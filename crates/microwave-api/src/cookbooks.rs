use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Org, Permission, StoredCookbookVersion};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

pub(crate) const SEGMENTS: [&str; 9] = [
    "recipes",
    "definitions",
    "libraries",
    "attributes",
    "files",
    "templates",
    "resources",
    "providers",
    "root_files",
];

const V0_KEYS: [&str; 16] = [
    "attributes",
    "chef_type",
    "cookbook_name",
    "definitions",
    "files",
    "frozen?",
    "json_class",
    "libraries",
    "metadata",
    "name",
    "providers",
    "recipes",
    "resources",
    "root_files",
    "templates",
    "version",
];

const V2_KEYS: [&str; 8] = [
    "all_files",
    "chef_type",
    "cookbook_name",
    "frozen?",
    "json_class",
    "metadata",
    "name",
    "version",
];

#[derive(Debug, Clone, Copy)]
pub(crate) enum NumVersions {
    All,
    Count(usize),
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/cookbooks",
            get(list_cookbooks),
        )
        .route(
            "/organizations/{organization}/cookbooks/{cookbook}",
            get(get_cookbook),
        )
        .route(
            "/organizations/{organization}/cookbooks/{cookbook}/{version}",
            get(get_cookbook_version)
                .put(put_cookbook_version)
                .delete(delete_cookbook_version),
        )
        .route("/organizations/{organization}/universe", get(universe))
}

async fn list_cookbooks(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;
    let num_versions = parse_num_versions(query.get("num_versions"), NumVersions::Count(1))?;
    Ok(Json(
        cookbook_map(&state, org, &headers, &num_versions, None).await?,
    ))
}

async fn get_cookbook(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;

    if name == "_latest" {
        return Ok(Json(latest_cookbook_map(&state, org, &headers).await?));
    }
    if name == "_recipes" {
        return Ok(Json(Value::Array(recipes_from_latest(&state, org).await?)));
    }

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let versions: Vec<_> = versions
        .into_iter()
        .filter(|version| version.name == name)
        .collect();
    if versions.is_empty() {
        return Err(cookbook_not_found(&name));
    }
    let mut versions = versions;
    // Upstream lists cookbook versions newest-first (`ORDER BY name, major
    // DESC, minor DESC, patch DESC`), so 2.0.0 sorts before 1.10.0, which sorts
    // before 1.2.3.
    sort_versions_descending(&mut versions);
    let base = base_url(&headers, &state);
    let mut result = Map::new();
    result.insert(name.clone(), cookbook_entry(&base, org, &name, &versions));
    Ok(Json(Value::Object(result)))
}

async fn get_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let cookbook = resolve_version(&state, org, &name, &version)
        .await?
        .ok_or_else(|| cookbook_version_not_found(&name, &version))?;
    require_permission(&state, &auth, &cookbook.authz_id, Permission::Read).await?;

    let manifest = render_manifest(&cookbook.manifest, api_version, Some(&state), &org.id)?;
    Ok(Json(manifest))
}

async fn put_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, version)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
    Json(manifest): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    validate_cookbook_name(&name)?;
    validate_version(&version)?;

    let normalized = normalize_cookbook_document(&manifest, &name, &version, api_version)?;
    let existing = state
        .store
        .find_cookbook_version(&org.id, &name, &version)
        .await
        .map_err(ApiError::from)?;

    if let Some(existing) = existing {
        require_permission(&state, &auth, &existing.authz_id, Permission::Update).await?;
        let force = query.get("force").is_some_and(|value| value != "false");
        if existing.frozen && !force {
            return Err(ApiError::conflict(frozen_message(&name, &version)));
        }
        validate_checksums(
            &state,
            org,
            &normalized.checksums,
            ChecksumErrorStyle::Update,
        )
        .await?;

        let frozen = existing.frozen || normalized.frozen;
        let mut response = normalized.response;
        response["frozen?"] = json!(frozen);
        let mut stored = normalized.stored;
        stored["frozen?"] = json!(frozen);

        let (_, removed) = state
            .store
            .update_cookbook_version(
                &org.id,
                &name,
                &version,
                &serde_json::to_string(&stored).map_err(|_| ApiError::internal())?,
                &normalized.checksums,
                frozen,
                &auth.principal.authz_id,
            )
            .await
            .map_err(ApiError::from)?;
        delete_blobs(&state, &org.id, &removed);
        Ok((StatusCode::OK, Json(response)))
    } else {
        let container = container_authz(&state, org, "cookbooks").await?;
        require_permission(&state, &auth, &container, Permission::Create).await?;
        validate_checksums(
            &state,
            org,
            &normalized.checksums,
            ChecksumErrorStyle::Create,
        )
        .await?;

        state
            .store
            .create_cookbook_version(
                &org.id,
                &name,
                &version,
                &serde_json::to_string(&normalized.stored).map_err(|_| ApiError::internal())?,
                &normalized.checksums,
                normalized.frozen,
                &auth.principal.authz_id,
            )
            .await
            .map_err(ApiError::from)?;

        Ok((StatusCode::CREATED, Json(normalized.response)))
    }
}

async fn delete_cookbook_version(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, name, version)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let cookbook = resolve_version(&state, org, &name, &version)
        .await?
        .ok_or_else(|| cookbook_version_not_found(&name, &version))?;
    require_permission(&state, &auth, &cookbook.authz_id, Permission::Delete).await?;

    let (deleted, removed) = state
        .store
        .delete_cookbook_version(&org.id, &name, &cookbook.version)
        .await
        .map_err(ApiError::from)?;
    delete_blobs(&state, &org.id, &removed);

    let manifest = render_manifest(&deleted.manifest, api_version, Some(&state), &org.id)?;
    Ok(Json(manifest))
}

async fn universe(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;

    let base = base_url(&headers, &state);
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let mut cookbooks: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for version in versions {
        let manifest: Value =
            serde_json::from_str(&version.manifest).map_err(|_| ApiError::internal())?;
        let dependencies = manifest
            .pointer("/metadata/dependencies")
            .and_then(Value::as_object)
            .map(|dependencies| {
                dependencies
                    .iter()
                    .filter_map(|(name, constraint)| {
                        constraint
                            .as_str()
                            .map(|constraint| (name.clone(), json!(constraint)))
                    })
                    .collect::<Map<String, Value>>()
            })
            .unwrap_or_default();
        cookbooks.entry(version.name.clone()).or_default().insert(
            version.version.clone(),
            json!({
                "dependencies": Value::Object(dependencies),
                "location_path": format!(
                    "{base}/organizations/{}/cookbooks/{}/{}",
                    org.name, version.name, version.version
                ),
                "location_type": "chef_server",
            }),
        );
    }

    let mut result = Map::new();
    for (name, versions) in cookbooks {
        let versions: Map<String, Value> = versions.into_iter().collect();
        result.insert(name, Value::Object(versions));
    }
    Ok(Json(Value::Object(result)))
}

struct NormalizedCookbook {
    response: Value,
    stored: Value,
    checksums: Vec<String>,
    frozen: bool,
}

fn normalize_cookbook_document(
    manifest: &Value,
    url_name: &str,
    url_version: &str,
    api_version: u8,
) -> Result<NormalizedCookbook, ApiError> {
    let mut document = manifest
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;

    let valid_keys: &[&str] = if api_version >= 2 { &V2_KEYS } else { &V0_KEYS };
    for key in document.keys() {
        if !valid_keys.contains(&key.as_str()) {
            return Err(ApiError::bad_request(format!(
                "Invalid key {key} in request body"
            )));
        }
    }

    set_default(&mut document, "json_class", json!("Chef::CookbookVersion"));
    set_default(&mut document, "chef_type", json!("cookbook_version"));
    set_default(&mut document, "frozen?", json!(false));

    let name = validate_string(&document, "name", ValueKind::Name)?;
    let expected_name = format!("{url_name}-{url_version}");
    if name != expected_name {
        return Err(field_invalid("name"));
    }
    let cookbook_name = validate_string(&document, "cookbook_name", ValueKind::Name)?;
    if cookbook_name != url_name {
        return Err(field_invalid("cookbook_name"));
    }
    validate_exact(&document, "json_class", "Chef::CookbookVersion")?;
    validate_exact(&document, "chef_type", "cookbook_version")?;
    let frozen = document
        .get("frozen?")
        .and_then(Value::as_bool)
        .ok_or_else(|| field_invalid("frozen?"))?;

    let metadata = document
        .get("metadata")
        .ok_or_else(|| field_missing("metadata"))?
        .as_object()
        .ok_or_else(|| field_invalid("metadata"))?;
    let metadata_version = metadata
        .get("version")
        .ok_or_else(|| field_missing("metadata.version"))?
        .as_str()
        .ok_or_else(|| field_invalid("metadata.version"))?;
    parse_version_value(metadata_version).map_err(|_| field_invalid("metadata.version"))?;
    if metadata_version != url_version {
        return Err(field_invalid("metadata.version"));
    }

    if let Some(value) = metadata.get("name") {
        match value.as_str() {
            Some(value) if valid_object_name(value) => {}
            _ => return Err(field_invalid("metadata.name")),
        }
    }
    for key in [
        "description",
        "long_description",
        "maintainer",
        "maintainer_email",
        "license",
    ] {
        if let Some(value) = metadata.get(key) {
            if !value.is_string() {
                return Err(field_invalid(&format!("metadata.{key}")));
            }
        }
    }
    for key in [
        "platforms",
        "dependencies",
        "recommendations",
        "suggestions",
        "conflicting",
        "replacing",
    ] {
        if let Some(value) = metadata.get(key) {
            validate_constraint_map(value, &format!("metadata.{key}"))?;
        }
    }

    let mut checksums = Vec::new();
    if api_version >= 2 {
        if let Some(value) = document.get("all_files") {
            checksums.extend(validate_file_list(value, "all_files")?);
        }
    } else {
        for segment in SEGMENTS {
            if let Some(value) = document.get(segment) {
                checksums.extend(validate_file_list(value, segment)?);
            }
        }
    }

    if let Some(value) = document.get("version") {
        let version = value.as_str().ok_or_else(|| field_invalid("version"))?;
        parse_version_value(version).map_err(|_| field_invalid("version"))?;
        if version != url_version {
            return Err(field_invalid("version"));
        }
    }

    checksums.sort();
    checksums.dedup();
    let response = Value::Object(document.clone());

    let metadata = document
        .get_mut("metadata")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| field_invalid("metadata"))?;
    set_default(metadata, "attributes", json!({}));
    set_default(metadata, "dependencies", json!({}));
    set_default(metadata, "long_description", json!(""));

    Ok(NormalizedCookbook {
        response,
        stored: Value::Object(document),
        checksums,
        frozen,
    })
}

#[derive(Debug, Clone, Copy)]
enum ValueKind {
    Name,
}

fn validate_string(
    document: &Map<String, Value>,
    key: &str,
    kind: ValueKind,
) -> Result<String, ApiError> {
    let value = document.get(key).ok_or_else(|| field_missing(key))?;
    let value = value.as_str().ok_or_else(|| field_invalid(key))?;
    match kind {
        ValueKind::Name if !valid_object_name(value) => Err(field_invalid(key)),
        _ => Ok(value.to_string()),
    }
}

fn validate_exact(
    document: &Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ApiError> {
    match document.get(key).and_then(Value::as_str) {
        Some(value) if value == expected => Ok(()),
        _ => Err(field_invalid(key)),
    }
}

fn validate_constraint_map(value: &Value, key: &str) -> Result<(), ApiError> {
    let values = value.as_object().ok_or_else(|| field_invalid(key))?;
    for (name, constraint) in values {
        if !valid_object_name(name) {
            return Err(field_invalid(key));
        }
        match constraint.as_str() {
            Some(constraint) if microwave_depsolver::Constraint::parse(constraint).is_ok() => {}
            _ => {
                return Err(ApiError::bad_request(format!(
                    "Invalid value '{}' for {key}",
                    display_metadata_value(constraint)
                )));
            }
        }
    }
    Ok(())
}

fn display_metadata_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Object(map) if map.is_empty() => "{[]}".to_string(),
        other => other.to_string(),
    }
}

fn validate_file_list(value: &Value, key: &str) -> Result<Vec<String>, ApiError> {
    let files = value.as_array().ok_or_else(|| field_invalid(key))?;
    let mut checksums = Vec::new();
    for file in files {
        let file = file.as_object().ok_or_else(|| invalid_array_element(key))?;
        let mut checksum = None;
        for field in ["name", "path", "checksum", "specificity"] {
            match file.get(field).and_then(Value::as_str) {
                Some(value) if field == "checksum" => checksum = Some(value.to_string()),
                Some(_) => {}
                None => return Err(invalid_array_element(key)),
            }
        }
        if let Some(checksum) = checksum {
            checksums.push(checksum);
        }
    }
    Ok(checksums)
}

enum ChecksumErrorStyle {
    Create,
    Update,
}

async fn validate_checksums(
    state: &AppState,
    org: &Org,
    checksums: &[String],
    style: ChecksumErrorStyle,
) -> Result<(), ApiError> {
    for checksum in checksums {
        if !state
            .store
            .checksum_exists(&org.id, checksum)
            .await
            .map_err(ApiError::from)?
        {
            return Err(match style {
                ChecksumErrorStyle::Create => {
                    ApiError::bad_request("Manifest has a checksum that hasn't been uploaded.")
                }
                ChecksumErrorStyle::Update => ApiError::bad_request(format!(
                    "Manifest has checksum {checksum} but it hasn't yet been uploaded"
                )),
            });
        }
    }
    Ok(())
}

async fn resolve_version(
    state: &AppState,
    org: &Org,
    name: &str,
    version: &str,
) -> Result<Option<StoredCookbookVersion>, ApiError> {
    if version == "_latest" || version == "latest" {
        let versions = state
            .store
            .list_cookbook_versions(&org.id)
            .await
            .map_err(ApiError::from)?;
        let mut versions: Vec<_> = versions
            .into_iter()
            .filter(|cookbook| cookbook.name == name)
            .collect();
        sort_versions_descending(&mut versions);
        return Ok(versions.into_iter().next());
    }
    validate_version(version)?;
    state
        .store
        .find_cookbook_version(&org.id, name, version)
        .await
        .map_err(ApiError::from)
}

async fn cookbook_map(
    state: &AppState,
    org: &Org,
    headers: &HeaderMap,
    num_versions: &NumVersions,
    only: Option<&str>,
) -> Result<Value, ApiError> {
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let base = base_url(headers, state);
    let mut names: Vec<String> = versions
        .iter()
        .map(|version| version.name.clone())
        .filter(|name| only.is_none_or(|only| name == only))
        .collect();
    names.sort();
    names.dedup();

    let mut map = Map::new();
    for name in names {
        let mut selected: Vec<_> = versions
            .iter()
            .filter(|version| version.name == name)
            .cloned()
            .collect();
        sort_versions_descending(&mut selected);
        let selected = select_versions(selected, num_versions);
        map.insert(name.clone(), cookbook_entry(&base, org, &name, &selected));
    }
    Ok(Value::Object(map))
}

async fn latest_cookbook_map(
    state: &AppState,
    org: &Org,
    headers: &HeaderMap,
) -> Result<Value, ApiError> {
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let base = base_url(headers, state);
    let mut latest: BTreeMap<String, StoredCookbookVersion> = BTreeMap::new();
    for version in versions {
        latest
            .entry(version.name.clone())
            .and_modify(|current| {
                if version_key(&version.version) > version_key(&current.version) {
                    *current = version.clone();
                }
            })
            .or_insert(version);
    }
    let mut map = Map::new();
    for (name, version) in latest {
        map.insert(
            name.clone(),
            json!(format!(
                "{base}/organizations/{}/cookbooks/{name}/{}",
                org.name, version.version
            )),
        );
    }
    Ok(Value::Object(map))
}

async fn recipes_from_latest(state: &AppState, org: &Org) -> Result<Vec<Value>, ApiError> {
    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let mut latest: BTreeMap<String, StoredCookbookVersion> = BTreeMap::new();
    for version in versions {
        latest
            .entry(version.name.clone())
            .and_modify(|current| {
                if version_key(&version.version) > version_key(&current.version) {
                    *current = version.clone();
                }
            })
            .or_insert(version);
    }
    let mut recipes = Vec::new();
    for (name, version) in latest {
        let manifest: Value =
            serde_json::from_str(&version.manifest).map_err(|_| ApiError::internal())?;
        recipes.extend(
            recipe_names(&name, &manifest)
                .into_iter()
                .map(Value::String),
        );
    }
    Ok(recipes)
}

pub(crate) fn recipe_names(cookbook_name: &str, manifest: &Value) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(files) = manifest.get("all_files").and_then(Value::as_array) {
        for file in files {
            if let Some(name) = file.get("name").and_then(Value::as_str) {
                if let Some(name) = name.strip_prefix("recipes/") {
                    names.push(qualify_recipe(cookbook_name, name));
                }
            }
        }
    } else if let Some(recipes) = manifest.get("recipes").and_then(Value::as_array) {
        for recipe in recipes {
            if let Some(name) = recipe.get("name").and_then(Value::as_str) {
                names.push(qualify_recipe(cookbook_name, name));
            }
        }
    }
    names.sort();
    names
}

fn qualify_recipe(cookbook_name: &str, recipe: &str) -> String {
    // Upstream uses `filename:basename/2`, so nested recipe paths collapse to
    // their final component before the `.rb` suffix is removed.
    let name = recipe.rsplit('/').next().unwrap_or(recipe);
    let name = name.strip_suffix(".rb").unwrap_or(name);
    if name == "default" {
        cookbook_name.to_string()
    } else {
        format!("{cookbook_name}::{name}")
    }
}

fn cookbook_entry(base: &str, org: &Org, name: &str, versions: &[StoredCookbookVersion]) -> Value {
    let versions: Vec<Value> = versions
        .iter()
        .map(|version| {
            json!({
                "version": version.version,
                "url": format!("{base}/organizations/{}/cookbooks/{name}/{}", org.name, version.version)
            })
        })
        .collect();
    json!({
        "url": format!("{base}/organizations/{}/cookbooks/{name}", org.name),
        "versions": versions
    })
}

fn select_versions(
    versions: Vec<StoredCookbookVersion>,
    num_versions: &NumVersions,
) -> Vec<StoredCookbookVersion> {
    match num_versions {
        NumVersions::All => versions,
        NumVersions::Count(count) => versions.into_iter().take(*count).collect(),
    }
}

pub(crate) fn parse_num_versions(
    value: Option<&String>,
    default: NumVersions,
) -> Result<NumVersions, ApiError> {
    let Some(value) = value else {
        return Ok(default);
    };
    if value == "all" {
        return Ok(NumVersions::All);
    }
    value
        .parse::<usize>()
        .map(NumVersions::Count)
        .map_err(|_| invalid_num_versions())
}

fn invalid_num_versions() -> ApiError {
    ApiError::bad_request("You have requested an invalid number of versions (x >= 0 || 'all')")
}

fn sort_versions_descending(versions: &mut [StoredCookbookVersion]) {
    versions.sort_by_key(|version| std::cmp::Reverse(version_key(&version.version)));
}

fn version_key(version: &str) -> (i64, i64, i64) {
    microwave_store::parse_version(version).unwrap_or((0, 0, 0))
}

fn validate_cookbook_name(name: &str) -> Result<(), ApiError> {
    if valid_object_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!(
            "Invalid cookbook name '{name}' using regex: 'Malformed cookbook name. Must only contain A-Z, a-z, 0-9, _, . or -'."
        )))
    }
}

fn validate_version(version: &str) -> Result<(), ApiError> {
    parse_version_value(version)
        .map(|_| ())
        .map_err(|_| ApiError::bad_request(format!("Invalid cookbook version '{version}'.")))
}

fn parse_version_value(version: &str) -> Result<(i64, i64, i64), ()> {
    microwave_store::parse_version(version).map_err(|_| ())
}

fn valid_object_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
}

fn set_default(document: &mut Map<String, Value>, key: &str, value: Value) {
    if !document.contains_key(key) {
        document.insert(key.to_string(), value);
    }
}

fn field_missing(key: &str) -> ApiError {
    ApiError::bad_request(format!("Field '{key}' missing"))
}

fn field_invalid(key: &str) -> ApiError {
    ApiError::bad_request(format!("Field '{key}' invalid"))
}

fn invalid_array_element(key: &str) -> ApiError {
    ApiError::bad_request(format!("Invalid element in array value of '{key}'."))
}

fn cookbook_not_found(name: &str) -> ApiError {
    ApiError::not_found(format!("Cannot find a cookbook named {name}"))
}

fn cookbook_version_not_found(name: &str, version: &str) -> ApiError {
    ApiError::not_found(format!(
        "Cannot find a cookbook named {name} with version {version}"
    ))
}

fn frozen_message(name: &str, version: &str) -> String {
    format!(
        "The cookbook {name} at version {version} is frozen. Use the 'force' option to override."
    )
}

pub(crate) fn manifest_checksums(manifest: &Value) -> Vec<String> {
    let mut checksums = Vec::new();
    if let Some(files) = manifest.get("all_files").and_then(Value::as_array) {
        for file in files {
            if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
                checksums.push(checksum.to_string());
            }
        }
    }
    for segment in SEGMENTS {
        if let Some(files) = manifest.get(segment).and_then(Value::as_array) {
            for file in files {
                if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
                    checksums.push(checksum.to_string());
                }
            }
        }
    }
    checksums.sort();
    checksums.dedup();
    checksums
}

/// Render a stored manifest for the requested API version.
pub(crate) fn render_manifest(
    manifest: &str,
    api_version: u8,
    state: Option<&AppState>,
    org_id: &str,
) -> Result<Value, ApiError> {
    let mut document: Value = serde_json::from_str(manifest).map_err(|_| ApiError::internal())?;
    convert_manifest(&mut document, api_version);
    if let Some(state) = state {
        add_download_urls(&mut document, state, org_id);
    }
    Ok(document)
}

fn convert_manifest(document: &mut Value, api_version: u8) {
    if api_version >= 2 {
        if document.get("all_files").is_none() {
            let mut all_files = Vec::new();
            for segment in SEGMENTS {
                if let Some(files) = document.get(segment).and_then(Value::as_array) {
                    for file in files {
                        let mut file = file.clone();
                        let name = file
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if let Some(object) = file.as_object_mut() {
                            object.insert("name".to_string(), json!(format!("{segment}/{name}")));
                        }
                        all_files.push(file);
                    }
                }
            }
            if let Some(object) = document.as_object_mut() {
                for segment in SEGMENTS {
                    object.remove(segment);
                }
                object.insert("all_files".to_string(), Value::Array(all_files));
            }
        }
    } else if let Some(all_files) = document.get("all_files").and_then(Value::as_array).cloned() {
        if let Some(object) = document.as_object_mut() {
            object.remove("all_files");
            for segment in SEGMENTS {
                object.insert(segment.to_string(), Value::Array(Vec::new()));
            }
        }
        for file in all_files {
            let Some(name) = file.get("name").and_then(Value::as_str) else {
                continue;
            };
            let (segment, filename) = match name.split_once('/') {
                Some((segment, rest)) => {
                    (segment.to_string(), rest.rsplit('/').next().unwrap_or(rest))
                }
                None => ("root_files".to_string(), name),
            };
            let mut file = file.clone();
            if let Some(object) = file.as_object_mut() {
                object.insert("name".to_string(), json!(filename));
            }
            if let Some(object) = document.as_object_mut() {
                if let Some(values) = object.get_mut(&segment).and_then(Value::as_array_mut) {
                    values.push(file);
                }
            }
        }
    }
}

pub(crate) fn add_download_urls(manifest: &mut Value, state: &AppState, org_id: &str) {
    if let Some(files) = manifest.get_mut("all_files").and_then(Value::as_array_mut) {
        annotate_files(files, state, org_id);
        return;
    }
    for segment in SEGMENTS {
        if let Some(files) = manifest.get_mut(segment).and_then(Value::as_array_mut) {
            annotate_files(files, state, org_id);
        }
    }
}

fn annotate_files(files: &mut [Value], state: &AppState, org_id: &str) {
    for file in files {
        if let Some(checksum) = file.get("checksum").and_then(Value::as_str) {
            let url = state.object_store.presigned_url("GET", org_id, checksum);
            file["url"] = json!(url);
        }
    }
}

pub(crate) fn delete_blobs(state: &AppState, org_id: &str, checksums: &[String]) {
    for checksum in checksums {
        let _ = state.object_store.delete(org_id, checksum);
    }
}
