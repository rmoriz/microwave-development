use crate::{AppState, AuthContext};
use microwave_store::{Org, Principal};

/// Per-request authentication and configuration resolved for one MCP call.
#[derive(Clone)]
pub struct McpContext {
    pub state: AppState,
    pub org: Org,
    pub principal: Principal,
    pub is_superuser: bool,
    pub writes: bool,
    pub max_results: usize,
    pub max_result_bytes: usize,
    pub max_upload_bytes: usize,
}

impl McpContext {
    pub fn auth_context(&self) -> AuthContext {
        AuthContext {
            principal: self.principal.clone(),
            org: Some(self.org.clone()),
            key_name: None,
            is_superuser: self.is_superuser,
        }
    }

    pub fn clamped_limit(&self, limit: Option<u32>) -> usize {
        limit
            .map(|value| value as usize)
            .unwrap_or(self.max_results)
            .clamp(1, self.max_results)
    }
}
