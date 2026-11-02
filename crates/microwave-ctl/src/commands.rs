use crate::{
    context::{Context, StoreHandle},
    secrets::KNOWN_CREDENTIALS,
    CtlError,
};
use microwave_authz::create_organization;
use microwave_crypto::{generate_rsa_keypair, hash_password, parse_public_key_pem};
use microwave_store::{ActorType, Org, StoreError, User, UserProfile};
use std::io::Write;
use std::path::{Path, PathBuf};
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

#[allow(clippy::too_many_arguments)]
pub fn user_create(
    context: &Context,
    positional: &[String],
    email_flag: Option<&str>,
    password_flag: Option<String>,
    admin: bool,
    filename: Option<&Path>,
    public_key_path: Option<&Path>,
) -> Result<(), CtlError> {
    let parsed = parse_user_create_positional(positional)?;
    let (password, generated_password) = match password_flag.or(parsed.password) {
        Some(password) => (password, false),
        None => (random_password(), true),
    };
    let email = email_flag
        .map(ToString::to_string)
        .or(parsed.email)
        .unwrap_or_else(|| format!("{}@localhost", parsed.name));
    let name = parsed.name;
    let first = parsed.first;
    let middle = parsed.middle;
    let last = parsed.last;

    let profile = match (&first, &last) {
        (Some(first), Some(last)) => {
            let mut display = first.clone();
            if let Some(middle) = &middle {
                display.push(' ');
                display.push_str(middle);
            }
            display.push(' ');
            display.push_str(last);
            UserProfile {
                display_name: Some(display),
                first_name: Some(first.clone()),
                middle_name: middle.clone(),
                last_name: Some(last.clone()),
                ..Default::default()
            }
        }
        // Preserve the flag-form behavior: the username is stored as the
        // display, first, and last name.
        _ => UserProfile {
            display_name: Some(name.clone()),
            first_name: Some(name.clone()),
            last_name: Some(name.clone()),
            ..Default::default()
        },
    };

    // Generate the key before creating the user so a key-generation failure
    // cannot leave a user without the default key the upstream v0 API creates.
    let (private_key, public_key) = prepare_default_key(public_key_path)?;
    let hash = hash_password(&password)?;

    let store = context.open_metadata_store()?;
    let user = store.run(store.store().create_user_with_profile(
        &name,
        Some(&email),
        &hash.hash_type,
        &hash.hash,
        &hash.salt,
        admin,
        &profile,
    ))?;

    if let Err(error) = store.run(store.store().add_actor_key(
        &user.id,
        ActorType::User,
        "default",
        &public_key,
    )) {
        // Keep user creation atomic from the operator's point of view; a user
        // without the expected default key is not a useful half-result.
        let _ = store.run(store.store().delete_user(&name));
        return Err(error.into());
    }

    println!("Created user {}", user.username);
    if generated_password {
        println!("Generated password for {}: {password}", user.username);
    }
    if let Some(private_key) = private_key {
        print_or_write_private_key(&format!("user {}", user.username), &private_key, filename)?;
    } else {
        println!("Added key default for user {}", user.username);
    }
    Ok(())
}

/// Parsed fields from the supported `user-create` forms.
struct UserCreatePositional {
    name: String,
    first: Option<String>,
    middle: Option<String>,
    last: Option<String>,
    email: Option<String>,
    password: Option<String>,
}

/// Parse the two supported `user-create` forms.
///
/// With no positional arguments after the username, the long flags supply the
/// email and password. The upstream form is either `USER FIRST LAST EMAIL
/// PASSWORD` (four arguments after the username) or `USER FIRST MIDDLE LAST
/// EMAIL PASSWORD` (five).
fn parse_user_create_positional(positional: &[String]) -> Result<UserCreatePositional, CtlError> {
    let Some(name) = positional.first() else {
        return Err(CtlError::message(
            "user-create requires USER_NAME; use `user-create USER_NAME --email EMAIL --password PASSWORD` or the upstream positional form",
        ));
    };
    let rest = &positional[1..];
    let (first, middle, last, email, password) = match rest.len() {
        0 => (None, None, None, None, None),
        4 => (
            Some(rest[0].clone()),
            None,
            Some(rest[1].clone()),
            Some(rest[2].clone()),
            Some(rest[3].clone()),
        ),
        5 => (
            Some(rest[0].clone()),
            Some(rest[1].clone()),
            Some(rest[2].clone()),
            Some(rest[3].clone()),
            Some(rest[4].clone()),
        ),
        _ => {
            return Err(CtlError::message(
                "user-create positional form is USER_NAME FIRST_NAME [MIDDLE_NAME] LAST_NAME EMAIL PASSWORD; use --email and --password for the flag form",
            ))
        }
    };
    Ok(UserCreatePositional {
        name: name.clone(),
        first,
        middle,
        last,
        email,
        password,
    })
}

/// Generate a keypair or read an existing public key.
fn prepare_default_key(
    public_key_path: Option<&Path>,
) -> Result<(Option<String>, String), CtlError> {
    match public_key_path {
        Some(path) => {
            let public_key = std::fs::read_to_string(path).map_err(|error| {
                CtlError::message(format!(
                    "failed to read public key {}: {error}",
                    path.display()
                ))
            })?;
            let public_key = public_key.trim().to_string();
            parse_public_key_pem(&public_key).map_err(|_| {
                CtlError::message(format!(
                    "public key at {} is not a valid PEM public key",
                    path.display()
                ))
            })?;
            Ok((None, public_key))
        }
        None => {
            let (private_key, public_key) = generate_rsa_keypair()?;
            Ok((Some(private_key), public_key))
        }
    }
}

/// Print a generated private key once, or write it to a 0600 file.
fn print_or_write_private_key(
    label: &str,
    private_key: &str,
    filename: Option<&Path>,
) -> Result<(), CtlError> {
    match filename {
        Some(path) => {
            write_private_key(path, private_key)?;
            println!("Wrote private key for {label} to {}", path.display());
        }
        None => {
            println!(
                "New private key for {label} (please store, it is not saved in the database):"
            );
            println!("{}", private_key.trim());
        }
    }
    Ok(())
}

fn write_private_key(path: &Path, private_key: &str) -> Result<(), CtlError> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(private_key.trim().as_bytes())?;
    file.write_all(b"\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
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
    association_user: Option<&str>,
    filename: Option<&Path>,
) -> Result<(), CtlError> {
    let creator_name = creator.unwrap_or_else(|| context.superuser_name());
    let full_name = full_name.unwrap_or(name);

    // Generate the validator key before creating any rows so a key generation
    // failure cannot leave a half-created organization behind.
    let (private_key, public_key) = generate_rsa_keypair()?;

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

    let client = match provision_validator(&store, &org, &creator.authz_id, &public_key) {
        Ok(client) => client,
        Err(error) => {
            let _ = store.run(store.store().delete_org(&org.id));
            return Err(error);
        }
    };

    println!("Created organization {} ({})", org.name, org.full_name);
    println!(
        "Created validator client {} with the default key",
        client.name
    );

    if let Some(username) = association_user {
        let user = find_user(&store, username)?;
        store.run(store.store().associate_user_org(&org.id, &user.id))?;
        for group_name in ["admins", "billing-admins"] {
            if let Some(group) = store.run(store.store().find_group(&org.id, group_name))? {
                store.run(
                    store
                        .store()
                        .add_group_member(&group.authz_id, &user.authz_id),
                )?;
            }
        }
        println!("Associated user {username} with organization {}", org.name);
    }

    print_or_write_private_key(&format!("client {}", client.name), &private_key, filename)?;
    Ok(())
}

fn provision_validator(
    store: &StoreHandle,
    org: &Org,
    creator_authz_id: &str,
    public_key: &str,
) -> Result<microwave_store::Client, CtlError> {
    let name = format!("{}-validator", org.name);
    let client = store.run(store.store().create_client_with_acl(
        &org.id,
        &name,
        true,
        false,
        Some(creator_authz_id),
    ))?;
    store.run(
        store
            .store()
            .add_actor_key(&client.id, ActorType::Client, "default", public_key),
    )?;
    if let Some(group) = store.run(store.store().find_group(&org.id, "clients"))? {
        store.run(
            store
                .store()
                .add_group_member(&group.authz_id, &client.authz_id),
        )?;
    }
    Ok(client)
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
    let org_id = org.id.clone();

    // Build the search backend before deleting the row so a remote backend can
    // still use the handle. Embedded search has no separate documents.
    let search =
        microwave_api::search::build_search_backend(&context.config.search, store.store_arc());

    store.run(store.store().delete_org(&org_id))?;

    remove_local_bookshelf_org(context, &org_id);
    match search {
        Ok(backend) if backend.supports_purge() => match store.run(backend.delete_index(&org_id)) {
            Ok(()) => println!("Removed remote search documents for organization {name}"),
            Err(error) => eprintln!(
                "warning: organization {name} was deleted, but its remote search documents could not be removed: {error}"
            ),
        },
        Ok(_) => {}
        Err(error) => eprintln!(
            "warning: organization {name} was deleted, but the search backend is not usable so remote documents were not removed: {error}"
        ),
    }

    println!("Deleted organization {name}");
    Ok(())
}

/// Remove `<data_dir>/bookshelf/organization-<id>` after the metadata row is
/// gone. The local backend is the only one with an on-disk prefix; a missing
/// tree (for example an S3 deployment) is not an error.
fn remove_local_bookshelf_org(context: &Context, org_id: &str) {
    let path = context
        .data_dir
        .join("bookshelf")
        .join(format!("organization-{org_id}"));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            if let Err(error) = std::fs::remove_dir_all(&path) {
                eprintln!(
                    "warning: could not remove local bookshelf prefix {}: {error}",
                    path.display()
                );
            }
        }
        Ok(_) => {
            // Never follow a symlink out of the data directory.
            let _ = std::fs::remove_file(&path);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "warning: could not inspect local bookshelf prefix {}: {error}",
            path.display()
        ),
    }
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

pub fn reindex(context: &Context, org: Option<&str>, purge: bool) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let backend =
        microwave_api::search::build_search_backend(&context.config.search, store.store_arc())
            .map_err(|error| CtlError::message(format!("search backend is not usable: {error}")))?;

    let report = store
        .run(microwave_api::search::reindex(
            backend.as_ref(),
            store.store(),
            org,
            purge,
        ))
        .map_err(|error| CtlError::message(format!("reindex failed: {error}")))?;

    println!("search_backend: {}", backend.name());
    match org {
        Some(org) => println!("organization: {org}"),
        None => println!("organizations: {}", report.organizations),
    }
    let embedded = backend.name() == "embedded";
    if embedded {
        println!(
            "relational search is authoritative; the embedded backend reads the store directly, so there is no external index to rebuild"
        );
    }
    println!("documents_acknowledged: {}", report.acknowledged_documents);
    println!("nodes: {}", report.nodes);
    println!("roles: {}", report.roles);
    println!("environments: {}", report.environments);
    println!("clients: {}", report.clients);
    println!("data_bags: {}", report.data_bags);
    println!("data_bag_items: {}", report.data_bag_items);
    if !embedded {
        if purge {
            println!("purged_organizations: {}", report.purged_organizations);
            println!(
                "note: --purge completed without reported delete failures and the rebuilt index was refreshed; verify search results before resuming traffic"
            );
        } else {
            println!(
                "note: reindex adds or replaces documents by id; documents for deleted objects remain in the remote index. Re-run with --purge to delete each organization's existing remote documents before reindexing"
            );
        }
    }
    Ok(())
}

/// Validate configuration, initialize or migrate the metadata backend, and
/// report the resolved backends.
///
/// This never restarts a server and never deletes metadata. Opening the store
/// applies the idempotent schema, so a fresh deployment is ready and an
/// existing deployment is migrated forward.
pub fn reconfigure(context: &Context) -> Result<(), CtlError> {
    crate::diagnostics::check_config(context)?;

    let store = context.open_metadata_store()?;
    let users = store.run(store.store().list_users())?;
    let orgs = store.run(store.store().list_orgs())?;

    println!("reconfigure: configuration is valid and the metadata backend is ready");
    println!("database_driver: {}", driver_name(&context.config));
    match &context.database_path {
        Some(path) => println!("database: {}", path.display()),
        None => println!(
            "database: PostgreSQL {}",
            microwave_migrate::redacted_postgres_url(&context.database_url)
        ),
    }
    println!(
        "search_backend: {}",
        crate::diagnostics::search_backend_name(context.config.search.backend)
    );
    println!(
        "object_store_backend: {}",
        crate::diagnostics::object_store_backend_name(context.config.object_store.backend)
    );
    println!("users: {}", users.len());
    println!("organizations: {}", orgs.len());
    println!(
        "note: a running microwave-server is not restarted; restart it to apply configuration changes."
    );
    Ok(())
}

pub fn migrate_from_legacy(context: &Context, input: &Path, yes: bool) -> Result<(), CtlError> {
    match context.config.database.driver {
        microwave_config::DatabaseDriver::Sqlite => {
            let database_path = context.sqlite_path()?;
            println!("Import target: sqlite:{}", database_path.display());
            crate::backup::confirm_destructive("import", yes)?;
            microwave_migrate::import_database_with_compression(
                database_path,
                input,
                context.store_compression(),
            )?;
            println!(
                "Imported migration snapshot from {} into {}",
                input.display(),
                database_path.display()
            );
        }
        microwave_config::DatabaseDriver::Postgres => {
            microwave_migrate::validate_postgres_url(&context.database_url)?;
            println!(
                "Import target: PostgreSQL {}",
                microwave_migrate::redacted_postgres_url(&context.database_url)
            );
            crate::backup::confirm_destructive("import", yes)?;
            microwave_migrate::import_postgres_with_compression(
                &context.database_url,
                input,
                context.store_compression(),
            )?;
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

/// Print every stored service credential.
///
/// This command intentionally reveals secret values, matching upstream's
/// `show-service-credentials` runbook behavior. Only credentials that have been
/// set are printed.
pub fn show_service_credentials(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let mut printed = false;
    for &(group, names) in KNOWN_CREDENTIALS {
        let mut values = Vec::new();
        for &name in names {
            if let Some(value) = store.run(store.store().get_secret(group, name))? {
                values.push((name, value));
            }
        }
        if values.is_empty() {
            continue;
        }
        printed = true;
        println!("{group}:");
        for (name, value) in values {
            println!("  {name}: {value}");
        }
    }
    if !printed {
        println!("No service credentials are stored.");
    }
    Ok(())
}

/// The sentinel file that marks the installation as disabled until credential
/// rotation.
pub(crate) fn credential_rotation_marker(context: &Context) -> PathBuf {
    context.data_dir.join("credential_rotation_required")
}

/// Write the credential-rotation sentinel without stopping systemd.
pub fn require_credential_rotation(context: &Context, yes: bool) -> Result<(), CtlError> {
    crate::backup::confirm_destructive("require-credential-rotation", yes)?;
    context.prepare_data_dir()?;
    let marker = credential_rotation_marker(context);
    std::fs::write(&marker, b"credential rotation required\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600))?;
    }
    println!(
        "The Microwave Server has been disabled until credentials have been rotated. \
         Run 'microwave-ctl rotate-shared-secrets' (or the chef-server-ctl alias) to rotate them."
    );
    println!("Wrote {}", marker.display());
    Ok(())
}

/// Remove the rotation sentinel. Called after a complete shared-secret
/// rotation.
pub fn clear_credential_rotation_marker(context: &Context) -> Result<(), CtlError> {
    let marker = credential_rotation_marker(context);
    match std::fs::remove_file(&marker) {
        Ok(()) => {
            println!("Cleared credential rotation requirement.");
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// List the services managed by this installation.
///
/// Microwave has a single systemd unit. Keeping this command honest is more
/// useful than emulating runit's multi-service output.
pub fn service_list(unit: Option<&str>) -> Result<(), CtlError> {
    let unit = unit
        .map(str::to_string)
        .or_else(|| std::env::var("MW_SERVICE_UNIT").ok())
        .filter(|unit| !unit.trim().is_empty())
        .unwrap_or_else(|| "microwave-server".to_string());
    println!("* {unit}");
    println!(
        "note: Microwave runs as a single systemd unit; use `service status` or `restart`/`tail` for this unit."
    );
    Ok(())
}

/// Remove local bookshelf blobs that no metadata row references.
///
/// The local object store lays blobs out as
/// `<data_dir>/bookshelf/organization-<org_id>/checksum-<checksum>`. The
/// `checksums` table is the authoritative set of committed cookbook and
/// cookbook-artifact checksums, so each on-disk file can be checked directly
/// through the backend-neutral repository. `--estimate-only` reports the same
/// set without deleting anything.
pub fn cleanup_orphan_blobs(context: &Context, estimate_only: bool) -> Result<(), CtlError> {
    let root = context.data_dir.join("bookshelf");
    if !root.is_dir() {
        println!("No local bookshelf directory exists; nothing to clean.");
        return Ok(());
    }

    let store = context.open_metadata_store()?;
    let mut scanned = 0usize;
    let mut referenced = 0usize;
    let mut orphaned = Vec::new();
    let mut orphaned_bytes = 0u64;

    let mut org_entries = std::fs::read_dir(&root)?.collect::<Result<Vec<_>, _>>()?;
    org_entries.sort_by_key(|entry| entry.file_name());
    for org_entry in org_entries {
        let file_type = org_entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        let org_name = org_entry.file_name();
        let org_name = org_name.to_string_lossy();
        let Some(org_id) = org_name.strip_prefix("organization-") else {
            continue;
        };

        let mut blob_entries =
            std::fs::read_dir(org_entry.path())?.collect::<Result<Vec<_>, _>>()?;
        blob_entries.sort_by_key(|entry| entry.file_name());
        for blob_entry in blob_entries {
            let file_type = blob_entry.file_type()?;
            // Skip symlinks and directories; only regular blob files were
            // written by the local object store.
            if !file_type.is_file() {
                continue;
            }
            let blob_name = blob_entry.file_name();
            let blob_name = blob_name.to_string_lossy();
            let Some(checksum) = blob_name.strip_prefix("checksum-") else {
                continue;
            };

            scanned += 1;
            if store.run(store.store().checksum_exists(org_id, checksum))? {
                referenced += 1;
            } else {
                let len = blob_entry
                    .metadata()
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                orphaned_bytes += len;
                orphaned.push(blob_entry.path());
            }
        }
    }

    if estimate_only {
        println!(
            "Local bookshelf: {}\nBlobs scanned: {scanned}\nReferenced blobs: {referenced}\nOrphaned blobs: {} ({orphaned_bytes} bytes)\nNothing deleted (--estimate-only).",
            root.display(),
            orphaned.len()
        );
        return Ok(());
    }

    let mut removed = 0usize;
    let mut removed_bytes = 0u64;
    for path in &orphaned {
        let len = std::fs::symlink_metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        match std::fs::remove_file(path) {
            Ok(()) => {
                removed += 1;
                removed_bytes += len;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CtlError::message(format!(
                    "failed to remove orphaned blob {}: {error}",
                    path.display()
                )))
            }
        }
    }

    // Best-effort removal of empty per-organization directories. The bookshelf
    // root itself is kept.
    let mut org_entries = std::fs::read_dir(&root)?.collect::<Result<Vec<_>, _>>()?;
    org_entries.sort_by_key(|entry| entry.file_name());
    for org_entry in org_entries {
        if org_entry
            .file_type()
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(org_entry.path());
        }
    }

    println!(
        "Local bookshelf: {}\nBlobs scanned: {scanned}\nReferenced blobs: {referenced}\nRemoved orphaned blobs: {removed} ({removed_bytes} bytes)",
        root.display()
    );
    Ok(())
}

fn random_password() -> String {
    format!("mw-{}", Uuid::new_v4().simple())
}
