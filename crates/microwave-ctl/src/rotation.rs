//! Service credential rotation commands.
//!
//! Values are generated locally and written through the backend-neutral
//! [`SecretsRepository`](microwave_store::SecretsRepository) contract. No
//! command in this module prints a generated value.

use crate::{
    context::{Context, StoreHandle},
    secrets::{set_webui_pair, EXTRA_CREDENTIALS, KNOWN_CREDENTIALS},
    CtlError,
};
use microwave_crypto::generate_rsa_keypair;
use microwave_store::{WEBUI_PRIVATE_KEY_NAME, WEBUI_PUBLIC_KEY_NAME, WEBUI_SECRET_GROUP};
use uuid::Uuid;

/// Private half of the `push-jobs-server` keypair.
const PUSHY_PRIVATE_KEY: &str = "pushy_priv_key";
/// Public half of the `push-jobs-server` keypair.
const PUSHY_PUBLIC_KEY: &str = "pushy_pub_key";
/// Group that holds secrets shared by every service.
const SHARED_GROUP: &str = "shared";

/// Rotate every known credential in one service group.
pub fn rotate_credentials(context: &Context, service: &str) -> Result<(), CtlError> {
    // Validate the group before touching the store so a typo cannot partially
    // rewrite credentials.
    let names = group_credentials(service)?;
    let store = context.open_metadata_store()?;
    rotate_group(&store, service, names)?;
    if service == WEBUI_SECRET_GROUP {
        println!(
            "WARNING: the WebUI keypair has changed. Restart the server and update every WebUI\n\
             (for example cinc-console) with the new private key before the old one stops working."
        );
    }
    Ok(())
}

/// Rotate every known service credential.
///
/// The shared secret/salt pair is intentionally excluded: only
/// [`rotate_shared_secrets`] changes it, together with the credential version.
pub fn rotate_all_credentials(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    for &(group, names) in KNOWN_CREDENTIALS {
        rotate_group(&store, group, names)?;
    }
    Ok(())
}

/// Generate a new shared secret and salt, rotate every service credential, and
/// reset the shared credential version to zero.
///
/// Nothing is restarted: the operator must distribute the updated secrets and
/// reconfigure the server.
pub fn rotate_shared_secrets(context: &Context) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;

    store_rotated_secret(&store, SHARED_GROUP, "secret", &random_hex())?;
    store_rotated_secret(&store, SHARED_GROUP, "salt", &random_hex())?;

    for &(group, names) in KNOWN_CREDENTIALS {
        rotate_group(&store, group, names)?;
    }

    store_rotated_secret(&store, SHARED_GROUP, "credential_version", "0")?;

    println!(
        "WARNING: the shared secrets have changed. Copy the updated secrets file \
         (or restore a verified backup) to every Chef Infra Server node, update \
         any running nodes, and restart/reconfigure the server before continuing."
    );
    Ok(())
}

/// Rotate the credentials in one group.
///
/// Keypairs are special: rotating either half regenerates and stores both at
/// once. The `webui` pair goes through [`set_webui_pair`], which restores the
/// old private half if the public write fails.
fn rotate_group(store: &StoreHandle, group: &str, names: &[&str]) -> Result<(), CtlError> {
    let mut rotated_pushy_pair = false;
    let mut rotated_webui_pair = false;

    for &name in names {
        if group == "push-jobs-server" && (name == PUSHY_PRIVATE_KEY || name == PUSHY_PUBLIC_KEY) {
            if !rotated_pushy_pair {
                let (private_pem, public_pem) = generate_rsa_keypair()?;
                // Key material is never printed, not even the credential names
                // that carry it.
                store_secret(store, group, PUSHY_PRIVATE_KEY, &private_pem)?;
                store_secret(store, group, PUSHY_PUBLIC_KEY, &public_pem)?;
                rotated_pushy_pair = true;
            }
            continue;
        }

        if group == WEBUI_SECRET_GROUP
            && (name == WEBUI_PRIVATE_KEY_NAME || name == WEBUI_PUBLIC_KEY_NAME)
        {
            if !rotated_webui_pair {
                let (private_pem, public_pem) = generate_rsa_keypair()?;
                store
                    .run(set_webui_pair(store.store(), &private_pem, &public_pem))
                    .map_err(|error| {
                        CtlError::message(format!(
                            "failed to rotate {group}.{WEBUI_PRIVATE_KEY_NAME}: {error}"
                        ))
                    })?;
                println!("Rotated {group}.{WEBUI_PRIVATE_KEY_NAME}");
                println!("Rotated {group}.{WEBUI_PUBLIC_KEY_NAME}");
                rotated_webui_pair = true;
            }
            continue;
        }

        store_rotated_secret(store, group, name, &generated_value(group, name))?;
    }

    Ok(())
}

/// Return the credential names for a known service group.
fn group_credentials(group: &str) -> Result<&'static [&'static str], CtlError> {
    KNOWN_CREDENTIALS
        .iter()
        .chain(EXTRA_CREDENTIALS.iter())
        .find(|(known_group, _)| *known_group == group)
        .map(|(_, names)| *names)
        .ok_or_else(|| {
            let groups = KNOWN_CREDENTIALS
                .iter()
                .chain(EXTRA_CREDENTIALS.iter())
                .map(|(known_group, _)| *known_group)
                .collect::<Vec<_>>()
                .join(", ");
            CtlError::message(format!(
                "unknown credential group '{group}'; known groups: {groups}"
            ))
        })
}

/// Generate a new value for one credential.
///
/// `oc_bifrost.superuser_id` is an identifier rather than a password, so it
/// keeps the UUID's hyphenated form. All other values are 64 lowercase hex
/// characters derived from two random UUIDs.
fn generated_value(group: &str, name: &str) -> String {
    match (group, name) {
        ("oc_bifrost", "superuser_id") | ("saml", "client_id") => Uuid::new_v4().to_string(),
        _ => random_hex(),
    }
}

/// 64 lowercase hex characters from two random UUIDs, without new dependencies.
fn random_hex() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// Store a value and report the credential name, never the value.
fn store_rotated_secret(
    store: &StoreHandle,
    group: &str,
    name: &str,
    value: &str,
) -> Result<(), CtlError> {
    store_secret(store, group, name, value)?;
    println!("Rotated {group}.{name}");
    Ok(())
}

/// Store a value without printing anything.
fn store_secret(store: &StoreHandle, group: &str, name: &str, value: &str) -> Result<(), CtlError> {
    store
        .run(store.store().set_secret(group, name, value))
        .map_err(|error| CtlError::message(format!("failed to rotate {group}.{name}: {error}")))
}
