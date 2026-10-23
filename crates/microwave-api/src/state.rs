use microwave_config::Config;
use microwave_objectstore::LocalObjectStore;
use microwave_store::{Org, Principal, SqliteStore};
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub config: Arc<Config>,
    pub object_store: Arc<LocalObjectStore>,
}

#[derive(Debug, Clone, Copy)]
pub struct ApiVersion(pub u8);

#[derive(Clone)]
pub struct AuthContext {
    pub principal: Principal,
    pub org: Option<Org>,
}
