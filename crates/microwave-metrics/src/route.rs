//! Route label normalization for the request histogram.
//!
//! Matched routes carry an axum template; everything else (the landing page,
//! `/version`, unknown paths) has no template and is normalized here so the
//! histogram never learns object names.
//!
//! The `:org`/`:id` fallback spelling deliberately differs from the axum
//! templates (`{organization}`/`{name}`). It only applies when no template
//! exists, so the two spellings never collide.

/// Literal path segments that stay in the label.
const KNOWN_SEGMENTS: &[&str] = &[
    "version",
    "_status",
    "_stats",
    "server_api_version",
    "license",
    "organizations",
    "nodes",
    "cookbooks",
    "data",
    "roles",
    "environments",
    "clients",
    "sandboxes",
    "users",
    "groups",
    "containers",
    "keys",
    "policies",
    "search",
    "file_store",
    "bookshelf",
    "id",
    "mcp",
    "validate",
    "required_recipe",
    "data-collector",
    "compliance",
    "css",
    "fonts",
    "favicon.svg",
    "index.html",
];

/// Split a path into non-empty segments, dropping any query string.
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .filter(|segment| !segment.is_empty())
}

/// Whether a segment looks like a canonical 8-4-4-4-12 UUID.
fn is_uuid(segment: &str) -> bool {
    let mut groups = segment.split('-');
    for expected in [8, 4, 4, 4, 12] {
        let Some(group) = groups.next() else {
            return false;
        };
        if group.len() != expected || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
    }
    groups.next().is_none()
}

/// Normalize a request path into a bounded histogram label.
pub fn metrics_route(path: &str) -> String {
    let mut segments = segments(path);
    let Some(first) = segments.next() else {
        return "/".to_string();
    };
    if !KNOWN_SEGMENTS.contains(&first) {
        return "unknown".to_string();
    }
    let mut route = format!("/{first}");
    let mut previous = first;
    for segment in segments {
        route.push('/');
        if previous == "organizations" {
            route.push_str(":org");
        } else if is_uuid(segment) {
            route.push_str(":uuid");
        } else if KNOWN_SEGMENTS.contains(&segment) {
            route.push_str(segment);
        } else {
            route.push_str(":id");
        }
        previous = segment;
    }
    route
}

/// Extract the organization from a request path, `_default` when absent.
pub fn organization_label(path: &str) -> String {
    let mut segments = segments(path);
    if segments.next() != Some("organizations") {
        return "_default".to_string();
    }
    segments
        .next()
        .map_or_else(|| "_default".to_string(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_templates_and_drops_ids() {
        assert_eq!(
            metrics_route("/organizations/acme/nodes/web1"),
            "/organizations/:org/nodes/:id"
        );
        assert_eq!(
            metrics_route("/organizations/acme/cookbooks/apache/1.2.3"),
            "/organizations/:org/cookbooks/:id/:id"
        );
        assert_eq!(
            metrics_route("/organizations/acme/nodes/0f1c2b3a-4d5e-6f70-8192-a3b4c5d6e7f8"),
            "/organizations/:org/nodes/:uuid"
        );
        assert_eq!(metrics_route("/"), "/");
        assert_eq!(metrics_route("/version"), "/version");
        assert_eq!(metrics_route("/_stats?format=json"), "/_stats");
        assert_eq!(metrics_route("/bookshelf/abc123"), "/bookshelf/:id");
        assert_eq!(metrics_route("/favicon.svg"), "/favicon.svg");
        assert_eq!(metrics_route("/totally-unknown"), "unknown");
    }

    #[test]
    fn organization_comes_from_the_path() {
        assert_eq!(organization_label("/organizations/acme/nodes/web1"), "acme");
        assert_eq!(
            organization_label("/organizations/acme/validate/nodes/web1"),
            "acme"
        );
        assert_eq!(organization_label("/organizations/acme/mcp"), "acme");
        assert_eq!(organization_label("/_stats"), "_default");
        assert_eq!(organization_label("/organizations/"), "_default");
    }
}
