use crate::commands::{find_org, find_user};
use crate::{
    context::{Context, StoreHandle},
    CtlError,
};
use chrono::{DateTime, SecondsFormat, Utc};
use microwave_crypto::{generate_rsa_keypair, parse_public_key_pem, public_key_fingerprint};
use microwave_store::{ActorType, StoreError};
use std::path::Path;

/// Options shared by `add-client-key` and `add-user-key`.
pub struct AddKeyOptions<'a> {
    pub key_name: Option<&'a str>,
    pub expiration_date: &'a str,
    pub public_key_path: Option<&'a Path>,
}

pub fn add_client_key(
    context: &Context,
    org_name: &str,
    client_name: &str,
    options: &AddKeyOptions<'_>,
) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, org_name)?;
    let client = store
        .run(store.store().find_client(&org.id, client_name))?
        .ok_or_else(|| StoreError::NotFound(format!("client {client_name}")))?;

    add_key(
        &store,
        &client.id,
        ActorType::Client,
        &format!("client {client_name} in organization {org_name}"),
        options,
    )
}

pub fn add_user_key(
    context: &Context,
    username: &str,
    options: &AddKeyOptions<'_>,
) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;

    add_key(
        &store,
        &user.id,
        ActorType::User,
        &format!("user {username}"),
        options,
    )
}

fn add_key(
    store: &StoreHandle,
    actor_id: &str,
    actor_type: ActorType,
    actor_description: &str,
    options: &AddKeyOptions<'_>,
) -> Result<(), CtlError> {
    let expires_at = parse_expiration(options.expiration_date)?;

    let (public_key, private_key) = match options.public_key_path {
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
            (public_key, None)
        }
        None => {
            let (private_key, public_key) = generate_rsa_keypair()?;
            (public_key, Some(private_key))
        }
    };

    let key_name = match options.key_name {
        Some(name) => {
            validate_key_name(name)?;
            name.to_string()
        }
        None => public_key_fingerprint(&public_key)?,
    };

    store.run(store.store().create_actor_key(
        actor_id,
        actor_type,
        &key_name,
        &public_key,
        &expires_at,
    ))?;
    println!("Created key {key_name} for {actor_description}");

    if let Some(private_key) = private_key {
        println!(
            "New private key for key named {key_name} (please store, it is not saved in the database):"
        );
        println!("{}", private_key.trim());
    }
    Ok(())
}

pub fn list_client_keys(
    context: &Context,
    org_name: &str,
    client_name: &str,
    verbose: bool,
) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, org_name)?;
    let client = store
        .run(store.store().find_client(&org.id, client_name))?
        .ok_or_else(|| StoreError::NotFound(format!("client {client_name}")))?;
    let keys = store.run(store.store().list_actor_keys(&client.id, ActorType::Client))?;
    print_keys(&keys, verbose);
    Ok(())
}

pub fn list_user_keys(context: &Context, username: &str, verbose: bool) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;
    let keys = store.run(store.store().list_actor_keys(&user.id, ActorType::User))?;
    print_keys(&keys, verbose);
    Ok(())
}

pub fn delete_client_key(
    context: &Context,
    org_name: &str,
    client_name: &str,
    key_name: &str,
) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let org = find_org(&store, org_name)?;
    let client = store
        .run(store.store().find_client(&org.id, client_name))?
        .ok_or_else(|| StoreError::NotFound(format!("client {client_name}")))?;
    store.run(
        store
            .store()
            .delete_actor_key(&client.id, ActorType::Client, key_name),
    )?;
    println!("Deleted key {key_name} for client {client_name} in organization {org_name}");
    Ok(())
}

pub fn delete_user_key(context: &Context, username: &str, key_name: &str) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let user = find_user(&store, username)?;
    store.run(
        store
            .store()
            .delete_actor_key(&user.id, ActorType::User, key_name),
    )?;
    println!("Deleted key {key_name} for user {username}");
    Ok(())
}

fn print_keys(keys: &[microwave_store::ActorKey], verbose: bool) {
    for key in keys {
        println!("name: {}", key.key_name);
        println!("expired: {}", key.expired);
        if verbose {
            println!("expiration_date: {}", key.expires_at);
            println!("public_key:");
            println!("{}", key.public_key.trim());
        }
        println!();
    }
}

fn parse_expiration(value: &str) -> Result<String, CtlError> {
    if value == "infinity" {
        return Ok("infinity".into());
    }
    let parsed = DateTime::parse_from_rfc3339(value).map_err(|_| {
        CtlError::message(
            "--expiration-date must be \"infinity\" or an ISO-8601 timestamp such as 2050-12-24T21:00:00Z",
        )
    })?;
    Ok(parsed
        .with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn validate_key_name(name: &str) -> Result<(), CtlError> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "_-:.".contains(character));
    if valid {
        Ok(())
    } else {
        Err(CtlError::message(
            "a key name can contain only A-Z, a-z, 0-9, _, -, :, or .",
        ))
    }
}
