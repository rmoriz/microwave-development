use crate::cookbooks::{render_manifest, resolve_version, sort_versions_descending};
use crate::mcp::context::McpContext;
use crate::ApiError;
use axum::http::StatusCode;
use chrono::DateTime;
use microwave_store::{NodeRunFilter, StoredCookbookVersion, StoredNodeRun};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// MCP server bound to one authenticated organization principal.
#[derive(Clone)]
pub struct McpServer {
    pub(crate) ctx: McpContext,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    pub fn new(ctx: McpContext) -> Self {
        Self {
            ctx,
            tool_router: Self::read_tool_router(),
        }
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
        tool_result(self.list_nodes_inner(args).await)
    }

    /// Fetch one node document.
    #[tool(description = "Fetch a node by name")]
    async fn get_node(
        &self,
        Parameters(args): Parameters<GetNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.get_node_inner(args).await)
    }

    /// List the environments this principal may read.
    #[tool(description = "List environments readable by the caller")]
    async fn list_environments(
        &self,
        Parameters(args): Parameters<ListEnvironmentsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.list_environments_inner(args).await)
    }

    /// Fetch one environment document.
    #[tool(description = "Fetch an environment by name")]
    async fn get_environment(
        &self,
        Parameters(args): Parameters<GetEnvironmentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.get_environment_inner(args).await)
    }

    /// List cookbooks with the versions this principal may read.
    #[tool(description = "List cookbooks readable by the caller, with their versions")]
    async fn list_cookbooks(
        &self,
        Parameters(args): Parameters<ListCookbooksArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.list_cookbooks_inner(args).await)
    }

    /// List the versions of one cookbook.
    #[tool(description = "List the versions of a cookbook readable by the caller")]
    async fn list_cookbook_versions(
        &self,
        Parameters(args): Parameters<CookbookNameArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.list_cookbook_versions_inner(args).await)
    }

    /// Fetch one rendered cookbook manifest.
    #[tool(description = "Fetch a rendered cookbook manifest by name and version")]
    async fn get_cookbook_version(
        &self,
        Parameters(args): Parameters<CookbookVersionArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.get_cookbook_version_inner(args).await)
    }

    /// List stored run reports, newest first.
    #[tool(description = "List stored run_converge reports for readable nodes, newest first")]
    async fn list_runs(
        &self,
        Parameters(args): Parameters<ListRunsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.list_runs_inner(args).await)
    }

    /// Fetch the newest run report summary for one node.
    #[tool(description = "Fetch the newest run_converge summary for a node, including its errors")]
    async fn latest_run(
        &self,
        Parameters(args): Parameters<LatestRunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.latest_run_inner(args).await)
    }

    /// Fetch one full run report.
    #[tool(description = "Fetch the full stored run_converge report for one node run")]
    async fn get_run(
        &self,
        Parameters(args): Parameters<GetRunArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.get_run_inner(args).await)
    }

    /// List converge errors across stored run reports.
    #[tool(description = "List converge errors across readable node runs, newest first")]
    async fn list_converge_errors(
        &self,
        Parameters(args): Parameters<ListConvergeErrorsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        tool_result(self.list_converge_errors_inner(args).await)
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
