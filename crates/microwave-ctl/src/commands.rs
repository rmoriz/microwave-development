use crate::{context::Context, CtlError};
use microwave_authz::create_organization;
use microwave_crypto::hash_password;
use microwave_store::{Org, SqliteStore, StoreError, User};
use uuid::Uuid;

pub fn status(context: &Context) -> Result<(), CtlError> {
    context.prepare_data_dir()?;

    println!("data_dir: {}", context.data_dir.display());
    println!("database: {}", context.database_path.display());

    let (readable, superuser_present) = if context.database_path.is_file() {
        match context.open_store() {
            Ok(store) => {
                let present = store.find_user(context.superuser_name())?.is_some();
                (true, present)
            }
            Err(_) => (false, false),
        }
    } else {
        (false, false)
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

    let store = context.open_store()?;
    let user = store.create_user(name, &email, &hash.hash_type, &hash.hash, &hash.salt, admin)?;

    println!("Created user {}", user.username);
    if generated {
        println!("Generated password for {}: {password}", user.username);
    }
    Ok(())
}

pub fn user_list(context: &Context) -> Result<(), CtlError> {
    let store = context.open_store()?;
    for user in store.list_users()? {
        println!("{}", user.username);
    }
    Ok(())
}

pub fn user_show(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
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
    let store = context.open_store()?;
    store.delete_user(name)?;
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

    let store = context.open_store()?;
    let creator = store.find_user(creator_name)?.ok_or_else(|| {
        CtlError::message(format!(
            "creator user {creator_name} does not exist; create it first or pass --creator"
        ))
    })?;
    let org = create_organization(&store, name, full_name, &creator.id, &creator.authz_id)?;
    println!("Created organization {} ({})", org.name, org.full_name);
    Ok(())
}

pub fn org_list(context: &Context) -> Result<(), CtlError> {
    let store = context.open_store()?;
    for org in store.list_orgs()? {
        println!("{}", org.name);
    }
    Ok(())
}

pub fn org_show(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
    let org = find_org(&store, name)?;
    println!("name: {}", org.name);
    println!("full_name: {}", org.full_name);
    println!("id: {}", org.id);
    println!("authz_id: {}", org.authz_id);
    println!("created_at: {}", org.created_at);
    println!("updated_at: {}", org.updated_at);
    for user in store.list_org_users(&org.id)? {
        println!("user: {}", user.username);
    }
    Ok(())
}

pub fn org_delete(context: &Context, name: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
    let org = find_org(&store, name)?;
    store.delete_org(&org.id)?;
    println!("Deleted organization {name}");
    Ok(())
}

pub fn org_user_add(context: &Context, org_name: &str, username: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
    let org = find_org(&store, org_name)?;
    let user = find_user(&store, username)?;

    store.associate_user_org(&org.id, &user.id)?;
    if let Some(users_group) = store.find_group(&org.id, "users")? {
        store.add_group_member(&users_group.authz_id, &user.authz_id)?;
    }

    println!("Added user {username} to organization {org_name}");
    Ok(())
}

pub fn org_user_remove(context: &Context, org_name: &str, username: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
    let org = find_org(&store, org_name)?;
    let user = find_user(&store, username)?;

    store.remove_user_from_org(&org.id, &user.id, &user.authz_id)?;
    println!("Removed user {username} from organization {org_name}");
    Ok(())
}

pub fn password(context: &Context, username: &str) -> Result<(), CtlError> {
    let store = context.open_store()?;
    find_user(&store, username)?;

    let password = random_password();
    let hash = hash_password(&password)?;
    store.update_user(
        username,
        None,
        Some((&hash.hash, &hash.salt, &hash.hash_type)),
        None,
    )?;

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

fn find_user(store: &SqliteStore, username: &str) -> Result<User, CtlError> {
    store
        .find_user(username)?
        .ok_or_else(|| StoreError::NotFound(format!("user {username}")).into())
}

fn find_org(store: &SqliteStore, name: &str) -> Result<Org, CtlError> {
    store
        .find_org(name)?
        .ok_or_else(|| StoreError::NotFound(format!("organization {name}")).into())
}

fn random_password() -> String {
    format!("mw-{}", Uuid::new_v4().simple())
}
