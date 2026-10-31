//! Values derived from a cookbook manifest and stored next to it.
//!
//! The helpers are lenient on purpose: the write paths validate manifests
//! before they reach the store, so a derivation never reports an error and only
//! gives up when the manifest does not carry the shape it needs.

/// Compact JSON of `metadata.dependencies`, or `None` when the manifest has
/// none or one entry is not a string.
pub fn cookbook_dependencies(manifest: &str) -> Option<String> {
    let manifest: serde_json::Value = serde_json::from_str(manifest).ok()?;
    compact_string_map(manifest.pointer("/metadata/dependencies")?.as_object()?)
}

/// Compact JSON of `cookbook_versions`, or `None` when the body has none or
/// one entry is not a string.
pub fn environment_constraints(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    compact_string_map(body.get("cookbook_versions")?.as_object()?)
}

/// Compact JSON copy of a string-valued map, or `None` when it is empty or a
/// value is not a string.
fn compact_string_map(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if map.is_empty() {
        return None;
    }
    let mut compact = serde_json::Map::with_capacity(map.len());
    for (name, value) in map {
        compact.insert(
            name.clone(),
            serde_json::Value::String(value.as_str()?.to_string()),
        );
    }
    serde_json::to_string(&compact).ok()
}
