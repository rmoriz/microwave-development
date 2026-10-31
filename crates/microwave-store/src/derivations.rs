//! Values derived from a cookbook manifest and stored next to it.
//!
//! The helpers are lenient on purpose: the write paths validate manifests
//! before they reach the store, so a derivation never reports an error and only
//! gives up when the manifest does not carry the shape it needs.

/// Compact JSON of `metadata.dependencies`, or `None` when the manifest has
/// none or one entry is not a string.
pub fn cookbook_dependencies(manifest: &str) -> Option<String> {
    let manifest: serde_json::Value = serde_json::from_str(manifest).ok()?;
    let dependencies = manifest.pointer("/metadata/dependencies")?.as_object()?;
    if dependencies.is_empty() {
        return None;
    }
    let mut compact = serde_json::Map::with_capacity(dependencies.len());
    for (name, value) in dependencies {
        compact.insert(
            name.clone(),
            serde_json::Value::String(value.as_str()?.to_string()),
        );
    }
    serde_json::to_string(&compact).ok()
}

/// Compact JSON of `cookbook_versions`, or `None` when the body has none or
/// one entry is not a string.
pub fn environment_constraints(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let constraints = body.get("cookbook_versions")?.as_object()?;
    if constraints.is_empty() {
        return None;
    }
    let mut compact = serde_json::Map::with_capacity(constraints.len());
    for (name, value) in constraints {
        compact.insert(
            name.clone(),
            serde_json::Value::String(value.as_str()?.to_string()),
        );
    }
    serde_json::to_string(&compact).ok()
}
