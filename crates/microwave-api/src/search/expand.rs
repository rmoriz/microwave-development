//! Document expansion for the in-memory search index.
//!
//! This mirrors the observable part of upstream `chef_index_expand`: nested
//! objects are flattened into `parent_child__=__value` pairs and leaf aliases
//! (`child__=__value`). Objects also contribute their child key names as
//! values under the parent path, matching the upstream expander.

use crate::search::query::Fields;
use serde_json::{Map, Value};

const ATTRIBUTE_KEYS: [&str; 4] = ["default", "normal", "override", "automatic"];

/// Flattens a JSON value into searchable field/value pairs.
pub fn flatten(value: &Value) -> Fields {
    let mut fields = Fields::new();
    flatten_into(value, &[], &mut fields);
    fields
}

/// Flattens a JSON value, adding the field/value pairs to `fields`.
pub fn flatten_into(value: &Value, path: &[String], fields: &mut Fields) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if !path.is_empty() {
                    add_value(fields, path, key);
                }
                let mut child_path = path.to_vec();
                child_path.push(key.clone());
                flatten_into(value, &child_path, fields);
            }
        }
        Value::Array(values) => {
            for value in values {
                flatten_into(value, path, fields);
            }
        }
        Value::Null => {
            add_value(fields, path, "");
        }
        Value::Bool(value) => {
            add_value(fields, path, if *value { "true" } else { "false" });
        }
        Value::Number(value) => {
            add_value(fields, path, &value.to_string());
        }
        Value::String(value) => {
            add_value(fields, path, value);
        }
    }
}

fn add_value(fields: &mut Fields, path: &[String], value: &str) {
    if path.is_empty() {
        return;
    }

    let leaf = path.last().map(String::as_str).unwrap_or_default();
    let full = path.join("_");
    insert_value(fields, &full, value);
    if full != leaf {
        insert_value(fields, leaf, value);
    }
}

fn insert_value(fields: &mut Fields, key: &str, value: &str) {
    let values = fields.entry(key.to_lowercase()).or_default();
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

/// Deep-merges node attribute groups into a single top-level object.
///
/// Precedence matches Chef: `default` < `normal` < `override` < `automatic`.
/// The remaining node keys (name, run_list, chef_environment, ...) are copied
/// on top of the merged attributes.
pub fn merged_node(value: &Value) -> Value {
    let mut merged = Map::new();
    for key in ATTRIBUTE_KEYS {
        if let Some(attributes) = value.get(key) {
            deep_merge(&mut merged, attributes);
        }
    }

    if let Some(object) = value.as_object() {
        for (key, value) in object {
            if !ATTRIBUTE_KEYS.contains(&key.as_str()) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(merged)
}

fn deep_merge(target: &mut Map<String, Value>, source: &Value) {
    let Some(source) = source.as_object() else {
        return;
    };

    for (key, value) in source {
        match (target.get_mut(key), value) {
            (Some(Value::Object(target_object)), Value::Object(source_object)) => {
                let mut merged = target_object.clone();
                deep_merge(&mut merged, &Value::Object(source_object.clone()));
                target.insert(key.clone(), Value::Object(merged));
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Adds `recipe` and `role` aliases derived from a `run_list`.
///
/// `run_list: ["recipe[apache]", "role[base]"]` yields `recipe:apache` and
/// `role:base`, matching the upstream search expectations.
pub fn add_run_list_expansions(value: &Value, fields: &mut Fields) {
    let Some(run_list) = value.get("run_list").and_then(Value::as_array) else {
        return;
    };

    for entry in run_list.iter().filter_map(Value::as_str) {
        if let Some(recipe) = entry
            .strip_prefix("recipe[")
            .and_then(|entry| entry.strip_suffix(']'))
        {
            insert_value(fields, "recipe", recipe);
        } else if let Some(role) = entry
            .strip_prefix("role[")
            .and_then(|entry| entry.strip_suffix(']'))
        {
            insert_value(fields, "role", role);
        }
    }
}

/// Looks up a dotted path inside a JSON object.
pub fn lookup_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    if path.is_empty() {
        return None;
    }

    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn values(fields: &Fields, key: &str) -> Vec<String> {
        fields.get(key).cloned().unwrap_or_default()
    }

    #[test]
    fn flattens_nested_values_and_leaf_aliases() {
        let fields = flatten(&json!({"a": {"b": "foo"}, "list": ["one", "two"]}));
        assert_eq!(values(&fields, "a_b"), vec!["foo"]);
        assert_eq!(values(&fields, "b"), vec!["foo"]);
        assert_eq!(values(&fields, "a"), vec!["b"]);
        assert_eq!(values(&fields, "list"), vec!["one", "two"]);
    }

    #[test]
    fn merges_node_attributes_with_chef_precedence() {
        let merged = merged_node(&json!({
            "name": "web1",
            "default": {"one": "default", "two": "default", "nested": {"value": "default"}},
            "normal": {"two": "normal", "nested": {"value": "normal"}},
            "override": {"three": "override"},
            "automatic": {"four": "automatic"}
        }));
        assert_eq!(merged["one"], "default");
        assert_eq!(merged["two"], "normal");
        assert_eq!(merged["three"], "override");
        assert_eq!(merged["four"], "automatic");
        assert_eq!(merged["nested"]["value"], "normal");
        assert_eq!(merged["name"], "web1");
        assert!(merged.get("default").is_none());
    }

    #[test]
    fn expands_run_lists() {
        let value = json!({"run_list": ["recipe[apache::default]", "role[web]"]});
        let mut fields = flatten(&value);
        add_run_list_expansions(&value, &mut fields);
        assert_eq!(values(&fields, "recipe"), vec!["apache::default"]);
        assert_eq!(values(&fields, "role"), vec!["web"]);
    }

    #[test]
    fn looks_up_paths_in_objects() {
        let value = json!({"default": {"platform": "ubuntu"}});
        let path = vec!["default".to_string(), "platform".to_string()];
        assert_eq!(lookup_path(&value, &path), Some(&json!("ubuntu")));
        assert_eq!(lookup_path(&value, &[]), None);
    }
}
