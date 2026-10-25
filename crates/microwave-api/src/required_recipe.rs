//! `GET /organizations/:org/required_recipe` when the feature is enabled.
//!
//! Upstream serves the contents of a single administrator-provided recipe as
//! plain text. The route is only registered when `required_recipe.enabled` is
//! true; while disabled the normal fallback returns `404` for every method,
//! matching `required_recipe_spec.rb`.

use crate::{ApiError, AppState};
use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::Path;

/// Recipe files are small. This bound also guards against accidentally
/// pointing the endpoint at a large log or binary file.
const MAX_RECIPE_BYTES: u64 = 1_048_576;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/organizations/{organization}/required_recipe",
        get(get_recipe),
    )
}

async fn get_recipe(State(state): State<AppState>) -> Result<Response, ApiError> {
    let path = state
        .config
        .required_recipe
        .path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            tracing::error!("required_recipe is enabled but required_recipe.path is not set");
            ApiError::internal()
        })?;

    let recipe_path = path.to_string();
    let contents = tokio::task::spawn_blocking(move || read_recipe(Path::new(&recipe_path)))
        .await
        .map_err(|error| {
            tracing::error!(%error, path, "required recipe read task failed");
            ApiError::internal()
        })?
        .map_err(|error| {
            tracing::error!(path, %error, "required recipe rejected");
            ApiError::internal()
        })?;

    let mut response = contents.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    Ok(response)
}

fn read_recipe(path: &Path) -> Result<String, String> {
    let mut file = open_no_follow(path).map_err(|error| {
        if is_symlink_error(&error) {
            "required_recipe.path must not be a symlink".to_string()
        } else {
            format!("failed to open required recipe: {error}")
        }
    })?;

    let metadata = file
        .metadata()
        .map_err(|error| format!("failed to stat required recipe: {error}"))?;
    if !metadata.is_file() {
        return Err("required_recipe.path must be a regular file".to_string());
    }
    if metadata.len() > MAX_RECIPE_BYTES {
        return Err(format!(
            "required recipe exceeds the {} byte limit",
            MAX_RECIPE_BYTES
        ));
    }
    validate_permissions(&metadata)?;

    let mut contents = String::new();
    file.by_ref()
        .take(MAX_RECIPE_BYTES + 1)
        .read_to_string(&mut contents)
        .map_err(|error| format!("failed to read required recipe: {error}"))?;
    if contents.len() as u64 > MAX_RECIPE_BYTES {
        return Err(format!(
            "required recipe exceeds the {} byte limit",
            MAX_RECIPE_BYTES
        ));
    }
    Ok(contents)
}

fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(not(unix))]
    {
        if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path is a symlink",
            ));
        }
    }
    options.open(path)
}

fn is_symlink_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        if error.raw_os_error() == Some(libc::ELOOP) {
            return true;
        }
    }
    error.kind() == std::io::ErrorKind::InvalidInput
}

fn validate_permissions(metadata: &std::fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let mode = metadata.mode();
        if mode & 0o022 != 0 {
            return Err("required recipe must not be group- or world-writable".to_string());
        }

        let owner = metadata.uid();
        let effective_uid = unsafe { libc::geteuid() };
        // Upstream expects root ownership. A dedicated service account may
        // also own the file, but a root-owned recipe remains valid even when
        // the server runs as an unprivileged UID (common in containers).
        if !ownership_allowed(owner, effective_uid) {
            return Err(format!(
                "required recipe must be owned by root or the server user (uid {effective_uid}), got uid {owner}"
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
    }
    Ok(())
}

#[cfg(unix)]
fn ownership_allowed(owner: u32, effective_uid: u32) -> bool {
    owner == 0 || owner == effective_uid
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn root_or_service_user_ownership_is_allowed() {
        use super::ownership_allowed;

        assert!(ownership_allowed(0, 0));
        assert!(ownership_allowed(0, 10_001));
        assert!(ownership_allowed(10_001, 10_001));
        assert!(!ownership_allowed(10_002, 10_001));
    }
}
