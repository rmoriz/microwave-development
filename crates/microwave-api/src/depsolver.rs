use crate::cookbooks::render_manifest;
use crate::helpers::{container_authz, is_superuser, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Extension, Json, Router};
use microwave_depsolver::{Constraint, Goal, SolveError, SolveOptions, Version, World};
use microwave_store::{Org, Permission, StoredCookbookVersion, StoredObject};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/organizations/{organization}/environments/{environment}/cookbook_versions",
        post(solve),
    )
}

async fn solve(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, environment_name)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = json_body(body)?;
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "cookbooks").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;

    let environment = state
        .store
        .find_environment(&org.id, &environment_name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| {
            ApiError::not_found(format!("environment '{environment_name}' not found"))
        })?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Read).await?;

    let run_list = match body.get("run_list") {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err(invalid_run_list()),
    };
    let goals = resolve_run_list(&state, &auth, org, &environment_name, &run_list).await?;
    let environment_constraints = environment_constraints(&environment)?;

    let versions = state
        .store
        .list_cookbook_versions(&org.id)
        .await
        .map_err(ApiError::from)?;
    let world = cookbook_world(&versions)?;

    match world.solve(&goals, &environment_constraints, &SolveOptions::default()) {
        Ok(solution) => {
            let stored_versions = versions
                .iter()
                .map(|version| ((version.name.clone(), version.version.clone()), version))
                .collect::<BTreeMap<_, _>>();
            let mut result = Map::new();
            let mut unauthorized = Vec::new();
            for package in solution.packages {
                let key = (package.name.clone(), package.version.to_string());
                let stored = stored_versions.get(&key).ok_or_else(ApiError::internal)?;
                if !is_superuser(&auth)
                    && !state
                        .store
                        .check_permission(
                            &auth.principal.authz_id,
                            &stored.authz_id,
                            Permission::Read,
                        )
                        .await
                        .map_err(ApiError::from)?
                {
                    unauthorized.push(json!({
                        "name": package.name,
                        "version": package.version.to_string()
                    }));
                    continue;
                }
                let mut manifest =
                    render_manifest(&stored.manifest, api_version, Some(&state), &org.id)?;
                strip_minimal_metadata(&mut manifest);
                result.insert(package.name, manifest);
            }
            if !unauthorized.is_empty() {
                return Ok((
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "error": ["Read permission is not granted for one or more cookbooks"],
                        "unauthorized_cookbooks": unauthorized
                    })),
                ));
            }
            Ok((StatusCode::OK, Json(Value::Object(result))))
        }
        Err(error) => Ok(solve_error(error)),
    }
}

/// Translate axum's plain-text `Json` rejections into the Chef API's JSON
/// error shape, which clients (and Pedant) parse.
fn json_body(body: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    match body {
        Ok(Json(value)) => Ok(value),
        Err(rejection) => match rejection {
            JsonRejection::JsonSyntaxError(_)
            | JsonRejection::JsonDataError(_)
            | JsonRejection::MissingJsonContentType(_) => {
                Err(ApiError::bad_request("invalid JSON"))
            }
            rejection => Err(ApiError::bad_request(rejection.body_text())),
        },
    }
}

fn solve_error(error: SolveError) -> (StatusCode, Json<Value>) {
    let detail = match error {
        SolveError::InvalidConstraints(error) => {
            let message = error.message();
            json!({
                "message": message,
                "non_existent_cookbooks": error.non_existent_cookbooks,
                "cookbooks_with_no_versions": error.cookbooks_with_no_versions,
            })
        }
        SolveError::NoSolution(error) => json!({
            "message": error.message,
            "unsatisfiable_run_list_item": error.unsatisfiable_run_list_item,
            "non_existent_cookbooks": error.non_existent_cookbooks,
            "most_constrained_cookbooks": error.most_constrained_cookbooks,
        }),
        SolveError::Timeout => json!({
            "message": "unable to solve dependencies in alotted time",
            "non_existent_cookbooks": [],
            "most_constrained_cookbooks": [],
        }),
    };
    (
        StatusCode::PRECONDITION_FAILED,
        Json(json!({ "error": [detail] })),
    )
}

fn strip_minimal_metadata(manifest: &mut Value) {
    if let Some(metadata) = manifest.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("attributes");
        metadata.remove("long_description");
    }
}

fn cookbook_world(versions: &[StoredCookbookVersion]) -> Result<World, ApiError> {
    let mut world = World::new();
    for stored in versions {
        let manifest: Value =
            serde_json::from_str(&stored.manifest).map_err(|_| ApiError::internal())?;
        let mut dependencies = Vec::new();
        if let Some(object) = manifest
            .pointer("/metadata/dependencies")
            .and_then(Value::as_object)
        {
            for (name, value) in object {
                let text = value.as_str().ok_or_else(ApiError::internal)?;
                let constraint = Constraint::parse(text).map_err(|_| ApiError::internal())?;
                dependencies.push((name.clone(), constraint));
            }
        }
        let version = Version::parse(&stored.version).map_err(|_| ApiError::internal())?;
        world.insert(stored.name.clone(), version, dependencies);
    }
    Ok(world)
}

fn environment_constraints(
    environment: &StoredObject,
) -> Result<BTreeMap<String, Constraint>, ApiError> {
    let value: Value =
        serde_json::from_str(&environment.serialized_object).map_err(|_| ApiError::internal())?;
    let Some(object) = value.get("cookbook_versions").and_then(Value::as_object) else {
        return Ok(BTreeMap::new());
    };

    let mut constraints = BTreeMap::new();
    for (name, value) in object {
        let text = value.as_str().ok_or_else(ApiError::internal)?;
        let constraint = Constraint::parse(text).map_err(|_| ApiError::internal())?;
        constraints.insert(name.clone(), constraint);
    }
    Ok(constraints)
}

async fn resolve_run_list(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    environment_name: &str,
    run_list: &[Value],
) -> Result<Vec<Goal>, ApiError> {
    let mut targets = Vec::new();
    for item in run_list {
        let item = item.as_str().ok_or_else(invalid_run_list)?;
        collect_run_list_item(
            state,
            auth,
            org,
            environment_name,
            item,
            &mut BTreeSet::new(),
            &mut targets,
            0,
        )
        .await?;
    }

    let mut seen = BTreeSet::new();
    Ok(targets
        .into_iter()
        .filter(|(name, _)| seen.insert(name.clone()))
        .map(|(name, constraint)| Goal { name, constraint })
        .collect())
}

#[allow(clippy::too_many_arguments)]
async fn collect_run_list_item(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    environment_name: &str,
    item: &str,
    visited_roles: &mut BTreeSet<String>,
    targets: &mut Vec<(String, Constraint)>,
    depth: usize,
) -> Result<(), ApiError> {
    if depth > 100 {
        return Err(ApiError::bad_request("run list role nesting is too deep"));
    }

    match parse_run_list_item(item)? {
        RunListItem::Recipe { name, version } => {
            let constraint = version
                .map(Constraint::exact)
                .unwrap_or_else(Constraint::any);
            targets.push((name, constraint));
        }
        RunListItem::Role(name) => {
            if !visited_roles.insert(name.clone()) {
                return Ok(());
            }
            let role = state
                .store
                .find_role(&org.id, &name)
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::not_found(format!("role {name}")))?;
            require_permission(state, auth, &role.authz_id, Permission::Read).await?;
            let value: Value =
                serde_json::from_str(&role.serialized_object).map_err(|_| ApiError::internal())?;
            for entry in role_run_list(&value, environment_name)? {
                Box::pin(collect_run_list_item(
                    state,
                    auth,
                    org,
                    environment_name,
                    entry,
                    visited_roles,
                    targets,
                    depth + 1,
                ))
                .await?;
            }
            visited_roles.remove(&name);
        }
    }
    Ok(())
}

fn role_run_list<'a>(role: &'a Value, environment_name: &str) -> Result<Vec<&'a str>, ApiError> {
    let run_list = role
        .get("env_run_lists")
        .and_then(Value::as_object)
        .and_then(|lists| lists.get(environment_name))
        .or_else(|| role.get("run_list"));

    let Some(run_list) = run_list else {
        return Ok(Vec::new());
    };
    let Some(items) = run_list.as_array() else {
        return Err(ApiError::internal());
    };
    items
        .iter()
        .map(|item| item.as_str().ok_or_else(invalid_run_list))
        .collect()
}

enum RunListItem {
    Recipe {
        name: String,
        version: Option<Version>,
    },
    Role(String),
}

fn parse_run_list_item(item: &str) -> Result<RunListItem, ApiError> {
    let trimmed = item.trim();
    if trimmed.is_empty() {
        return Err(invalid_run_list());
    }

    if let Some(inner) = trimmed
        .strip_prefix("recipe[")
        .and_then(|rest| rest.strip_suffix(']'))
    {
        let (name, version) = parse_recipe(inner)?;
        return Ok(RunListItem::Recipe { name, version });
    }

    if let Some(inner) = trimmed
        .strip_prefix("role[")
        .and_then(|rest| rest.strip_suffix(']'))
    {
        if inner.is_empty() || inner.contains('[') || inner.contains(']') || inner.contains('@') {
            return Err(invalid_run_list());
        }
        return Ok(RunListItem::Role(inner.to_string()));
    }

    if trimmed.contains('[') || trimmed.contains(']') {
        return Err(invalid_run_list());
    }

    let (name, version) = parse_recipe(trimmed)?;
    Ok(RunListItem::Recipe { name, version })
}

fn parse_recipe(input: &str) -> Result<(String, Option<Version>), ApiError> {
    let (recipe, version) = match input.rsplit_once('@') {
        Some((recipe, version)) => {
            if recipe.is_empty() || version.is_empty() || recipe.contains('@') {
                return Err(invalid_run_list());
            }
            let version = Version::parse(version).map_err(|_| invalid_run_list())?;
            (recipe, Some(version))
        }
        None => (input, None),
    };

    let name = recipe.split("::").next().unwrap_or(recipe);
    if name.is_empty() {
        return Err(invalid_run_list());
    }
    Ok((name.to_string(), version))
}

fn invalid_run_list() -> ApiError {
    ApiError::bad_request("Field 'run_list' is not a valid run list")
}
