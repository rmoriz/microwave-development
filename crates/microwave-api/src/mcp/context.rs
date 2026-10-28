use crate::{ApiError, AppState, AuthContext};
use microwave_store::{Org, Permission, Principal};

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

    /// Whether the authenticated principal may read the object behind
    /// `authz_id`. Superusers bypass ACL checks like the HTTP API does.
    pub async fn can_read(&self, authz_id: &str) -> Result<bool, ApiError> {
        if self.is_superuser {
            return Ok(true);
        }
        self.state
            .store
            .check_permission(&self.principal.authz_id, authz_id, Permission::Read)
            .await
            .map_err(ApiError::from)
    }

    /// Fail with the API's forbidden error unless the principal may read
    /// `authz_id`.
    pub async fn require_read(&self, authz_id: &str) -> Result<(), ApiError> {
        if self.can_read(authz_id).await? {
            Ok(())
        } else {
            Err(ApiError::forbidden_with("missing read permission"))
        }
    }
}
