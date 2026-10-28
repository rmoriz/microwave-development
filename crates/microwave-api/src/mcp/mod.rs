//! Experimental per-organization Streamable HTTP MCP endpoint.
//!
//! The endpoint authenticates with a per-organization bearer token (or the
//! operator-managed `mcp` secret) and then delegates the JSON-RPC request to
//! `rmcp`'s streamable HTTP service.

pub mod auth;
pub mod context;
pub mod tools;

use crate::AppState;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::any;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tools::McpServer;
use tower::ServiceExt;

pub fn routes() -> Router<AppState> {
    Router::new().route("/organizations/{organization}/mcp", any(handle))
}

async fn handle(
    State(state): State<AppState>,
    Path(organization): Path<String>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    let ctx = match auth::authenticate(&state, &organization, &headers).await {
        Ok(ctx) => ctx,
        Err(response) => return response,
    };
    let config = server_config(&state);
    let service: StreamableHttpService<McpServer, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(McpServer::new(ctx.clone())),
        Default::default(),
        config,
    );
    let response = service
        .oneshot(request)
        .await
        .expect("rmcp service is infallible");
    response.map(Body::new)
}

fn server_config(state: &AppState) -> StreamableHttpServerConfig {
    let mut config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_legacy_session_mode(false)
        .with_sse_keep_alive(None)
        .with_max_request_body_bytes(
            state.config.mcp.max_upload_bytes.saturating_mul(4) / 3 + 1024 * 1024,
        );

    let hosts = state
        .config
        .mcp
        .effective_allowed_hosts(state.config.server.public_url.as_deref());
    if !hosts.is_empty() {
        config = config.with_allowed_hosts(hosts);
    }
    if !state.config.mcp.allowed_origins.is_empty() {
        config = config.with_allowed_origins(state.config.mcp.allowed_origins.clone());
    }
    config
}
