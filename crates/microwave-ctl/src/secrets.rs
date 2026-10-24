use crate::{context::Context, CtlError};
use microwave_store::StoreError;

/// Credentials that can be managed through the CLI. The names mirror the
/// upstream `chef-server-ctl` groups plus the `object_store` group introduced
/// by Microwave's local `/bookshelf` endpoint.
const KNOWN_CREDENTIALS: &[(&str, &[&str])] = &[
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
];

/// Store a secret. The value is never echoed back to the terminal.
pub fn set_secret(context: &Context, group: &str, name: &str, value: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_store()?;
    store.set_secret(group, name, value)?;
    println!("Secret {group}.{name} updated.");
    Ok(())
}

/// Print a secret to stdout. This is the only command that reveals values.
pub fn show_secret(context: &Context, group: &str, name: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_store()?;
    let value = store
        .get_secret(group, name)?
        .ok_or_else(|| StoreError::NotFound(format!("secret {group}.{name}")))?;
    println!("{value}");
    Ok(())
}

pub fn remove_secret(context: &Context, group: &str, name: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    let store = context.open_store()?;
    store.remove_secret(group, name)?;
    println!("Removed secret {group}.{name}.");
    Ok(())
}

pub fn maintenance(context: &Context, action: crate::MaintenanceAction) -> Result<(), CtlError> {
    use crate::MaintenanceAction;

    let store = context.open_store()?;
    match action {
        MaintenanceAction::On => {
            store.set_maintenance_enabled(true)?;
            println!("Maintenance mode enabled.");
        }
        MaintenanceAction::Off => {
            store.set_maintenance_enabled(false)?;
            println!("Maintenance mode disabled.");
        }
        MaintenanceAction::Status => {
            println!(
                "maintenance: {}",
                if store.maintenance_enabled()? {
                    "on"
                } else {
                    "off"
                }
            );
        }
    }
    println!(
        "note: HTTP maintenance enforcement is not implemented yet; running servers do not return 503 while maintenance mode is on."
    );
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

    if names.contains(&name) {
        Ok(())
    } else {
        Err(CtlError::message(format!(
            "unknown credential '{name}' in group '{group}'; known names: {}",
            names.join(", ")
        )))
    }
}
