use microwave_config::Config;
use microwave_crypto::{
    generate_rsa_keypair, hash_password, parse_private_key_pem, public_key_fingerprint,
    public_key_pem_from_private,
};
use microwave_store::{
    ActorType, MetadataStore, StoreError, BOOKSHELF_ACCESS_KEY_ID, BOOKSHELF_SECRET_ACCESS_KEY,
    BOOKSHELF_SECRET_GROUP, OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME, OTEL_SECRET_GROUP,
    OTEL_SECRET_NAME, WEBUI_PRIVATE_KEY_NAME, WEBUI_PUBLIC_KEY_NAME, WEBUI_SECRET_GROUP,
};
use std::path::Path;
use uuid::Uuid;

/// Ensure a platform superuser exists with a usable default key.
///
/// On first start this creates the configured `auth.superuser_name`, writes
/// its private key to `<data_dir>/<name>.pem`, and writes a random login
/// password to `<data_dir>/<name>.password`. Neither file is logged.
///
/// An existing deployment whose configured superuser does not exist is not
/// silently given a new account; the operator must create or rename it first.
/// Several replicas may start at once. When another replica wins the
/// `create_user` race this re-reads the existing row instead of failing.
pub async fn ensure_superuser(
    store: &dyn MetadataStore,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = Path::new(&config.paths.data_dir);
    std::fs::create_dir_all(data_dir)?;
    let superuser = config.auth.superuser_name.clone();

    let user = match store.find_user(&superuser).await? {
        Some(user) => user,
        None => {
            if !store.list_users().await?.is_empty() {
                return Err(format!(
                    "configured superuser {superuser:?} does not exist; create it with                      `microwave-ctl user-create {superuser}` or change auth.superuser_name"
                )
                .into());
            }
            let password = format!("mw-{}", Uuid::new_v4().simple());
            let hash = hash_password(&password)?;
            let created = store
                .create_user(
                    &superuser,
                    &format!("{superuser}@localhost"),
                    &hash.hash_type,
                    &hash.hash,
                    &hash.salt,
                    true,
                )
                .await;
            match created {
                Ok(user) => {
                    write_private_file(&data_dir.join(format!("{superuser}.password")), &password)?;
                    user
                }
                Err(StoreError::Conflict(_)) => store
                    .find_user(&superuser)
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
            Ok(()) => write_private_file(&data_dir.join(format!("{superuser}.pem")), &private_key)?,
            // Another replica already created the default key.
            Err(StoreError::Conflict(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

fn write_private_file(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    write_materialized_file(path, contents, 0o600)
}

fn write_materialized_file(path: &Path, contents: &str, mode: u32) -> Result<(), std::io::Error> {
    // A bare file name has an empty parent, which `DirBuilder` cannot create.
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_private_dirs(parent)?;
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no file name")
    })?;
    let temp = parent.join(format!(
        ".{}.tmp-{}",
        name.to_string_lossy(),
        Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options.open(&temp)?;
    use std::io::Write;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    // The creation-time mode above is masked by the process umask, and the
    // shipped units set `UMask=0077`. Set the exact mode before publishing the
    // file so the documented `0644` public key does not silently become `0600`.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&temp, path)?;
    Ok(())
}

fn create_private_dirs(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

/// Ensure a dedicated WebUI keypair exists and is materialized.
///
/// The metadata store is the source of truth: a stored key wins over the
/// import file, and a divergent materialized file is overwritten. Without a
/// stored key the configured import path is used, or a fresh pair is
/// generated. Returns the public PEM when `auth.webui_key` is enabled.
///
/// Only the public fingerprint is logged, never key material.
pub async fn ensure_webui_key(
    config: &Config,
    store: &dyn MetadataStore,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    ensure_webui_key_with(config, store).await
}

/// The secret-store seam of [`ensure_webui_key`].
///
/// Kept generic over the narrow trait so tests can inject a store whose
/// `get_secret` lies once, deterministically exercising the losing-replica
/// branch below.
async fn ensure_webui_key_with<S: microwave_store::SecretsRepository + ?Sized>(
    config: &Config,
    store: &S,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if !config.auth.webui_key {
        return Ok(None);
    }
    let private_path = webui_private_key_path(config);

    let (candidate, source) = match store
        .get_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME)
        .await?
    {
        Some(stored) => (stored, "secret:webui:private_key".to_string()),
        None if private_path.is_file() => (
            std::fs::read_to_string(&private_path)
                .map_err(|error| read_error(&private_path, &error))?,
            private_path.display().to_string(),
        ),
        None => (generate_rsa_keypair()?.0, "generated".to_string()),
    };
    let invalid = |source: &str, error: &dyn std::fmt::Display| {
        format!(
            "invalid webui private key from {source}: {error}; run `microwave-ctl rotate-credentials webui` to replace the pair"
        )
    };
    parse_private_key_pem(&candidate).map_err(|error| invalid(&source, &error))?;

    let retained = store
        .set_secret_if_absent(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, &candidate)
        .await?;
    if retained != candidate {
        // Another replica won the race. Validate the stored key so an invalid
        // value still produces the actionable message instead of a raw crypto
        // error on the next line.
        parse_private_key_pem(&retained)
            .map_err(|error| invalid("secret:webui:private_key", &error))?;
    }

    let public_pem = public_key_pem_from_private(&retained)?;
    store
        .set_secret(WEBUI_SECRET_GROUP, WEBUI_PUBLIC_KEY_NAME, &public_pem)
        .await?;

    let unchanged =
        std::fs::read_to_string(&private_path).is_ok_and(|existing| existing == retained);
    write_materialized_file(&private_path, &retained, 0o600)
        .map_err(|error| write_error(&private_path, &error))?;
    let public_path = private_path.with_file_name("webui_pub.pem");
    write_materialized_file(&public_path, &public_pem, 0o644)
        .map_err(|error| write_error(&public_path, &error))?;
    if !unchanged {
        tracing::info!(
            path = %private_path.display(),
            "materialized the webui private key from the metadata store"
        );
    }
    tracing::info!(
        fingerprint = %public_key_fingerprint(&public_pem)?,
        "webui keypair is enabled"
    );
    Ok(Some(public_pem))
}

fn webui_private_key_path(config: &Config) -> std::path::PathBuf {
    config
        .auth
        .webui_key_path
        .clone()
        .unwrap_or_else(|| Path::new(&config.paths.data_dir).join("webui_priv.pem"))
}

/// Context for a failed import read. The import file is opt-in, so the error
/// must name the file and both ways out instead of only the OS error.
fn read_error(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!(
        "cannot read webui private key from {}: {error}; the file must be readable by the \
         service account and contain PEM text, or run `microwave-ctl rotate-credentials webui` \
         to replace the pair",
        path.display()
    )
}

/// Context for a failed materialization write. The target directory is a
/// deployment concern more often than the key itself, so name it and the
/// systemd remediation.
fn write_error(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!(
        "cannot write webui key file {}: {error}; the nearest existing directory must be \
         writable by the service account and a path outside paths.data_dir needs a systemd \
         `ReadWritePaths=` drop-in (or use a writable subdirectory)",
        path.display()
    )
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
    // A legacy row may contain only whitespace. `get_secret` treats it as
    // absent, but `set_secret_if_absent` preserves it, so replace it instead
    // of handing `LocalObjectStore` an empty HMAC key.
    let secret = if secret.trim().is_empty() {
        store
            .set_secret(
                OBJECT_STORE_SECRET_GROUP,
                OBJECT_STORE_SECRET_NAME,
                &candidate,
            )
            .await?;
        candidate
    } else {
        if secret != candidate {
            tracing::info!(
                "another replica initialized the object-store secret; using the stored value"
            );
        }
        secret
    };
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

/// Fill `otel.headers` from the `otel`/`headers` secret when configuration and
/// environment do not provide headers.
///
/// The secret holds one `Name: value` entry per line, as
/// `microwave-ctl set-secret otel headers` writes it. The block is validated
/// again afterwards so a malformed stored value fails closed at startup instead
/// of failing on the first export. An absent secret is not an error.
pub async fn apply_otel_secrets(
    config: &mut Config,
    store: &dyn MetadataStore,
) -> Result<(), Box<dyn std::error::Error>> {
    if !config.otel.headers.is_empty() {
        return Ok(());
    }
    let Some(secret) = store
        .get_secret(OTEL_SECRET_GROUP, OTEL_SECRET_NAME)
        .await?
    else {
        return Ok(());
    };
    config.otel.headers = secret
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    config
        .otel
        .validate()
        .map_err(|(key, message)| format!("{key}: {message}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use microwave_store::SqliteStore;

    fn scratch_config(data_dir: &Path) -> Config {
        let mut config = Config::default();
        config.paths.data_dir = data_dir.to_string_lossy().into_owned();
        config.object_store.secret = None;
        config
    }

    #[tokio::test]
    async fn loads_otel_headers_from_the_secret_store() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret(
                OTEL_SECRET_GROUP,
                OTEL_SECRET_NAME,
                "Authorization: Bearer s3cr3t\n\nX-Scope: metrics",
            )
            .unwrap();
        let mut config = Config::default();

        apply_otel_secrets(&mut config, &store).await.unwrap();

        assert_eq!(
            config.otel.headers,
            vec!["Authorization: Bearer s3cr3t", "X-Scope: metrics"]
        );
    }

    #[tokio::test]
    async fn configured_otel_headers_win_over_the_secret() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret(
                OTEL_SECRET_GROUP,
                OTEL_SECRET_NAME,
                "Authorization: Bearer stored",
            )
            .unwrap();
        let mut config = Config::default();
        config.otel.headers = vec!["Authorization: Bearer configured".into()];

        apply_otel_secrets(&mut config, &store).await.unwrap();

        assert_eq!(
            config.otel.headers,
            vec!["Authorization: Bearer configured"]
        );
    }

    #[tokio::test]
    async fn an_absent_otel_headers_secret_is_not_an_error() {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut config = Config::default();

        apply_otel_secrets(&mut config, &store).await.unwrap();

        assert!(config.otel.headers.is_empty());
    }

    #[tokio::test]
    async fn rejects_a_malformed_otel_headers_secret() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret(OTEL_SECRET_GROUP, OTEL_SECRET_NAME, "malformed")
            .unwrap();
        let mut config = Config::default();

        let error = apply_otel_secrets(&mut config, &store)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("otel.headers"), "{error}");
    }

    #[tokio::test]
    async fn replaces_a_whitespace_only_object_store_secret() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret(OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME, "   ")
            .unwrap();
        let config = scratch_config(data_dir.path());

        let secret = ensure_object_store_secret(&config, &store)
            .await
            .expect("bootstrap secret");

        assert!(!secret.trim().is_empty(), "empty HMAC key would be unsafe");
        assert!(secret.starts_with("os-"));
        assert_eq!(
            store
                .get_secret(OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME)
                .unwrap()
                .as_deref(),
            Some(secret.as_str()),
            "the store must be healed, not just the return value"
        );
        assert_eq!(
            std::fs::read_to_string(data_dir.path().join("objectstore.secret")).unwrap(),
            secret
        );
    }

    #[tokio::test]
    async fn reuses_an_existing_non_empty_object_store_secret() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret(
                OBJECT_STORE_SECRET_GROUP,
                OBJECT_STORE_SECRET_NAME,
                "existing-secret",
            )
            .unwrap();
        let config = scratch_config(data_dir.path());

        let secret = ensure_object_store_secret(&config, &store)
            .await
            .expect("bootstrap secret");

        assert_eq!(secret, "existing-secret");
        assert_eq!(
            store
                .get_secret(OBJECT_STORE_SECRET_GROUP, OBJECT_STORE_SECRET_NAME)
                .unwrap()
                .as_deref(),
            Some("existing-secret")
        );
    }
}

#[cfg(test)]
mod superuser_tests {
    use super::*;
    use microwave_store::SqliteStore;

    #[tokio::test]
    async fn creates_the_configured_superuser() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.paths.data_dir = data_dir.path().to_string_lossy().into_owned();
        config.auth.superuser_name = "root".into();
        let store = SqliteStore::open_in_memory().unwrap();

        ensure_superuser(&store, &config).await.unwrap();

        let user = store
            .find_user("root")
            .unwrap()
            .expect("configured superuser");
        assert!(user.admin);
        assert!(store.find_user("pivotal").unwrap().is_none());
        assert!(!store
            .find_actor_keys(&user.id, ActorType::User)
            .unwrap()
            .is_empty());
        assert!(data_dir.path().join("root.pem").is_file());
        assert!(data_dir.path().join("root.password").is_file());
    }

    #[tokio::test]
    async fn refuses_to_bootstrap_a_missing_configured_superuser_on_an_existing_store() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.paths.data_dir = data_dir.path().to_string_lossy().into_owned();
        config.auth.superuser_name = "root".into();
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .create_user("pivotal", "pivotal@localhost", "bcrypt", "h", "s", true)
            .unwrap();

        let error = ensure_superuser(&store, &config)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("configured superuser"), "{error}");
    }
}

#[cfg(test)]
mod webui_tests {
    use super::*;
    use async_trait::async_trait;
    use microwave_crypto::{
        generate_rsa_keypair, public_key_fingerprint, public_key_pem_from_private,
    };
    use microwave_store::{
        SecretsRepository, SqliteStore, StoreError, WEBUI_PRIVATE_KEY_NAME, WEBUI_PUBLIC_KEY_NAME,
        WEBUI_SECRET_GROUP,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn webui_config(data_dir: &Path) -> Config {
        let mut config = Config::default();
        config.paths.data_dir = data_dir.to_string_lossy().into_owned();
        config.auth.webui_key = true;
        config
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn stored(store: &SqliteStore, name: &str) -> Option<String> {
        store.get_secret(WEBUI_SECRET_GROUP, name).unwrap()
    }

    /// A store whose first `get_secret` reports absence even though the key is
    /// already stored, standing in for a replica that read before the winner
    /// committed.
    struct LosingReplica<'a> {
        inner: &'a SqliteStore,
        forced_absent_once: AtomicBool,
    }

    #[async_trait]
    impl SecretsRepository for LosingReplica<'_> {
        async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
            if !self.forced_absent_once.swap(true, Ordering::SeqCst) {
                return Ok(None);
            }
            self.inner.get_secret(group, name)
        }

        async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
            self.inner.set_secret(group, name, value)
        }

        async fn set_secret_if_absent(
            &self,
            group: &str,
            name: &str,
            value: &str,
        ) -> Result<String, StoreError> {
            self.inner.set_secret_if_absent(group, name, value)
        }

        async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
            self.inner.remove_secret(group, name)
        }
    }

    #[tokio::test]
    async fn generates_and_materializes_on_first_start() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());

        let public_pem = ensure_webui_key(&config, &store).await.unwrap().unwrap();

        let private_pem = stored(&store, WEBUI_PRIVATE_KEY_NAME).unwrap();
        assert_eq!(
            stored(&store, WEBUI_PUBLIC_KEY_NAME).as_deref(),
            Some(public_pem.as_str())
        );
        assert_eq!(
            public_key_pem_from_private(&private_pem).unwrap(),
            public_pem
        );
        let private_path = data_dir.path().join("webui_priv.pem");
        let public_path = data_dir.path().join("webui_pub.pem");
        assert_eq!(std::fs::read_to_string(&private_path).unwrap(), private_pem);
        assert_eq!(std::fs::read_to_string(&public_path).unwrap(), public_pem);
        assert_eq!(mode(&private_path), 0o600);
        assert_eq!(mode(&public_path), 0o644);
    }

    #[tokio::test]
    async fn reuses_the_stored_key_and_overwrites_a_divergent_file() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());

        let first = ensure_webui_key(&config, &store).await.unwrap().unwrap();
        std::fs::write(data_dir.path().join("webui_priv.pem"), "not a key").unwrap();

        let second = ensure_webui_key(&config, &store).await.unwrap().unwrap();

        assert_eq!(first, second);
        let private_pem = stored(&store, WEBUI_PRIVATE_KEY_NAME).unwrap();
        assert_eq!(
            std::fs::read_to_string(data_dir.path().join("webui_priv.pem")).unwrap(),
            private_pem
        );
    }

    #[tokio::test]
    async fn imports_an_existing_webui_priv_pem() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        let (private_pem, _) = generate_rsa_keypair().unwrap();
        std::fs::write(data_dir.path().join("webui_priv.pem"), &private_pem).unwrap();

        let public_pem = ensure_webui_key(&config, &store).await.unwrap().unwrap();

        assert_eq!(
            stored(&store, WEBUI_PRIVATE_KEY_NAME).as_deref(),
            Some(private_pem.as_str())
        );
        assert_eq!(
            public_key_fingerprint(&public_pem).unwrap(),
            public_key_fingerprint(&public_key_pem_from_private(&private_pem).unwrap()).unwrap()
        );
    }

    #[tokio::test]
    async fn a_custom_path_is_the_only_import_source() {
        let data_dir = tempfile::tempdir().unwrap();
        let custom_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let mut config = webui_config(data_dir.path());
        let custom_path = custom_dir.path().join("webui_priv.pem");
        config.auth.webui_key_path = Some(custom_path.clone());

        let (data_dir_key, _) = generate_rsa_keypair().unwrap();
        let (custom_key, _) = generate_rsa_keypair().unwrap();
        std::fs::write(data_dir.path().join("webui_priv.pem"), &data_dir_key).unwrap();
        std::fs::write(&custom_path, &custom_key).unwrap();

        ensure_webui_key(&config, &store).await.unwrap().unwrap();

        assert_eq!(
            stored(&store, WEBUI_PRIVATE_KEY_NAME).as_deref(),
            Some(custom_key.as_str())
        );
        assert_eq!(std::fs::read_to_string(&custom_path).unwrap(), custom_key);
        assert_eq!(
            std::fs::read_to_string(data_dir.path().join("webui_priv.pem")).unwrap(),
            data_dir_key
        );
    }

    #[tokio::test]
    async fn creates_missing_parent_directories_with_0700() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let mut config = webui_config(data_dir.path());
        let custom_path = data_dir.path().join("nested/keys/webui_priv.pem");
        config.auth.webui_key_path = Some(custom_path.clone());

        ensure_webui_key(&config, &store).await.unwrap().unwrap();

        assert_eq!(mode(custom_path.parent().unwrap()), 0o700);
        assert_eq!(mode(&custom_path), 0o600);
        assert_eq!(mode(&custom_path.with_file_name("webui_pub.pem")), 0o644);
    }

    /// The umask is process-global, so this test changes it for the duration of
    /// one call and restores it. A umask of `0o077` is what the shipped units
    /// set; the explicit `chmod` before the rename must still yield `0644`.
    #[tokio::test]
    async fn materialized_modes_are_exact_under_a_0077_umask() {
        let previous = unsafe { libc::umask(0o077) };
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());

        let result = ensure_webui_key(&config, &store).await;
        unsafe { libc::umask(previous) };
        result.unwrap().unwrap();

        assert_eq!(mode(&data_dir.path().join("webui_priv.pem")), 0o600);
        assert_eq!(mode(&data_dir.path().join("webui_pub.pem")), 0o644);
    }

    #[tokio::test]
    async fn an_unreadable_import_file_names_the_path_and_the_fix() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        let path = data_dir.path().join("webui_priv.pem");
        // Non-UTF8 bytes fail the read regardless of whether the test runs as
        // root.
        std::fs::write(&path, [0xff, 0xfe]).unwrap();

        let error = ensure_webui_key(&config, &store)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("service account"), "{error}");
        assert!(error.contains("rotate-credentials webui"), "{error}");
    }

    #[tokio::test]
    async fn a_write_failure_names_the_path_and_the_fix() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let mut config = webui_config(data_dir.path());
        // The parent of the target does not exist and sits below a regular
        // file, so `create_dir_all` fails.
        let blocker = data_dir.path().join("blocked");
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("webui_priv.pem");
        config.auth.webui_key_path = Some(path.clone());

        let error = ensure_webui_key(&config, &store)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(error.contains("writable by the service account"), "{error}");
        assert!(error.contains("ReadWritePaths"), "{error}");
    }

    #[tokio::test]
    async fn an_invalid_stored_key_fails_with_an_actionable_error() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        store
            .set_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, "not a key")
            .unwrap();

        let error = ensure_webui_key(&config, &store)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("webui"), "{error}");
        assert!(error.contains("rotate-credentials webui"), "{error}");
    }

    #[tokio::test]
    async fn an_invalid_import_file_fails_with_an_actionable_error() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        std::fs::write(data_dir.path().join("webui_priv.pem"), "not a key").unwrap();

        let error = ensure_webui_key(&config, &store)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("webui"), "{error}");
        assert!(error.contains("rotate-credentials webui"), "{error}");
        assert!(stored(&store, WEBUI_PRIVATE_KEY_NAME).is_none());
    }

    #[tokio::test]
    async fn disabled_touches_nothing() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let mut config = webui_config(data_dir.path());
        config.auth.webui_key = false;
        let existing = data_dir.path().join("webui_priv.pem");
        std::fs::write(&existing, "pre-existing key material").unwrap();

        assert!(ensure_webui_key(&config, &store).await.unwrap().is_none());

        assert!(stored(&store, WEBUI_PRIVATE_KEY_NAME).is_none());
        assert!(stored(&store, WEBUI_PUBLIC_KEY_NAME).is_none());
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "pre-existing key material"
        );
        assert!(!data_dir.path().join("webui_pub.pem").exists());
    }

    #[tokio::test]
    async fn concurrent_bootstraps_converge_on_one_key() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::open_in_memory().unwrap());
        let config = webui_config(data_dir.path());

        let (left, right) = tokio::join!(
            ensure_webui_key(&config, store.as_ref()),
            ensure_webui_key(&config, store.as_ref())
        );
        let left = left.unwrap().unwrap();
        let right = right.unwrap().unwrap();

        assert_eq!(left, right);
        let private_pem = stored(&store, WEBUI_PRIVATE_KEY_NAME).unwrap();
        assert_eq!(
            std::fs::read_to_string(data_dir.path().join("webui_priv.pem")).unwrap(),
            private_pem
        );
        assert_eq!(public_key_pem_from_private(&private_pem).unwrap(), left);
    }

    #[tokio::test]
    async fn a_losing_race_uses_the_retained_key_not_its_own_candidate() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        // The winner's key is already stored, but the racing replica reads an
        // absent secret and would otherwise import the stale file below.
        let (stored_private, _) = generate_rsa_keypair().unwrap();
        store
            .set_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, &stored_private)
            .unwrap();
        let (candidate_private, candidate_public) = generate_rsa_keypair().unwrap();
        std::fs::write(data_dir.path().join("webui_priv.pem"), &candidate_private).unwrap();
        let racing = LosingReplica {
            inner: &store,
            forced_absent_once: AtomicBool::new(false),
        };

        let public_pem = ensure_webui_key_with(&config, &racing)
            .await
            .unwrap()
            .unwrap();

        assert_ne!(candidate_public, public_pem, "must not use its candidate");
        assert_eq!(
            public_key_fingerprint(&public_pem).unwrap(),
            public_key_fingerprint(&public_key_pem_from_private(&stored_private).unwrap()).unwrap(),
            "the public key must be derived from the retained private key"
        );
        assert_eq!(
            stored(&store, WEBUI_PRIVATE_KEY_NAME).as_deref(),
            Some(stored_private.as_str())
        );
        assert_eq!(
            stored(&store, WEBUI_PUBLIC_KEY_NAME).as_deref(),
            Some(public_pem.as_str())
        );
        assert_eq!(
            std::fs::read_to_string(data_dir.path().join("webui_priv.pem")).unwrap(),
            stored_private,
            "the divergent file must be overwritten with the retained key"
        );
    }

    #[tokio::test]
    async fn a_losing_race_validates_the_retained_key() {
        let data_dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open_in_memory().unwrap();
        let config = webui_config(data_dir.path());
        store
            .set_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, "not a key")
            .unwrap();
        // A valid candidate would pass the first validation; the invalid stored
        // key must still produce the actionable message.
        let (candidate_private, _) = generate_rsa_keypair().unwrap();
        std::fs::write(data_dir.path().join("webui_priv.pem"), &candidate_private).unwrap();
        let racing = LosingReplica {
            inner: &store,
            forced_absent_once: AtomicBool::new(false),
        };

        let error = ensure_webui_key_with(&config, &racing)
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("webui"), "{error}");
        assert!(error.contains("rotate-credentials webui"), "{error}");
    }
}
