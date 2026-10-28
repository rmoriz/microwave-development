//! Argument decoding for the `upload_cookbook_version` MCP tool.
//!
//! The HTTP API expects every file blob to be uploaded to bookshelf first. The
//! MCP tool collapses that into one call: inline base64 files are decoded here,
//! the v2 manifest is assembled, and the shared cookbook service performs the
//! validation, ACL, frozen, and checksum checks.

use crate::cookbooks;
use crate::mcp::context::McpContext;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

/// The HTTP API accepts at most this many files per cookbook version.
const MAX_FILES: usize = 100;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct UploadCookbookArgs {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) metadata: Value,
    pub(crate) files: Vec<UploadFile>,
    #[serde(default)]
    pub(crate) frozen: bool,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct UploadFile {
    pub(crate) path: String,
    pub(crate) content_base64: Option<String>,
    pub(crate) checksum: Option<String>,
    pub(crate) specificity: Option<String>,
}

/// Decode the tool arguments into a v2 manifest and the blobs that still have
/// to be written. `Err` carries a caller-facing message: bad arguments are the
/// only expected failures, and the tool surfaces them as `isError` text.
pub(crate) async fn build_manifest(
    ctx: &McpContext,
    args: &UploadCookbookArgs,
) -> Result<(Value, Vec<(String, Vec<u8>)>), String> {
    cookbooks::validate_cookbook_name(&args.name).map_err(|error| error.message)?;
    cookbooks::validate_version(&args.version).map_err(|error| error.message)?;
    if args.files.len() > MAX_FILES {
        return Err(format!(
            "A cookbook version may contain at most {MAX_FILES} files"
        ));
    }

    let metadata = args
        .metadata
        .as_object()
        .ok_or_else(|| "metadata must be an object".to_string())?;
    let mut metadata = metadata.clone();
    metadata.insert("version".to_string(), json!(args.version));

    let mut decoded_total = 0usize;
    let mut blobs = Vec::new();
    let mut all_files = Vec::with_capacity(args.files.len());
    for file in &args.files {
        let checksum = match &file.content_base64 {
            Some(encoded) => {
                let bytes = STANDARD
                    .decode(encoded)
                    .map_err(|error| format!("invalid base64 for '{}': {error}", file.path))?;
                decoded_total = decoded_total.saturating_add(bytes.len());
                if decoded_total > ctx.max_upload_bytes {
                    return Err(format!(
                        "Decoded upload exceeds max_upload_bytes ({})",
                        ctx.max_upload_bytes
                    ));
                }
                let checksum = microwave_objectstore::md5_hex(&bytes);
                blobs.push((checksum.clone(), bytes));
                checksum
            }
            None => {
                let checksum = file.checksum.clone().ok_or_else(|| {
                    format!("file '{}' needs content_base64 or a checksum", file.path)
                })?;
                if !is_lowercase_md5(&checksum) {
                    return Err(format!(
                        "file '{}' has an invalid checksum '{checksum}'",
                        file.path
                    ));
                }
                if !ctx
                    .state
                    .store
                    .checksum_exists(&ctx.org.id, &checksum)
                    .await
                    .map_err(|error| {
                        tracing::error!(%error, "mcp cookbook upload checksum lookup failed");
                        "internal error".to_string()
                    })?
                {
                    return Err(format!(
                        "file '{}' references checksum {checksum}, which has not been uploaded",
                        file.path
                    ));
                }
                checksum
            }
        };

        all_files.push(json!({
            "name": file.path,
            "path": file.path,
            "checksum": checksum,
            "specificity": file.specificity.clone().unwrap_or_else(|| "default".to_string()),
        }));
    }

    let manifest = json!({
        "name": format!("{}-{}", args.name, args.version),
        "cookbook_name": args.name,
        "version": args.version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": args.frozen,
        "metadata": metadata,
        "all_files": all_files,
    });
    Ok((manifest, blobs))
}

fn is_lowercase_md5(checksum: &str) -> bool {
    checksum.len() == 32
        && checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
