use crate::{context::Context, CtlError};
use microwave_store::StoreError;

/// Credentials that can be managed through the CLI. The names mirror the
/// upstream `chef-server-ctl` groups plus the `object_store` group introduced
/// by Microwave's local `/bookshelf` endpoint.
pub(crate) const KNOWN_CREDENTIALS: &[(&str, &[&str])] = &[
    ("object_store", &["secret"]),
    (
        "bookshelf",
        &[
            "access_key_id",
            "secret_access_key",
            "sql_password",
            "sql_ro_password",
        ],
    ),
    ("data_collector", &["token"]),
    ("otel", &["headers"]),
    ("redis_lb", &["password"]),
    ("postgresql", &["db_superuser_password"]),
    ("ldap", &["bind_password"]),
    (
        "opscode_erchef",
        &["sql_password", "sql_ro_password", "stats_password"],
    ),
    (
        "oc_bifrost",
        &["superuser_id", "sql_password", "sql_ro_password"],
    ),
    (
        "oc_id",
        &["secret_key_base", "sql_password", "sql_ro_password"],
    ),
    ("manage", &["secret_key_base", "secret_token"]),
    ("saml", &["client_id", "client_secret"]),
    (
        "push-jobs-server",
        &[
            "pushy_priv_key",
            "pushy_pub_key",
            "sql_password",
            "sql_ro_password",
        ],
    ),
    ("opscode-reporting", &["sql_password", "sql_ro_password"]),
    ("drbd", &["shared_secret"]),
    ("keepalived", &["vrrp_instance_password"]),
    ("mcp", &[]),
];

/// Store a secret. The value is never echoed back to the terminal.
pub fn set_secret(context: &Context, group: &str, name: &str, value: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_metadata_store()?;
    store.run(store.store().set_secret(group, name, value))?;
    println!("Secret {group}.{name} updated.");
    Ok(())
}

/// Print a secret to stdout. This is the only command that reveals values.
pub fn show_secret(context: &Context, group: &str, name: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_metadata_store()?;
    let value = store
        .run(store.store().get_secret(group, name))?
        .ok_or_else(|| StoreError::NotFound(format!("secret {group}.{name}")))?;
    println!("{value}");
    Ok(())
}

pub fn remove_secret(context: &Context, group: &str, name: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_metadata_store()?;
    store.run(store.store().remove_secret(group, name))?;
    println!("Removed secret {group}.{name}.");
    Ok(())
}

pub fn maintenance(context: &Context, action: crate::MaintenanceAction) -> Result<(), CtlError> {
    use crate::MaintenanceAction;

    let store = context.open_metadata_store()?;
    match action {
        MaintenanceAction::On => {
            store.run(store.store().set_maintenance_enabled(true))?;
            println!("Maintenance mode enabled.");
        }
        MaintenanceAction::Off => {
            store.run(store.store().set_maintenance_enabled(false))?;
            println!("Maintenance mode disabled.");
        }
        MaintenanceAction::Status => {
            println!(
                "maintenance: {}",
                if store.run(store.store().maintenance_enabled())? {
                    "on"
                } else {
                    "off"
                }
            );
        }
    }
    Ok(())
}

fn validate_credential(group: &str, name: &str) -> Result<(), CtlError> {
    let Some((_, names)) = KNOWN_CREDENTIALS
        .iter()
        .find(|(known_group, _)| *known_group == group)
    else {
        let groups = KNOWN_CREDENTIALS
            .iter()
            .map(|(group, _)| *group)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CtlError::message(format!(
            "unknown credential group '{group}'; known groups: {groups}"
        )));
    };

    if group == "mcp" {
        return microwave_config::valid_org_name(name)
            .then_some(())
            .ok_or_else(|| {
                CtlError::message(format!(
                    "invalid organization name '{name}' for the mcp credential group"
                ))
            });
    }

    if names.contains(&name) {
        Ok(())
    } else {
        Err(CtlError::message(format!(
            "unknown credential '{name}' in group '{group}'; known names: {}",
            names.join(", ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{set_secret, show_secret, validate_credential};
    use crate::context::Context;
    use std::path::Path;

    fn context_with_store(dir: &Path) -> Context {
        let config_path = dir.join("microwave.toml");
        std::fs::write(
            &config_path,
            format!(
                "[paths]\ndata_dir = '{}'\n\n[database]\ndriver = \"sqlite\"\n",
                dir.join("data").display()
            ),
        )
        .unwrap();
        Context::load(Some(&config_path)).unwrap()
    }

    #[test]
    fn mcp_group_accepts_org_names_and_rejects_bad_names() {
        assert!(validate_credential("mcp", "acme").is_ok());
        assert!(validate_credential("mcp", "team-1").is_ok());
        assert!(validate_credential("mcp", "Acme").is_err());
        assert!(validate_credential("mcp", "").is_err());
        assert!(validate_credential("mcp", "with/slash").is_err());
    }

    /// `otel headers` stores the newline-separated header lines that
    /// `bootstrap::apply_otel_secrets` later reads back.
    #[test]
    fn otel_group_stores_and_shows_the_headers_secret() {
        assert!(validate_credential("otel", "headers").is_ok());
        assert!(validate_credential("otel", "endpoint").is_err());

        let dir = tempfile::tempdir().unwrap();
        let context = context_with_store(dir.path());
        set_secret(
            &context,
            "otel",
            "headers",
            "Authorization: Bearer s3cr3t\nX-Scope: metrics",
        )
        .unwrap();
        show_secret(&context, "otel", "headers").unwrap();

        let store = context.open_metadata_store().unwrap();
        let value = store
            .run(store.store().get_secret("otel", "headers"))
            .unwrap()
            .expect("stored otel headers secret");
        assert_eq!(value, "Authorization: Bearer s3cr3t\nX-Scope: metrics");
    }
}
