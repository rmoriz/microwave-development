use crate::mcp::context::McpContext;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::{tool_handler, tool_router, ServerHandler};

/// MCP server bound to one authenticated organization principal.
#[derive(Clone)]
pub struct McpServer {
    // Tool handlers added in later tasks read the authenticated context.
    #[allow(dead_code)]
    pub(crate) ctx: McpContext,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    pub fn new(ctx: McpContext) -> Self {
        Self {
            ctx,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router(allow_empty)]
impl McpServer {}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Microwave Server organization tools")
    }
}
