use crate::{context::Context, CtlError};
use microwave_crypto::public_key_pem_from_private;
use microwave_store::{
    SecretsRepository, StoreError, WEBUI_PRIVATE_KEY_NAME, WEBUI_PUBLIC_KEY_NAME,
    WEBUI_SECRET_GROUP,
};

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

/// Credentials accepted by the generic secret commands but excluded from
/// `show-service-credentials` and `rotate-all`.
pub(crate) const EXTRA_CREDENTIALS: &[(&str, &[&str])] = &[(
    WEBUI_SECRET_GROUP,
    &[WEBUI_PRIVATE_KEY_NAME, WEBUI_PUBLIC_KEY_NAME],
)];

/// Store a secret. The value is never echoed back to the terminal.
pub fn set_secret(context: &Context, group: &str, name: &str, value: &str) -> Result<(), CtlError> {
    validate_credential(group, name)?;
    if group == WEBUI_SECRET_GROUP {
        return set_webui_secret(context, name, value);
    }
    let store = context.open_metadata_store()?;
    store.run(store.store().set_secret(group, name, value))?;
    println!("Secret {group}.{name} updated.");
    Ok(())
}

/// Store the WebUI private key and derive the public half from it.
fn set_webui_secret(context: &Context, name: &str, value: &str) -> Result<(), CtlError> {
    if name == WEBUI_PUBLIC_KEY_NAME {
        return Err(CtlError::message(
            "webui.public_key is derived from webui.private_key; set webui.private_key instead",
        ));
    }
    let public_pem = public_key_pem_from_private(value).map_err(|error| {
        CtlError::message(format!(
            "webui.private_key is not a valid PEM private key: {error}"
        ))
    })?;
    let store = context.open_metadata_store()?;
    store.run(set_webui_pair(store.store(), value, &public_pem))?;
    println!("Secret {WEBUI_SECRET_GROUP}.{WEBUI_PRIVATE_KEY_NAME} updated.");
    println!("Secret {WEBUI_SECRET_GROUP}.{WEBUI_PUBLIC_KEY_NAME} updated.");
    Ok(())
}

/// Write both halves of the WebUI keypair, keeping the store consistent on a
/// best-effort basis.
///
/// The secret repository has no multi-write transaction. If the public write
/// fails, the previous private value is restored (or removed when there was
/// none) and the original error is returned. When the restore itself fails, the
/// returned error names both failures.
///
/// This is best effort within one command only. A hard failure is reconciled on
/// the next server start: bootstrap retains the stored private key and derives
/// the public half from it again.
pub(crate) async fn set_webui_pair(
    store: &(impl SecretsRepository + ?Sized),
    private_pem: &str,
    public_pem: &str,
) -> Result<(), StoreError> {
    let previous = store
        .get_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME)
        .await?;
    store
        .set_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, private_pem)
        .await?;
    if let Err(error) = store
        .set_secret(WEBUI_SECRET_GROUP, WEBUI_PUBLIC_KEY_NAME, public_pem)
        .await
    {
        let restored = match previous.as_deref() {
            Some(previous) => {
                store
                    .set_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME, previous)
                    .await
            }
            None => {
                store
                    .remove_secret(WEBUI_SECRET_GROUP, WEBUI_PRIVATE_KEY_NAME)
                    .await
            }
        };
        if let Err(restore_error) = restored {
            return Err(StoreError::Invalid(format!(
                "failed to write {WEBUI_SECRET_GROUP}.{WEBUI_PUBLIC_KEY_NAME} ({error}) and \
                 failed to restore {WEBUI_SECRET_GROUP}.{WEBUI_PRIVATE_KEY_NAME} \
                 ({restore_error}); the stored pair is inconsistent until the next server start \
                 reconciles it"
            )));
        }
        return Err(error);
    }
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
        .chain(EXTRA_CREDENTIALS.iter())
        .find(|(known_group, _)| *known_group == group)
    else {
        let groups = KNOWN_CREDENTIALS
            .iter()
            .chain(EXTRA_CREDENTIALS.iter())
            .map(|(group, _)| *group)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CtlError::message(format!(
            "unknown credential group '{group}'; known groups: {groups}"
        )));
    };

    if group == "object_store" {
        return validate_object_store_credential(name);
    }

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

/// The `object_store` group holds the local `/bookshelf` HMAC secret and the
/// per-organization key pairs `object_store.<org>.access_key` /
/// `.secret_key`.
fn validate_object_store_credential(name: &str) -> Result<(), CtlError> {
    if name == microwave_store::OBJECT_STORE_SECRET_NAME {
        return Ok(());
    }
    let Some((organization, field)) = name.rsplit_once('.') else {
        return Err(CtlError::message(format!(
            "unknown credential '{name}' in group 'object_store'; known names: {}, \
             <organization>.access_key, <organization>.secret_key",
            microwave_store::OBJECT_STORE_SECRET_NAME
        )));
    };
    if !matches!(field, "access_key" | "secret_key") {
        return Err(CtlError::message(format!(
            "unknown credential '{name}' in group 'object_store'; expected \
             <organization>.access_key or <organization>.secret_key"
        )));
    }
    if !microwave_config::valid_org_name(organization) {
        return Err(CtlError::message(format!(
            "invalid organization name '{organization}' for the object_store credential group"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{set_secret, set_webui_pair, show_secret, validate_credential};
    use crate::context::Context;
    use microwave_store::{SecretsRepository, SqliteStore, StoreError};
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Delegates to a real store but fails the second `set_secret` call, which
    /// is the public half of a WebUI pair write.
    struct FailSecondWrite<'a> {
        inner: &'a SqliteStore,
        writes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SecretsRepository for FailSecondWrite<'_> {
        async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
            SecretsRepository::get_secret(self.inner, group, name).await
        }

        async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
            if self.writes.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(StoreError::Invalid(
                    "injected public-key write failure".into(),
                ));
            }
            SecretsRepository::set_secret(self.inner, group, name, value).await
        }

        async fn set_secret_if_absent(
            &self,
            group: &str,
            name: &str,
            value: &str,
        ) -> Result<String, StoreError> {
            SecretsRepository::set_secret_if_absent(self.inner, group, name, value).await
        }

        async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
            SecretsRepository::remove_secret(self.inner, group, name).await
        }
    }

    fn fail_second_write(store: &SqliteStore) -> FailSecondWrite<'_> {
        FailSecondWrite {
            inner: store,
            writes: AtomicUsize::new(0),
        }
    }

    /// Fails the public write and the private restore that follows it.
    struct FailPublicAndRestore<'a> {
        inner: &'a SqliteStore,
        writes: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl SecretsRepository for FailPublicAndRestore<'_> {
        async fn get_secret(&self, group: &str, name: &str) -> Result<Option<String>, StoreError> {
            SecretsRepository::get_secret(self.inner, group, name).await
        }

        async fn set_secret(&self, group: &str, name: &str, value: &str) -> Result<(), StoreError> {
            match self.writes.fetch_add(1, Ordering::SeqCst) {
                1 => Err(StoreError::Invalid("injected public failure".into())),
                2 => Err(StoreError::Invalid("injected restore failure".into())),
                _ => SecretsRepository::set_secret(self.inner, group, name, value).await,
            }
        }

        async fn set_secret_if_absent(
            &self,
            group: &str,
            name: &str,
            value: &str,
        ) -> Result<String, StoreError> {
            SecretsRepository::set_secret_if_absent(self.inner, group, name, value).await
        }

        async fn remove_secret(&self, group: &str, name: &str) -> Result<(), StoreError> {
            SecretsRepository::remove_secret(self.inner, group, name).await
        }
    }

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

    #[test]
    fn object_store_group_accepts_per_org_credential_names() {
        assert!(validate_credential("object_store", "secret").is_ok());
        assert!(validate_credential("object_store", "acme.access_key").is_ok());
        assert!(validate_credential("object_store", "acme.secret_key").is_ok());
        assert!(validate_credential("object_store", "team-1.access_key").is_ok());

        assert!(validate_credential("object_store", "acme.foo").is_err());
        assert!(validate_credential("object_store", "ACME.access_key").is_err());
        assert!(validate_credential("object_store", "acme").is_err());
        assert!(validate_credential("object_store", "acme.").is_err());
        assert!(validate_credential("object_store", ".access_key").is_err());
        assert!(validate_credential("object_store", "a.b.access_key").is_err());
    }

    #[test]
    fn a_per_org_object_store_secret_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_store(dir.path());

        set_secret(&context, "object_store", "acme.access_key", "AKIAEXAMPLE").unwrap();
        show_secret(&context, "object_store", "acme.access_key").unwrap();

        let store = context.open_metadata_store().unwrap();
        let value = store
            .run(store.store().get_secret("object_store", "acme.access_key"))
            .unwrap();
        assert_eq!(value.as_deref(), Some("AKIAEXAMPLE"));
    }

    #[test]
    fn webui_is_extra_and_not_in_the_bulk_credential_list() {
        assert!(super::KNOWN_CREDENTIALS
            .iter()
            .all(|(group, _)| *group != microwave_store::WEBUI_SECRET_GROUP));
        assert!(super::EXTRA_CREDENTIALS
            .iter()
            .any(|(group, _)| *group == microwave_store::WEBUI_SECRET_GROUP));
    }

    #[test]
    fn setting_the_webui_private_key_derives_and_stores_the_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_store(dir.path());
        let (private_pem, _) = microwave_crypto::generate_rsa_keypair().unwrap();

        set_secret(&context, "webui", "private_key", &private_pem).unwrap();

        let store = context.open_metadata_store().unwrap();
        let stored_private = store
            .run(store.store().get_secret("webui", "private_key"))
            .unwrap()
            .unwrap();
        let stored_public = store
            .run(store.store().get_secret("webui", "public_key"))
            .unwrap()
            .unwrap();
        assert_eq!(stored_private, private_pem);
        assert_eq!(
            stored_public,
            microwave_crypto::public_key_pem_from_private(&private_pem).unwrap()
        );
        show_secret(&context, "webui", "private_key").unwrap();
    }

    #[test]
    fn setting_the_webui_public_key_directly_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_store(dir.path());

        let error = set_secret(&context, "webui", "public_key", "anything").unwrap_err();

        assert!(error.to_string().contains("derived"), "{error}");
        let store = context.open_metadata_store().unwrap();
        assert!(store
            .run(store.store().get_secret("webui", "public_key"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_malformed_webui_private_key_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let context = context_with_store(dir.path());

        assert!(set_secret(&context, "webui", "private_key", "not a key").is_err());

        let store = context.open_metadata_store().unwrap();
        assert!(store
            .run(store.store().get_secret("webui", "private_key"))
            .unwrap()
            .is_none());
        assert!(store
            .run(store.store().get_secret("webui", "public_key"))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_failed_public_write_restores_the_previous_pair() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret("webui", "private_key", "old-private")
            .unwrap();
        store
            .set_secret("webui", "public_key", "old-public")
            .unwrap();
        let failing = fail_second_write(&store);

        let error = set_webui_pair(&failing, "new-private", "new-public")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("injected"), "{error}");
        assert_eq!(
            store.get_secret("webui", "private_key").unwrap().as_deref(),
            Some("old-private")
        );
        assert_eq!(
            store.get_secret("webui", "public_key").unwrap().as_deref(),
            Some("old-public")
        );
    }

    #[tokio::test]
    async fn a_failed_public_write_removes_a_fresh_private_half() {
        let store = SqliteStore::open_in_memory().unwrap();
        let failing = fail_second_write(&store);

        let error = set_webui_pair(&failing, "new-private", "new-public")
            .await
            .unwrap_err();

        assert!(error.to_string().contains("injected"), "{error}");
        assert!(store.get_secret("webui", "private_key").unwrap().is_none());
        assert!(store.get_secret("webui", "public_key").unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failed_restore_names_both_failures() {
        let store = SqliteStore::open_in_memory().unwrap();
        store
            .set_secret("webui", "private_key", "old-private")
            .unwrap();
        store
            .set_secret("webui", "public_key", "old-public")
            .unwrap();
        let failing = FailPublicAndRestore {
            inner: &store,
            writes: AtomicUsize::new(0),
        };

        let error = set_webui_pair(&failing, "new-private", "new-public")
            .await
            .unwrap_err();
        let text = error.to_string();

        assert!(text.contains("injected public failure"), "{text}");
        assert!(text.contains("injected restore failure"), "{text}");
        assert!(text.contains("inconsistent"), "{text}");
        // The failed restore leaves the new private half behind; startup
        // re-derives the public half from it.
        assert_eq!(
            store.get_secret("webui", "private_key").unwrap().as_deref(),
            Some("new-private")
        );
        assert_eq!(
            store.get_secret("webui", "public_key").unwrap().as_deref(),
            Some("old-public")
        );
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
