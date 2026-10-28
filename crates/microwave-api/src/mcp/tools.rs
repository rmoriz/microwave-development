use crate::cookbooks::{render_manifest, resolve_version, sort_versions_descending};
use crate::mcp::context::McpContext;
use crate::mcp::cookbook_upload::{build_manifest, UploadCookbookArgs};
use crate::ApiError;
use axum::http::StatusCode;
use chrono::DateTime;
use microwave_store::{NodeRunFilter, StoredCookbookVersion, StoredNode, StoredNodeRun};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::Instant;

/// MCP server bound to one authenticated organization principal.
#[derive(Clone)]
pub struct McpServer {
    pub(crate) ctx: McpContext,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    pub fn new(ctx: McpContext) -> Self {
        let mut tool_router = Self::read_tool_router();
        if ctx.writes {
            tool_router += Self::write_tool_router();
        }
        Self { ctx, tool_router }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListNodesArgs {
    query: Option<String>,
    environment: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetNodeArgs {
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListEnvironmentsArgs {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetEnvironmentArgs {
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListCookbooksArgs {
    query: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CookbookNameArgs {
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CookbookVersionArgs {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListRunsArgs {
    node_name: Option<String>,
    status: Option<String>,
    failures_only: Option<bool>,
    since: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LatestRunArgs {
    node_name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRunArgs {
    node_name: String,
    run_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ListConvergeErrorsArgs {
    node_name: Option<String>,
    since: Option<String>,
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateNodeArgs {
    name: String,
    environment: Option<String>,
    run_list: Option<Vec<String>>,
    normal: Option<Value>,
    policy_name: Option<String>,
    policy_group: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateNodeArgs {
    name: String,
    node: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeleteNodeArgs {
    name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CreateEnvironmentArgs {
    name: String,
    attributes: Option<Value>,
    cookbook_versions: Option<Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct UpdateEnvironmentArgs {
    name: String,
    environment: Value,
    new_name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeleteEnvironmentArgs {
    name: String,
}

#[tool_router(router = read_tool_router, vis = "pub(crate)")]
impl McpServer {
    /// List the nodes this principal may read.
    #[tool(
        description = "List nodes readable by the caller, optionally filtered by a name substring or environment"
    )]
    async fn list_nodes(
        &self,
        Parameters(args): Parameters<ListNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(&self.ctx, "list_nodes", None, self.list_nodes_inner(args)).await
    }

    /// Fetch one node document.
    #[tool(description = "Fetch a node by name")]
    async fn get_node(
        &self,
        Parameters(args): Parameters<GetNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(&self.ctx, "get_node", None, self.get_node_inner(args)).await
    }

    /// List the environments this principal may read.
    #[tool(description = "List environments readable by the caller")]
    async fn list_environments(
        &self,
        Parameters(args): Parameters<ListEnvironmentsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "list_environments",
            None,
            self.list_environments_inner(args),
        )
        .await
    }

    /// Fetch one environment document.
    #[tool(description = "Fetch an environment by name")]
    async fn get_environment(
        &self,
        Parameters(args): Parameters<GetEnvironmentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "get_environment",
            None,
            self.get_environment_inner(args),
        )
        .await
    }

    /// List cookbooks with the versions this principal may read.
    #[tool(description = "List cookbooks readable by the caller, with their versions")]
    async fn list_cookbooks(
        &self,
        Parameters(args): Parameters<ListCookbooksArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "list_cookbooks",
            None,
            self.list_cookbooks_inner(args),
        )
        .await
    }

    /// List the versions of one cookbook.
    #[tool(description = "List the versions of a cookbook readable by the caller")]
    async fn list_cookbook_versions(
        &self,
        Parameters(args): Parameters<CookbookNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "list_cookbook_versions",
            None,
            self.list_cookbook_versions_inner(args),
        )
        .await
    }

    /// Fetch one rendered cookbook manifest.
    #[tool(description = "Fetch a rendered cookbook manifest by name and version")]
    async fn get_cookbook_version(
        &self,
        Parameters(args): Parameters<CookbookVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "get_cookbook_version",
            None,
            self.get_cookbook_version_inner(args),
        )
        .await
    }

    /// List stored run reports, newest first.
    #[tool(description = "List stored run_converge reports for readable nodes, newest first")]
    async fn list_runs(
        &self,
        Parameters(args): Parameters<ListRunsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(&self.ctx, "list_runs", None, self.list_runs_inner(args)).await
    }

    /// Fetch the newest run report summary for one node.
    #[tool(description = "Fetch the newest run_converge summary for a node, including its errors")]
    async fn latest_run(
        &self,
        Parameters(args): Parameters<LatestRunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(&self.ctx, "latest_run", None, self.latest_run_inner(args)).await
    }

    /// Fetch one full run report.
    #[tool(description = "Fetch the full stored run_converge report for one node run")]
    async fn get_run(
        &self,
        Parameters(args): Parameters<GetRunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(&self.ctx, "get_run", None, self.get_run_inner(args)).await
    }

    /// List converge errors across stored run reports.
    #[tool(description = "List converge errors across readable node runs, newest first")]
    async fn list_converge_errors(
        &self,
        Parameters(args): Parameters<ListConvergeErrorsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        logged_tool(
            &self.ctx,
            "list_converge_errors",
            None,
            self.list_converge_errors_inner(args),
        )
        .await
    }
}

#[tool_router(router = write_tool_router, vis = "pub(crate)")]
impl McpServer {
    /// Create a node document.
    #[tool(
        description = "Create a node from a compact document; runs the same validation and ACL checks as the HTTP API"
    )]
    async fn create_node(
        &self,
        Parameters(args): Parameters<CreateNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "create_node",
            Some(&target),
            self.create_node_inner(args),
        )
        .await
    }

    /// Replace a node with a full document.
    #[tool(
        description = "Replace a node with a full document (PUT semantics, not JSON patch); the same normalization as the HTTP API applies"
    )]
    async fn update_node(
        &self,
        Parameters(args): Parameters<UpdateNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "update_node",
            Some(&target),
            self.update_node_inner(args),
        )
        .await
    }

    /// Delete a node.
    #[tool(description = "Delete a node by name")]
    async fn delete_node(
        &self,
        Parameters(args): Parameters<DeleteNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "delete_node",
            Some(&target),
            self.delete_node_inner(args),
        )
        .await
    }

    /// Create an environment document.
    #[tool(description = "Create an environment from attributes and cookbook version constraints")]
    async fn create_environment(
        &self,
        Parameters(args): Parameters<CreateEnvironmentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "create_environment",
            Some(&target),
            self.create_environment_inner(args),
        )
        .await
    }

    /// Replace an environment with a full document.
    #[tool(
        description = "Replace an environment with a full document (PUT semantics, not JSON patch); set new_name to rename it"
    )]
    async fn update_environment(
        &self,
        Parameters(args): Parameters<UpdateEnvironmentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "update_environment",
            Some(&target),
            self.update_environment_inner(args),
        )
        .await
    }

    /// Delete an environment.
    #[tool(description = "Delete an environment by name; the _default environment is protected")]
    async fn delete_environment(
        &self,
        Parameters(args): Parameters<DeleteEnvironmentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = args.name.clone();
        logged_tool(
            &self.ctx,
            "delete_environment",
            Some(&target),
            self.delete_environment_inner(args),
        )
        .await
    }

    /// Upload one cookbook version from inline files or known checksums.
    #[tool(
        description = "Upload a cookbook version from inline base64 files or already-uploaded checksums; runs the same validation, ACL, and frozen checks as the HTTP API"
    )]
    async fn upload_cookbook_version(
        &self,
        Parameters(args): Parameters<UploadCookbookArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = format!("{}/{}", args.name, args.version);
        logged_tool(
            &self.ctx,
            "upload_cookbook_version",
            Some(&target),
            self.upload_cookbook_version_inner(args),
        )
        .await
    }

    /// Delete one cookbook version.
    #[tool(description = "Delete a cookbook version by name and version")]
    async fn delete_cookbook_version(
        &self,
        Parameters(args): Parameters<CookbookVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let target = format!("{}/{}", args.name, args.version);
        logged_tool(
            &self.ctx,
            "delete_cookbook_version",
            Some(&target),
            self.delete_cookbook_version_inner(args),
        )
        .await
    }
}

impl McpServer {
    async fn create_node_inner(&self, args: CreateNodeArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let mut body = json!({ "name": args.name });
        let object = body.as_object_mut().ok_or_else(ApiError::internal)?;
        if let Some(environment) = args.environment {
            object.insert("chef_environment".into(), json!(environment));
        }
        if let Some(run_list) = args.run_list {
            object.insert("run_list".into(), json!(run_list));
        }
        if let Some(normal) = args.normal {
            object.insert("normal".into(), normal);
        }
        if let Some(policy_name) = args.policy_name {
            object.insert("policy_name".into(), json!(policy_name));
        }
        if let Some(policy_group) = args.policy_group {
            object.insert("policy_group".into(), json!(policy_group));
        }
        let auth = ctx.auth_context();
        let node = crate::nodes::create_node_object(&ctx.state, &auth, &ctx.org, &mut body).await?;
        Ok(self.json_result(&node_summary(&node)))
    }

    async fn update_node_inner(&self, args: UpdateNodeArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let auth = ctx.auth_context();
        let mut body = args.node;
        let node =
            crate::nodes::update_node_object(&ctx.state, &auth, &ctx.org, &args.name, &mut body)
                .await?;
        Ok(self.json_result(&node_summary(&node)))
    }

    async fn delete_node_inner(&self, args: DeleteNodeArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let auth = ctx.auth_context();
        let node =
            crate::nodes::delete_node_object(&ctx.state, &auth, &ctx.org, &args.name).await?;
        Ok(self.json_result(&json!({ "name": node.name, "deleted": true })))
    }

    async fn create_environment_inner(
        &self,
        args: CreateEnvironmentArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let mut body = json!({ "name": args.name });
        let object = body.as_object_mut().ok_or_else(ApiError::internal)?;
        if let Some(attributes) = args.attributes {
            if let Some(default) = attributes.get("default").filter(|value| !value.is_null()) {
                object.insert("default_attributes".into(), default.clone());
            }
            if let Some(override_) = attributes.get("override").filter(|value| !value.is_null()) {
                object.insert("override_attributes".into(), override_.clone());
            }
        }
        if let Some(cookbook_versions) = args.cookbook_versions {
            object.insert("cookbook_versions".into(), cookbook_versions);
        }
        let auth = ctx.auth_context();
        let environment =
            crate::environments::create_environment_object(&ctx.state, &auth, &ctx.org, &mut body)
                .await?;
        Ok(self.json_result(&json!({
            "name": environment.name,
            "updated_at": environment.updated_at,
        })))
    }

    async fn update_environment_inner(
        &self,
        args: UpdateEnvironmentArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let mut body = args.environment;
        if let Some(new_name) = args.new_name {
            if new_name != args.name {
                let object = body
                    .as_object_mut()
                    .ok_or_else(|| ApiError::bad_request("Incorrect JSON type for request body"))?;
                object.insert("name".into(), json!(new_name));
            }
        }
        let auth = ctx.auth_context();
        let (environment, renamed) = crate::environments::update_environment_object(
            &ctx.state, &auth, &ctx.org, &args.name, &mut body,
        )
        .await?;
        Ok(self.json_result(&json!({
            "name": environment.name,
            "updated_at": environment.updated_at,
            "renamed": renamed,
        })))
    }

    async fn delete_environment_inner(
        &self,
        args: DeleteEnvironmentArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let auth = ctx.auth_context();
        let environment =
            crate::environments::delete_environment_object(&ctx.state, &auth, &ctx.org, &args.name)
                .await?;
        Ok(self.json_result(&json!({ "name": environment.name, "deleted": true })))
    }

    async fn upload_cookbook_version_inner(
        &self,
        args: UploadCookbookArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let (manifest, blobs) = match build_manifest(ctx, &args).await {
            Ok(built) => built,
            Err(message) => return Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        };

        let mut checksums: Vec<String> =
            blobs.iter().map(|(checksum, _)| checksum.clone()).collect();
        checksums.sort();
        checksums.dedup();
        if !checksums.is_empty() {
            ctx.state
                .store
                .ensure_checksums(&ctx.org.id, &checksums)
                .await
                .map_err(ApiError::from)?;
        }
        for (checksum, bytes) in &blobs {
            ctx.state
                .object_store
                .put(&ctx.org.id, checksum, bytes)
                .map_err(|error| {
                    tracing::error!(%error, "mcp cookbook upload blob write failed");
                    ApiError::internal()
                })?;
        }

        let auth = ctx.auth_context();
        let (_, response) = crate::cookbooks::put_cookbook_version_object(
            &ctx.state,
            &auth,
            &ctx.org,
            &args.name,
            &args.version,
            &manifest,
            2,
            args.force,
        )
        .await?;
        let frozen = response
            .get("frozen?")
            .and_then(Value::as_bool)
            .unwrap_or(args.frozen);
        Ok(self.json_result(&json!({
            "name": args.name,
            "version": args.version,
            "frozen": frozen,
        })))
    }

    async fn delete_cookbook_version_inner(
        &self,
        args: CookbookVersionArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let auth = ctx.auth_context();
        let deleted = crate::cookbooks::delete_cookbook_version_object(
            &ctx.state,
            &auth,
            &ctx.org,
            &args.name,
            &args.version,
            2,
        )
        .await?;
        let name = deleted
            .get("cookbook_name")
            .and_then(Value::as_str)
            .unwrap_or(&args.name);
        let version = deleted
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or(&args.version);
        Ok(self.json_result(&json!({
            "name": name,
            "version": version,
            "deleted": true,
        })))
    }
}

impl McpServer {
    async fn list_nodes_inner(&self, args: ListNodesArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let mut nodes = ctx
            .state
            .store
            .list_nodes(&ctx.org.id)
            .await
            .map_err(ApiError::from)?;
        nodes.sort_by(|left, right| left.name.cmp(&right.name));

        let query = args.query.map(|query| query.to_lowercase());
        let limit = ctx.clamped_limit(args.limit);
        let mut rows = Vec::new();
        for node in nodes {
            if let Some(query) = &query {
                if !node.name.to_lowercase().contains(query) {
                    continue;
                }
            }
            if args
                .environment
                .as_deref()
                .is_some_and(|environment| environment != node.environment.as_str())
            {
                continue;
            }
            if !ctx.can_read(&node.authz_id).await? {
                continue;
            }
            rows.push(json!({
                "name": node.name,
                "environment": node.environment,
                "policy_name": node.policy_name,
                "policy_group": node.policy_group,
                "updated_at": node.updated_at,
            }));
            if rows.len() == limit {
                break;
            }
        }
        Ok(self.json_result(&json!({ "nodes": rows })))
    }

    async fn get_node_inner(&self, args: GetNodeArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let node = ctx
            .state
            .store
            .find_node(&ctx.org.id, &args.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::not_found(format!("node '{}' not found", args.name)))?;
        ctx.require_read(&node.authz_id).await?;
        let value: Value =
            serde_json::from_str(&node.serialized_object).map_err(|_| ApiError::internal())?;
        Ok(self.json_result(&value))
    }

    async fn list_environments_inner(
        &self,
        args: ListEnvironmentsArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let limit = ctx.clamped_limit(args.limit);
        let names = ctx
            .state
            .store
            .list_environment_names(&ctx.org.id)
            .await
            .map_err(ApiError::from)?;
        let mut environments = Vec::new();
        for name in names {
            let Some(environment) = ctx
                .state
                .store
                .find_environment(&ctx.org.id, &name)
                .await
                .map_err(ApiError::from)?
            else {
                continue;
            };
            if !ctx.can_read(&environment.authz_id).await? {
                continue;
            }
            environments.push(json!({
                "name": environment.name,
                "updated_at": environment.updated_at,
            }));
            if environments.len() == limit {
                break;
            }
        }
        Ok(self.json_result(&json!({ "environments": environments })))
    }

    async fn get_environment_inner(
        &self,
        args: GetEnvironmentArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let environment = ctx
            .state
            .store
            .find_environment(&ctx.org.id, &args.name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::not_found(format!("environment '{}' not found", args.name)))?;
        ctx.require_read(&environment.authz_id).await?;
        let value: Value = serde_json::from_str(&environment.serialized_object)
            .map_err(|_| ApiError::internal())?;
        Ok(self.json_result(&value))
    }

    async fn list_cookbooks_inner(
        &self,
        args: ListCookbooksArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let query = args.query.map(|query| query.to_lowercase());
        let versions = ctx
            .state
            .store
            .list_cookbook_versions(&ctx.org.id)
            .await
            .map_err(ApiError::from)?;

        let mut grouped: BTreeMap<String, Vec<StoredCookbookVersion>> = BTreeMap::new();
        for version in versions {
            if let Some(query) = &query {
                if !version.name.to_lowercase().contains(query) {
                    continue;
                }
            }
            if !ctx.can_read(&version.authz_id).await? {
                continue;
            }
            grouped
                .entry(version.name.clone())
                .or_default()
                .push(version);
        }

        let limit = ctx.clamped_limit(args.limit);
        let mut cookbooks = Vec::new();
        for (name, mut versions) in grouped {
            sort_versions_descending(&mut versions);
            let newest_version = versions
                .first()
                .map(|version| version.version.clone())
                .unwrap_or_default();
            let versions: Vec<&str> = versions
                .iter()
                .map(|version| version.version.as_str())
                .collect();
            cookbooks.push(json!({
                "name": name,
                "versions": versions,
                "newest_version": newest_version,
            }));
            if cookbooks.len() == limit {
                break;
            }
        }
        Ok(self.json_result(&json!({ "cookbooks": cookbooks })))
    }

    async fn list_cookbook_versions_inner(
        &self,
        args: CookbookNameArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let versions = ctx
            .state
            .store
            .list_cookbook_versions(&ctx.org.id)
            .await
            .map_err(ApiError::from)?;

        let mut readable = Vec::new();
        for version in versions {
            if version.name != args.name {
                continue;
            }
            if ctx.can_read(&version.authz_id).await? {
                readable.push(version);
            }
        }
        sort_versions_descending(&mut readable);
        let versions: Vec<Value> = readable
            .iter()
            .map(|version| {
                json!({
                    "name": version.name,
                    "version": version.version,
                    "frozen": version.frozen,
                    "updated_at": version.updated_at,
                })
            })
            .collect();
        Ok(self.json_result(&json!({ "versions": versions })))
    }

    async fn get_cookbook_version_inner(
        &self,
        args: CookbookVersionArgs,
    ) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let cookbook = resolve_version(&ctx.state, &ctx.org, &args.name, &args.version)
            .await?
            .ok_or_else(|| {
                ApiError::not_found(format!(
                    "Cannot find a cookbook named {} with version {}",
                    args.name, args.version
                ))
            })?;
        ctx.require_read(&cookbook.authz_id).await?;
        let manifest = render_manifest(&cookbook.manifest, 2, Some(&ctx.state), &ctx.org.id)?;
        Ok(self.json_result(&manifest))
    }

    async fn list_runs_inner(&self, args: ListRunsArgs) -> Result<CallToolResult, ApiError> {
        let runs = self
            .filtered_runs(
                args.node_name,
                args.status,
                args.failures_only,
                args.since,
                args.limit,
            )
            .await?;
        let runs: Vec<Value> = runs.iter().map(|run| run_summary(run, None)).collect();
        Ok(self.json_result(&json!({ "runs": runs })))
    }

    async fn latest_run_inner(&self, args: LatestRunArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let run = ctx
            .state
            .store
            .latest_node_run(&ctx.org.id, &args.node_name)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| ApiError::not_found(format!("no runs for node '{}'", args.node_name)))?;
        require_node_read(ctx, &args.node_name).await?;

        let report = parse_report(&run).unwrap_or(Value::Null);
        Ok(self.json_result(&run_summary(&run, Some(&report))))
    }

    async fn get_run_inner(&self, args: GetRunArgs) -> Result<CallToolResult, ApiError> {
        let ctx = &self.ctx;
        let run = ctx
            .state
            .store
            .find_node_run(&ctx.org.id, &args.node_name, &args.run_id)
            .await
            .map_err(ApiError::from)?
            .ok_or_else(|| {
                ApiError::not_found(format!(
                    "run '{}' for node '{}' not found",
                    args.run_id, args.node_name
                ))
            })?;
        require_node_read(ctx, &args.node_name).await?;
        let report: Value = serde_json::from_str(&run.report).map_err(|_| ApiError::internal())?;
        Ok(self.json_result(&report))
    }

    async fn list_converge_errors_inner(
        &self,
        args: ListConvergeErrorsArgs,
    ) -> Result<CallToolResult, ApiError> {
        let runs = self
            .filtered_runs(args.node_name, None, None, args.since, args.limit)
            .await?;
        let mut errors = Vec::new();
        for run in &runs {
            errors.extend(converge_errors(run));
        }
        Ok(self.json_result(&json!({ "errors": errors })))
    }

    /// Fetch the runs matching the given filters, dropping rows whose node the
    /// principal cannot read. A named but unreadable node is a forbidden error
    /// instead of an empty result, so callers can tell "no runs" from "not
    /// allowed".
    async fn filtered_runs(
        &self,
        node_name: Option<String>,
        status: Option<String>,
        failures_only: Option<bool>,
        since: Option<String>,
        limit: Option<u32>,
    ) -> Result<Vec<StoredNodeRun>, ApiError> {
        let ctx = &self.ctx;
        let since = parse_since(since)?;
        if let Some(node_name) = &node_name {
            require_node_read(ctx, node_name).await?;
        }
        let runs = ctx
            .state
            .store
            .list_node_runs(
                &ctx.org.id,
                &NodeRunFilter {
                    node_name,
                    status,
                    since,
                    failures_only: failures_only.unwrap_or(false),
                    limit: ctx.clamped_limit(limit),
                },
            )
            .await
            .map_err(ApiError::from)?;

        let mut readable = Vec::new();
        for run in runs {
            if ctx.node_readable(&run.node_name).await? {
                readable.push(run);
            }
        }
        Ok(readable)
    }

    fn json_result<T: serde::Serialize>(&self, value: &T) -> CallToolResult {
        match serde_json::to_string(value) {
            Ok(serialized) if serialized.len() > self.ctx.max_result_bytes => {
                CallToolResult::error(vec![ContentBlock::text("result exceeds max_result_bytes")])
            }
            Ok(serialized) => CallToolResult::success(vec![ContentBlock::text(serialized)]),
            Err(error) => {
                tracing::error!(%error, "failed to serialize mcp tool result");
                CallToolResult::error(vec![ContentBlock::text("failed to serialize result")])
            }
        }
    }
}

/// The compact summary returned by node writes: identity and routing fields
/// only, never the full document.
fn node_summary(node: &StoredNode) -> Value {
    json!({
        "name": node.name,
        "environment": node.environment,
        "policy_name": node.policy_name,
        "policy_group": node.policy_group,
    })
}

/// The list-view summary of one run. With `report`, the summary is extended
/// with the run's top-level `error` and normalized `resource_errors`.
fn run_summary(run: &StoredNodeRun, report: Option<&Value>) -> Value {
    let mut summary = json!({
        "run_id": run.run_id,
        "node_name": run.node_name,
        "status": run.status,
        "start_time": run.start_time,
        "end_time": run.end_time,
        "resource_count": run.resource_count,
        "updated_resource_count": run.updated_resource_count,
        "error_count": run.error_count,
    });
    if let (Some(report), Some(summary)) = (report, summary.as_object_mut()) {
        summary.insert(
            "error".to_string(),
            report.get("error").cloned().unwrap_or(Value::Null),
        );
        summary.insert(
            "resource_errors".to_string(),
            Value::Array(resource_errors(report)),
        );
    }
    summary
}

fn parse_since(since: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(since) = since else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(&since)
        .map_err(|_| ApiError::bad_request("since must be an RFC 3339 timestamp"))?;
    Ok(Some(since))
}

async fn require_node_read(ctx: &McpContext, node_name: &str) -> Result<(), ApiError> {
    if ctx.node_readable(node_name).await? {
        Ok(())
    } else {
        Err(ApiError::forbidden_with("missing read permission"))
    }
}

fn parse_report(run: &StoredNodeRun) -> Option<Value> {
    match serde_json::from_str(&run.report) {
        Ok(report) => Some(report),
        Err(error) => {
            tracing::warn!(
                node_name = %run.node_name,
                run_id = %run.run_id,
                %error,
                "stored run report is not valid JSON"
            );
            None
        }
    }
}

/// The summary entries for every resource in `report` that carries an error.
fn resource_errors(report: &Value) -> Vec<Value> {
    report
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|resource| {
            let error = resource.get("error").filter(|error| !error.is_null())?;
            Some(json!({
                "resource_type": resource.get("type").cloned().unwrap_or(Value::Null),
                "resource_name": resource.get("name").cloned().unwrap_or(Value::Null),
                "error_class": error.get("class").cloned().unwrap_or(Value::Null),
                "error_message": error.get("message").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect()
}

/// Flatten one run's top-level and resource errors into `list_converge_errors`
/// rows. Reports are written by this server, so a parse failure is logged and
/// treated as "no errors" rather than failing the whole listing.
fn converge_errors(run: &StoredNodeRun) -> Vec<Value> {
    let Some(report) = parse_report(run) else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    if let Some(error) = report.get("error").filter(|error| !error.is_null()) {
        errors.push(json!({
            "run_id": run.run_id,
            "node_name": run.node_name,
            "start_time": run.start_time,
            "resource_type": Value::Null,
            "resource_name": Value::Null,
            "error_class": error.get("class").cloned().unwrap_or(Value::Null),
            "error_message": error.get("message").cloned().unwrap_or(Value::Null),
        }));
    }
    for resource in resource_errors(&report) {
        errors.push(json!({
            "run_id": run.run_id,
            "node_name": run.node_name,
            "start_time": run.start_time,
            "resource_type": resource.get("resource_type").cloned().unwrap_or(Value::Null),
            "resource_name": resource.get("resource_name").cloned().unwrap_or(Value::Null),
            "error_class": resource.get("error_class").cloned().unwrap_or(Value::Null),
            "error_message": resource.get("error_message").cloned().unwrap_or(Value::Null),
        }));
    }
    errors
}

/// Time one tool call and emit the single structured log line required by the
/// MCP design: organization, principal, tool, duration, and result size. Write
/// callers pass the target; arguments and result bodies are never logged.
async fn logged_tool<F>(
    ctx: &McpContext,
    tool: &'static str,
    target: Option<&str>,
    future: F,
) -> Result<CallToolResult, ErrorData>
where
    F: std::future::Future<Output = Result<CallToolResult, ApiError>>,
{
    let started = Instant::now();
    let result = tool_result(future.await)?;
    let elapsed = started.elapsed();
    let error = result.is_error == Some(true);
    tracing::info!(
        organization = %ctx.org.name,
        principal = %ctx.principal.name,
        tool = tool,
        target = target.unwrap_or(""),
        duration_ms = elapsed.as_millis() as u64,
        result_count = result_count(&result),
        outcome = if error { "error" } else { "ok" },
        "mcp tool call"
    );
    Ok(result)
}

/// The number of rows a tool result represents: 0 for errors and non-JSON
/// content, the length of a top-level array or of the sole array in a result
/// envelope (`{"nodes": [...]}`), and 1 for any other JSON value.
fn result_count(result: &CallToolResult) -> usize {
    if result.is_error == Some(true) {
        return 0;
    }
    let Some(ContentBlock::Text(text)) = result.content.first() else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text.text) else {
        return 0;
    };
    match &value {
        Value::Array(rows) => rows.len(),
        Value::Object(object) if object.len() == 1 => match object.values().next() {
            Some(Value::Array(rows)) => rows.len(),
            _ => 1,
        },
        _ => 1,
    }
}

fn tool_result(result: Result<CallToolResult, ApiError>) -> Result<CallToolResult, ErrorData> {
    Ok(match result {
        Ok(result) => result,
        Err(error) => api_error_result(error),
    })
}

/// Map an API failure onto the caller-visible tool error text. 404 and 403
/// become stable short strings; other client errors keep their message.
fn api_error_result(error: ApiError) -> CallToolResult {
    let message = if error.status == StatusCode::FORBIDDEN {
        "forbidden".to_string()
    } else if error.status == StatusCode::NOT_FOUND {
        "not found".to_string()
    } else if error.status.is_server_error() {
        tracing::error!(message = %error.message, "mcp tool failed");
        "internal error".to_string()
    } else {
        error.message
    };
    CallToolResult::error(vec![ContentBlock::text(message)])
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Microwave Server organization tools")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_result(text: String) -> CallToolResult {
        CallToolResult::success(vec![ContentBlock::text(text)])
    }

    #[test]
    fn result_count_counts_envelope_array() {
        let result = text_result(json!({ "nodes": ["a", "b"] }).to_string());
        assert_eq!(result_count(&result), 2);
    }

    #[test]
    fn result_count_counts_bare_array() {
        let result = text_result(json!([{ "name": "a" }, { "name": "b" }]).to_string());
        assert_eq!(result_count(&result), 2);
    }

    #[test]
    fn result_count_of_empty_array_is_zero() {
        let result = text_result(json!([]).to_string());
        assert_eq!(result_count(&result), 0);
    }

    #[test]
    fn result_count_of_full_object_is_one() {
        let result = text_result(
            json!({
                "name": "web-1",
                "chef_environment": "_default",
                "run_list": ["recipe[apt::default]"],
                "normal": {},
            })
            .to_string(),
        );
        assert_eq!(result_count(&result), 1);
    }

    #[test]
    fn result_count_of_error_is_zero() {
        let result = CallToolResult::error(vec![ContentBlock::text(
            json!({ "nodes": ["a", "b"] }).to_string(),
        )]);
        assert_eq!(result_count(&result), 0);
    }

    #[test]
    fn result_count_of_non_json_is_zero() {
        let result = text_result("forbidden".to_string());
        assert_eq!(result_count(&result), 0);
    }
}
