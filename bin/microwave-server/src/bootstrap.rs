use microwave_config::Config;
use microwave_crypto::{generate_rsa_keypair, hash_password};
use microwave_store::{ActorType, SqliteStore};
use std::path::Path;
use uuid::Uuid;

/// Ensure a platform superuser exists with a usable default key.
///
/// On first start this creates `pivotal`, writes its private key to
/// `<data_dir>/pivotal.pem`, and writes a random login password to
/// `<data_dir>/pivotal.password`. Neither file is logged.
pub fn ensure_superuser(
    store: &SqliteStore,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = Path::new(&config.paths.data_dir);
    std::fs::create_dir_all(data_dir)?;

    let user = match store.find_user("pivotal")? {
        Some(user) => user,
        None => {
            let password = format!("mw-{}", Uuid::new_v4().simple());
            let hash = hash_password(&password)?;
            let user = store.create_user(
                "pivotal",
                "pivotal@localhost",
                &hash.hash_type,
                &hash.hash,
                &hash.salt,
                true,
            )?;
            write_private_file(&data_dir.join("pivotal.password"), &password)?;
            user
        }
    };

    let keys = store.find_actor_keys(&user.id, ActorType::User)?;
    if keys.is_empty() {
        let (private_key, public_key) = generate_rsa_keypair()?;
        store.add_actor_key(&user.id, ActorType::User, "default", &public_key)?;
        write_private_file(&data_dir.join("pivotal.pem"), &private_key)?;
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
pub fn ensure_object_store_secret(config: &Config) -> Result<String, Box<dyn std::error::Error>> {
    if let Some(secret) = &config.object_store.secret {
        if !secret.is_empty() {
            return Ok(secret.clone());
        }
    }

    let path = Path::new(&config.paths.data_dir).join("objectstore.secret");
    if path.is_file() {
        let secret = std::fs::read_to_string(&path)?;
        if !secret.trim().is_empty() {
            return Ok(secret.trim().to_string());
        }
    }

    let secret = format!("os-{}", Uuid::new_v4().simple());
    write_private_file(&path, &secret)?;
    Ok(secret)
}
