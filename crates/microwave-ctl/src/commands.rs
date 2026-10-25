use crate::{
    context::{Context, StoreHandle},
    CtlError,
};
use microwave_authz::create_organization;
use microwave_crypto::hash_password;
use microwave_store::{Org, StoreError, User};
use std::path::Path;
use uuid::Uuid;

pub fn status(context: &Context) -> Result<(), CtlError> {
    context.prepare_data_dir()?;

    println!("data_dir: {}", context.data_dir.display());
    println!("database_driver: {}", driver_name(&context.config));
    match &context.database_path {
        Some(path) => println!("database: {}", path.display()),
        None => println!("database: postgresql (configured)"),
    }

    let (readable, superuser_present) = match &context.database_path {
        Some(path) if path.is_file() => match context.open_existing_metadata_store() {
            Ok(store) => match store.run(store.store().find_user(context.superuser_name())) {
                Ok(user) => ("true", user.is_some()),
                Err(_) => ("false", false),
            },
            Err(_) => ("false", false),
        },
        Some(_) => ("false", false),
        // Probe PostgreSQL without making an unavailable database a hard
        // status failure: the report says `database_readable: false`.
        None => match context.open_existing_metadata_store() {
            Ok(store) => match store.run(store.store().find_user(context.superuser_name())) {
                Ok(user) => ("true", user.is_some()),
                Err(_) => ("false", false),
            },
            Err(_) => ("false", false),
        },
    };

    println!("database_readable: {readable}");
    println!(
        "superuser: {} ({})",
        context.superuser_name(),
        if superuser_present {
            "present"
        } else {
            "absent"
        }
    );
    Ok(())
}

fn driver_name(config: &microwave_config::Config) -> &'static str {
    match config.database.driver {
        microwave_config::DatabaseDriver::Sqlite => "sqlite",
        microwave_config::DatabaseDriver::Postgres => "postgresql",
    }
}

pub fn user_create(
    context: &Context,
    name: &str,
    email: Option<&str>,
    password: Option<String>,
    admin: bool,
) -> Result<(), CtlError> {
    let (password, generated) = match password {
        Some(password) => (password, false),
        None => (random_password(), true),
    };
    let email = email
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("{name}@localhost"));
    let hash = hash_password(&password)?;

    let store = context.open_metadata_store()?;
    let user = store.run(store.store().create_user(
        name,
        &email,
        &hash.hash_type,
        &hash.hash,
        &hash.salt,
        admin,
    ))?;

    println!("Created user {}", user.username);
    if generated {
        println!("Generated password for {}: {password}", user.username);
    }
    Ok(())
}

pub fn user_list(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    for user in store.run(store.store().list_users())? {
        println!("{}", user.username);
    }
    Ok(())
}

pub fn user_show(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, name)?;
    println!("username: {}", user.username);
    println!("email: {}", user.email.as_deref().unwrap_or(""));
    println!("admin: {}", user.admin);
    println!("id: {}", user.id);
    println!("authz_id: {}", user.authz_id);
    println!("created_at: {}", user.created_at);
    println!("updated_at: {}", user.updated_at);
    Ok(())
}

pub fn user_delete(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    store.run(store.store().delete_user(name))?;
    println!("Deleted user {name}");
    Ok(())
}

pub fn org_create(
    context: &Context,
    name: &str,
    full_name: Option<&str>,
    creator: Option<&str>,
) -> Result<(), CtlError> {
    let creator_name = creator.unwrap_or_else(|| context.superuser_name());
    let full_name = full_name.unwrap_or(name);

    let store = context.open_metadata_store()?;
    let creator = find_user(&store, creator_name).map_err(|error| match error {
        CtlError::Store(StoreError::NotFound(_)) => CtlError::message(format!(
            "creator user {creator_name} does not exist; create it first or pass --creator"
        )),
        other => other,
    })?;
    // The CLI is synchronous. The authorization policy lives behind async
    // repository traits, so bridge into the handle's current-thread runtime.
    let org = store.run(create_organization(
        store.store(),
        name,
        full_name,
        &creator.id,
        &creator.authz_id,
    ))?;
    println!("Created organization {} ({})", org.name, org.full_name);
    Ok(())
}

pub fn org_list(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    for org in store.run(store.store().list_orgs())? {
        println!("{}", org.name);
    }
    Ok(())
}

pub fn org_show(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, name)?;
    println!("name: {}", org.name);
    println!("full_name: {}", org.full_name);
    println!("id: {}", org.id);
    println!("authz_id: {}", org.authz_id);
    println!("created_at: {}", org.created_at);
    println!("updated_at: {}", org.updated_at);
    for user in store.run(store.store().list_org_users(&org.id))? {
        println!("user: {}", user.username);
    }
    Ok(())
}

pub fn org_delete(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, name)?;
    store.run(store.store().delete_org(&org.id))?;
    println!("Deleted organization {name}");
    Ok(())
}

pub fn org_user_add(context: &Context, org_name: &str, username: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, org_name)?;
    let user = find_user(&store, username)?;

    store.run(store.store().associate_user_org(&org.id, &user.id))?;
    if let Some(users_group) = store.run(store.store().find_group(&org.id, "users"))? {
        store.run(
            store
                .store()
                .add_group_member(&users_group.authz_id, &user.authz_id),
        )?;
    }

    println!("Added user {username} to organization {org_name}");
    Ok(())
}

pub fn org_user_remove(context: &Context, org_name: &str, username: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, org_name)?;
    let user = find_user(&store, username)?;

    store.run(
        store
            .store()
            .remove_user_from_org(&org.id, &user.id, &user.username),
    )?;
    println!("Removed user {username} from organization {org_name}");
    Ok(())
}

pub fn password(context: &Context, username: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    find_user(&store, username)?;

    let password = random_password();
    let hash = hash_password(&password)?;
    store.run(store.store().update_user(
        username,
        None,
        Some((&hash.hash, &hash.salt, &hash.hash_type)),
        None,
    ))?;

    println!("New password for {username}: {password}");
    Ok(())
}

pub fn reindex(org: Option<&str>) -> Result<(), CtlError> {
    match org {
        Some(org) => println!(
            "reindex: search reindex is not implemented for the local store; no changes were made for organization {org}"
        ),
        None => println!(
            "reindex: search reindex is not implemented for the local store; no changes were made"
        ),
    }
    Ok(())
}

pub fn migrate_from_legacy(context: &Context, input: &Path) -> Result<(), CtlError> {
    match context.config.database.driver {
        microwave_config::DatabaseDriver::Sqlite => {
            let database_path = context.sqlite_path()?;
            microwave_migrate::import_database(database_path, input)?;
            println!(
                "Imported migration snapshot from {} into {}",
                input.display(),
                database_path.display()
            );
        }
        microwave_config::DatabaseDriver::Postgres => {
            microwave_migrate::import_postgres(&context.database_url, input)?;
            println!(
                "Imported migration snapshot from {} into the configured PostgreSQL database",
                input.display()
            );
        }
    }
    Ok(())
}

/// Grant platform administrator privileges by setting `users.admin`.
///
/// The unified schema does not have a global `server-admins` group: the CLI
/// and HTTP surfaces already treat `users.admin` as the server-admin flag, so
/// these commands change that column rather than maintaining a parallel group.
pub fn grant_server_admin_permissions(context: &Context, username: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;
    if user.admin {
        println!("User {username} is already a server-admin.");
        return Ok(());
    }

    store.run(store.store().update_user(username, None, None, Some(true)))?;
    println!(
        "User {username} was added to server-admins. This user can now list, read, create, and delete users (even for organizations they are not members of)."
    );
    Ok(())
}

/// Remove platform administrator privileges by clearing `users.admin`.
///
/// The configured superuser cannot be demoted; doing so would leave the
/// deployment without an operator that can manage other administrators.
pub fn remove_server_admin_permissions(context: &Context, username: &str) -> Result<(), CtlError> {
    if username.eq_ignore_ascii_case(context.superuser_name()) {
        return Err(CtlError::message(format!(
            "You cannot remove the base superuser {} from server-admins.",
            context.superuser_name()
        )));
    }

    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;
    if !user.admin {
        return Err(CtlError::message(format!(
            "User {username} is not a member of server-admins so it cannot be removed."
        )));
    }

    store.run(store.store().update_user(username, None, None, Some(false)))?;
    println!(
        "User {username} was removed from the server-admins group. This user can no longer list, read, create, and delete users except where they have default organization permissions."
    );
    Ok(())
}

pub fn list_server_admins(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    for user in store.run(store.store().list_users())? {
        if user.admin {
            println!("{}", user.username);
        }
    }
    Ok(())
}

pub(crate) fn find_user(store: &StoreHandle, username: &str) -> Result<User, CtlError> {
    store
        .run(store.store().find_user(username))?
        .ok_or_else(|| StoreError::NotFound(format!("user {username}")).into())
}

pub(crate) fn find_org(store: &StoreHandle, name: &str) -> Result<Org, CtlError> {
    store
        .run(store.store().find_org(name))?
        .ok_or_else(|| StoreError::NotFound(format!("organization {name}")).into())
}

fn random_password() -> String {
    format!("mw-{}", Uuid::new_v4().simple())
}
