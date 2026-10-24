use microwave_config::Config;
use microwave_crypto::{generate_rsa_keypair, hash_password};
use microwave_store::{
    ActorType, MetadataStore, StoreError, BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY,
    BOOKSHELF_SECRET_GROUP, OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME,
};
use std::path::Path;
use uuid::Uuid;

/// Ensure a platform superuser exists with a usable default key.
///
/// On first start this creates `pivotal`, writes its private key to
/// `<data_dir>/pivotal.pem`, and writes a random login password to
/// `<data_dir>/pivotal.password`. Neither file is logged.
///
/// Several replicas may start at once. When another replica wins the
/// `create_user` race this re-reads the existing row instead of failing.
pub async fn ensure_superuser(
    store: &dyn MetadataStore,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = Path::new(&config.paths.data_dir);
    std::fs::create_dir_all(data_dir)?;

    let user = match store.find_user("pivotal").await? {
        Some(user) => user,
        None => {
            let password = format!("mw-{}", Uuid::new_v4().simple());
            let hash = hash_password(&password)?;
            let created = store
                .create_user(
                    "pivotal",
                    "pivotal@localhost",
                    &hash.hash_type,
                    &hash.hash,
                    &hash.salt,
                    true,
                )
                .await;
            match created {
                Ok(user) => {
                    write_private_file(&data_dir.join("pivotal.password"), &password)?;
                    user
                }
                Err(StoreError::Conflict(_)) => store
                    .find_user("pivotal")
                    .await?
                    .ok_or("superuser creation lost a race but the user could not be read back")?,
                Err(error) => return Err(error.into()),
            }
        }
    };

    let keys = store.find_actor_keys(&user.id, ActorType::User).await?;
    if keys.is_empty() {
        let (private_key, public_key) = generate_rsa_keypair()?;
        match store
            .add_actor_key(&user.id, ActorType::User, "default", &public_key)
            .await
        {
            Ok(()) => write_private_file(&data_dir.join("pivotal.pem"), &private_key)?,
            // Another replica already created the default key.
            Err(StoreError::Conflict(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Load the local object-store HMAC secret, creating it on first start.
///
/// Precedence is explicit configuration, then the `object_store.secret` entry
/// in the metadata store, then `<data_dir>/objectstore.secret`, and finally a
/// freshly generated value. File-backed values are copied into the secret
/// store so `microwave-ctl show-secret object_store secret` can report them.
///
/// The store write uses [`MetadataStore`]'s atomic `set_secret_if_absent`
/// primitive, so concurrent first starts of several replicas converge on a
/// single secret instead of overwriting each other.
pub async fn ensure_object_store_secret(
    config: &Config,
    store: &dyn MetadataStore,
) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(secret) = &config.object_store.secret {
        if !secret.is_empty() {
            return Ok(secret.clone());
        }
    }

    if let Some(secret) = store
        .get_secret(OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME)
        .await?
    {
        if !secret.trim().is_empty() {
            return Ok(secret);
        }
    }

    let path = Path::new(&config.paths.data_dir).join("objectstore.secret");
    let candidate = if path.is_file() {
        let secret = std::fs::read_to_string(&path)?;
        if secret.trim().is_empty() {
            format!("os-{}", Uuid::new_v4().simple())
        } else {
            secret.trim().to_string()
        }
    } else {
        format!("os-{}", Uuid::new_v4().simple())
    };
    let secret = store
        .set_secret_if_absent(
            OBJECT_STORE_SECRET_GROUP,
            OBJECT_STORE_SECRET_NAME,
            &candidate,
        )
        .await?;
    if secret != candidate {
        tracing::info!(
            "another replica initialized the object-store secret; using the stored value"
        );
    }
    write_private_file(&path, &secret)?;
    Ok(secret)
}

/// Fill S3/Garage credentials from the secret store when configuration does
/// not provide them. Explicit configuration always wins.
pub async fn apply_bookshelf_secrets(
    config: &mut Config,
    store: &dyn MetadataStore,
) -> Result<(), Box<dyn std::error::Error>> {
    if config
        .object_store
        .access_key
        .as_deref()
        .is_none_or(str::is_empty)
    {
        config.object_store.access_key = store
            .get_secret(BOOKSHELF_SECRET_GROUP, BOOKSHELF_ACCESS_KEY_ID)
            .await?;
    }
    if config
        .object_store
        .secret_key
        .as_deref()
        .is_none_or(str::is_empty)
    {
        config.object_store.secret_key = store
            .get_secret(BOOKSHELF_SECRET_GROUP, BOOKSHELF_SECRET_ACCESS_KEY)
            .await?;
    }
    Ok(())
}
