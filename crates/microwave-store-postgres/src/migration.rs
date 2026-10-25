//! Full-fidelity `StoreSnapshot` export and import for PostgreSQL.
//!
//! The snapshot format is owned by `microwave-store` and shared with the
//! SQLite backend. Export reads every migration-relevant table inside a
//! repeatable-read, read-only transaction so the rows come from one logical
//! point in time. Import mirrors the SQLite upsert semantics: rows keep their
//! ids, authz ids, timestamps, password hashes, and keys, and importing the
//! same snapshot twice is safe.
//!
//! PostgreSQL's sequence-backed identifiers (cookbooks, cookbook artifacts,
//! artifact versions, and OAuth applications) are inserted explicitly and the
//! sequences are advanced afterwards so later inserts cannot collide.
//!
//! The snapshot predates PostgreSQL's explicit `sandboxes` table. Import
//! derives sandbox rows from `sandboxed_checksums`, matching the backfill in
//! the embedded schema migration.

use microwave_store::{
    decode_serialized_optional, encode_serialized_optional, ActorKeyRow, AuthzAclActorRow,
    AuthzAclGroupRow, AuthzEntityRow, AuthzGroupActorRow, AuthzGroupGroupRow, ChecksumRow,
    ClientRow, Compression, ContainerRow, CookbookArtifactRow, CookbookArtifactVersionChecksumRow,
    CookbookArtifactVersionRow, CookbookRow, CookbookVersionChecksumRow, CookbookVersionRow,
    DataBagItemRow, GlobalGroupRow, GroupRow, NamedObjectRow, NodeRow, OAuthApplicationRow, OrgRow,
    OrgUserAssociationRow, OrgUserInviteRow, PolicyGroupRevisionAssociationRow, PolicyGroupRow,
    PolicyRevisionRow, PolicyRow, SandboxedChecksumRow, StoreError, StoreSnapshot, UserRow,
};
use sqlx::postgres::{PgRow, Postgres};
use sqlx::{Row, Transaction};

use crate::store::{db, PostgresStore};

type PgTransaction<'a> = Transaction<'a, Postgres>;

impl PostgresStore {
    /// Read every migration-relevant table into a [`StoreSnapshot`].
    pub async fn export_snapshot(&self) -> Result<StoreSnapshot, StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;
        sqlx::raw_sql("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
            .execute(&mut *transaction)
            .await
            .map_err(db)?;

        let snapshot = StoreSnapshot {
            orgs: collect_orgs(&mut transaction).await?,
            users: collect_users(&mut transaction).await?,
            org_user_associations: collect_org_user_associations(&mut transaction).await?,
            org_user_invites: collect_org_user_invites(&mut transaction).await?,
            clients: collect_clients(&mut transaction).await?,
            actor_keys: collect_actor_keys(&mut transaction).await?,
            authz_entities: collect_authz_entities(&mut transaction).await?,
            authz_acl_actor: collect_authz_acl_actor(&mut transaction).await?,
            authz_acl_group: collect_authz_acl_group(&mut transaction).await?,
            authz_group_actors: collect_authz_group_actors(&mut transaction).await?,
            authz_group_groups: collect_authz_group_groups(&mut transaction).await?,
            containers: collect_containers(&mut transaction).await?,
            groups: collect_groups(&mut transaction).await?,
            global_groups: collect_global_groups(&mut transaction).await?,
            nodes: collect_nodes(&mut transaction).await?,
            roles: collect_named_objects(&mut transaction, "roles").await?,
            environments: collect_named_objects(&mut transaction, "environments").await?,
            data_bags: collect_named_objects(&mut transaction, "data_bags").await?,
            data_bag_items: collect_data_bag_items(&mut transaction).await?,
            checksums: collect_checksums(&mut transaction).await?,
            sandboxed_checksums: collect_sandboxed_checksums(&mut transaction).await?,
            cookbooks: collect_cookbooks(&mut transaction).await?,
            cookbook_versions: collect_cookbook_versions(&mut transaction).await?,
            cookbook_version_checksums: collect_cookbook_version_checksums(&mut transaction)
                .await?,
            cookbook_artifacts: collect_cookbook_artifacts(&mut transaction).await?,
            cookbook_artifact_versions: collect_cookbook_artifact_versions(&mut transaction)
                .await?,
            cookbook_artifact_version_checksums: collect_cookbook_artifact_version_checksums(
                &mut transaction,
            )
            .await?,
            policies: collect_policies(&mut transaction).await?,
            policy_groups: collect_policy_groups(&mut transaction).await?,
            policy_revisions: collect_policy_revisions(&mut transaction).await?,
            policy_group_revision_associations: collect_policy_group_revision_associations(
                &mut transaction,
            )
            .await?,
            oauth_applications: collect_oauth_applications(&mut transaction).await?,
        };

        transaction.commit().await.map_err(db)?;
        Ok(snapshot)
    }

    /// Import a snapshot into this store in one transaction.
    pub async fn import_snapshot(&self, snapshot: &StoreSnapshot) -> Result<(), StoreError> {
        let mut transaction = self.pool().begin().await.map_err(db)?;

        import_orgs(&mut transaction, snapshot).await?;
        import_users(&mut transaction, snapshot).await?;
        import_org_user_associations(&mut transaction, snapshot).await?;
        import_org_user_invites(&mut transaction, snapshot).await?;
        import_clients(&mut transaction, snapshot).await?;
        import_actor_keys(&mut transaction, snapshot).await?;
        import_authz_entities(&mut transaction, snapshot).await?;
        import_authz_acl(&mut transaction, snapshot).await?;
        import_authz_groups(&mut transaction, snapshot).await?;
        import_containers(&mut transaction, snapshot).await?;
        import_groups(&mut transaction, snapshot).await?;
        import_global_groups(&mut transaction, snapshot).await?;
        import_nodes(&mut transaction, snapshot, self.compression()).await?;
        import_named_objects(
            &mut transaction,
            "roles",
            &snapshot.roles,
            self.compression(),
        )
        .await?;
        import_named_objects(
            &mut transaction,
            "environments",
            &snapshot.environments,
            self.compression(),
        )
        .await?;
        import_named_objects(
            &mut transaction,
            "data_bags",
            &snapshot.data_bags,
            self.compression(),
        )
        .await?;
        import_data_bag_items(&mut transaction, snapshot, self.compression()).await?;
        import_checksums(&mut transaction, snapshot).await?;
        import_sandboxed_checksums(&mut transaction, snapshot).await?;
        import_cookbooks(&mut transaction, snapshot).await?;
        import_cookbook_versions(&mut transaction, snapshot, self.compression()).await?;
        import_cookbook_version_checksums(&mut transaction, snapshot).await?;
        import_cookbook_artifacts(&mut transaction, snapshot).await?;
        import_cookbook_artifact_versions(&mut transaction, snapshot, self.compression()).await?;
        import_cookbook_artifact_version_checksums(&mut transaction, snapshot).await?;
        import_policies(&mut transaction, snapshot).await?;
        import_policy_groups(&mut transaction, snapshot, self.compression()).await?;
        import_policy_revisions(&mut transaction, snapshot, self.compression()).await?;
        import_policy_group_revision_associations(&mut transaction, snapshot).await?;
        import_oauth_applications(&mut transaction, snapshot).await?;
        advance_sequences(&mut transaction).await?;

        transaction.commit().await.map_err(db)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

async fn rows(transaction: &mut PgTransaction<'_>, sql: &str) -> Result<Vec<PgRow>, StoreError> {
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .fetch_all(&mut **transaction)
        .await
        .map_err(db)
}

fn text(row: &PgRow, index: usize) -> Result<String, StoreError> {
    row.try_get(index).map_err(db)
}

fn opt_text(row: &PgRow, index: usize) -> Result<Option<String>, StoreError> {
    row.try_get(index).map_err(db)
}

/// Read a `serialized_object` column as plain snapshot JSON.
fn serialized(row: &PgRow, index: usize) -> Result<Option<String>, StoreError> {
    let value = opt_text(row, index)?;
    decode_serialized_optional(value.as_deref())
}

fn integer(row: &PgRow, index: usize) -> Result<i64, StoreError> {
    row.try_get(index).map_err(db)
}

/// Decode a PostgreSQL `INTEGER` (int4) column into the snapshot's i64 field.
fn integer_i32(row: &PgRow, index: usize) -> Result<i64, StoreError> {
    let value: i32 = row.try_get(index).map_err(db)?;
    Ok(i64::from(value))
}

fn boolean(row: &PgRow, index: usize) -> Result<bool, StoreError> {
    row.try_get(index).map_err(db)
}

async fn collect_orgs(transaction: &mut PgTransaction<'_>) -> Result<Vec<OrgRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, authz_id, name, full_name, assigned_at, last_updated_by,
                created_at, updated_at
         FROM orgs ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(OrgRow {
            id: text(row, 0)?,
            authz_id: text(row, 1)?,
            name: text(row, 2)?,
            full_name: text(row, 3)?,
            assigned_at: text(row, 4)?,
            last_updated_by: text(row, 5)?,
            created_at: text(row, 6)?,
            updated_at: text(row, 7)?,
        })
    })
    .collect()
}

async fn collect_users(transaction: &mut PgTransaction<'_>) -> Result<Vec<UserRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, authz_id, username, email, password_hash, password_salt,
                hash_type, admin, recovery_authentication_enabled,
                external_authentication_uid, display_name, first_name, middle_name,
                last_name, last_updated_by, created_at, updated_at
         FROM users ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(UserRow {
            id: text(row, 0)?,
            authz_id: text(row, 1)?,
            username: text(row, 2)?,
            email: opt_text(row, 3)?,
            password_hash: text(row, 4)?,
            password_salt: text(row, 5)?,
            hash_type: text(row, 6)?,
            admin: boolean(row, 7)?,
            recovery_authentication_enabled: boolean(row, 8)?,
            external_authentication_uid: opt_text(row, 9)?,
            display_name: opt_text(row, 10)?,
            first_name: opt_text(row, 11)?,
            middle_name: opt_text(row, 12)?,
            last_name: opt_text(row, 13)?,
            last_updated_by: text(row, 14)?,
            created_at: text(row, 15)?,
            updated_at: text(row, 16)?,
        })
    })
    .collect()
}

async fn collect_org_user_associations(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<OrgUserAssociationRow>, StoreError> {
    rows(
        transaction,
        "SELECT org_id, user_id, last_updated_by, created_at, updated_at
         FROM org_user_associations ORDER BY org_id, user_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(OrgUserAssociationRow {
            org_id: text(row, 0)?,
            user_id: text(row, 1)?,
            last_updated_by: text(row, 2)?,
            created_at: text(row, 3)?,
            updated_at: text(row, 4)?,
        })
    })
    .collect()
}

async fn collect_org_user_invites(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<OrgUserInviteRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, user_id, last_updated_by, created_at, updated_at
         FROM org_user_invites ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(OrgUserInviteRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            user_id: text(row, 2)?,
            last_updated_by: text(row, 3)?,
            created_at: text(row, 4)?,
            updated_at: text(row, 5)?,
        })
    })
    .collect()
}

async fn collect_clients(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<ClientRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, authz_id, name, validator, admin, created_at, updated_at
         FROM clients ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(ClientRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            authz_id: text(row, 2)?,
            name: text(row, 3)?,
            validator: boolean(row, 4)?,
            admin: boolean(row, 5)?,
            created_at: text(row, 6)?,
            updated_at: text(row, 7)?,
        })
    })
    .collect()
}

async fn collect_actor_keys(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<ActorKeyRow>, StoreError> {
    rows(
        transaction,
        "SELECT actor_id, actor_type, key_name, public_key, key_version,
                expires_at, last_updated_by, created_at, updated_at
         FROM actor_keys ORDER BY actor_id, actor_type, key_name",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(ActorKeyRow {
            actor_id: text(row, 0)?,
            actor_type: text(row, 1)?,
            key_name: text(row, 2)?,
            public_key: text(row, 3)?,
            key_version: integer_i32(row, 4)?,
            expires_at: text(row, 5)?,
            last_updated_by: text(row, 6)?,
            created_at: text(row, 7)?,
            updated_at: text(row, 8)?,
        })
    })
    .collect()
}

async fn collect_authz_entities(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<AuthzEntityRow>, StoreError> {
    rows(
        transaction,
        "SELECT authz_id, entity_type, org_id, created_at
         FROM authz_entities ORDER BY authz_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(AuthzEntityRow {
            authz_id: text(row, 0)?,
            entity_type: text(row, 1)?,
            org_id: opt_text(row, 2)?,
            created_at: text(row, 3)?,
        })
    })
    .collect()
}

async fn collect_authz_acl_actor(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<AuthzAclActorRow>, StoreError> {
    rows(
        transaction,
        "SELECT target_authz_id, permission, actor_authz_id
         FROM authz_acl_actor ORDER BY target_authz_id, permission, actor_authz_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(AuthzAclActorRow {
            target_authz_id: text(row, 0)?,
            permission: text(row, 1)?,
            actor_authz_id: text(row, 2)?,
        })
    })
    .collect()
}

async fn collect_authz_acl_group(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<AuthzAclGroupRow>, StoreError> {
    rows(
        transaction,
        "SELECT target_authz_id, permission, group_authz_id
         FROM authz_acl_group ORDER BY target_authz_id, permission, group_authz_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(AuthzAclGroupRow {
            target_authz_id: text(row, 0)?,
            permission: text(row, 1)?,
            group_authz_id: text(row, 2)?,
        })
    })
    .collect()
}

async fn collect_authz_group_actors(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<AuthzGroupActorRow>, StoreError> {
    rows(
        transaction,
        "SELECT group_authz_id, actor_authz_id
         FROM authz_group_actors ORDER BY group_authz_id, actor_authz_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(AuthzGroupActorRow {
            group_authz_id: text(row, 0)?,
            actor_authz_id: text(row, 1)?,
        })
    })
    .collect()
}

async fn collect_authz_group_groups(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<AuthzGroupGroupRow>, StoreError> {
    rows(
        transaction,
        "SELECT parent_group_authz_id, child_group_authz_id
         FROM authz_group_groups ORDER BY parent_group_authz_id, child_group_authz_id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(AuthzGroupGroupRow {
            parent_group_authz_id: text(row, 0)?,
            child_group_authz_id: text(row, 1)?,
        })
    })
    .collect()
}

async fn collect_containers(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<ContainerRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, authz_id, name, created_at, updated_at
         FROM containers ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(ContainerRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            authz_id: text(row, 2)?,
            name: text(row, 3)?,
            created_at: text(row, 4)?,
            updated_at: text(row, 5)?,
        })
    })
    .collect()
}

async fn collect_groups(transaction: &mut PgTransaction<'_>) -> Result<Vec<GroupRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, authz_id, name, created_at, updated_at
         FROM groups ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(GroupRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            authz_id: text(row, 2)?,
            name: text(row, 3)?,
            created_at: text(row, 4)?,
            updated_at: text(row, 5)?,
        })
    })
    .collect()
}

async fn collect_global_groups(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<GlobalGroupRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, authz_id, name, created_at, updated_at
         FROM global_groups ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(GlobalGroupRow {
            id: text(row, 0)?,
            authz_id: text(row, 1)?,
            name: text(row, 2)?,
            created_at: text(row, 3)?,
            updated_at: text(row, 4)?,
        })
    })
    .collect()
}

async fn collect_nodes(transaction: &mut PgTransaction<'_>) -> Result<Vec<NodeRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, authz_id, org_id, name, environment, policy_name, policy_group,
                serialized_object, last_updated_by, created_at, updated_at
         FROM nodes ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(NodeRow {
            id: text(row, 0)?,
            authz_id: text(row, 1)?,
            org_id: text(row, 2)?,
            name: text(row, 3)?,
            environment: text(row, 4)?,
            policy_name: opt_text(row, 5)?,
            policy_group: opt_text(row, 6)?,
            serialized_object: serialized(row, 7)?,
            last_updated_by: text(row, 8)?,
            created_at: text(row, 9)?,
            updated_at: text(row, 10)?,
        })
    })
    .collect()
}

async fn collect_named_objects(
    transaction: &mut PgTransaction<'_>,
    table: &str,
) -> Result<Vec<NamedObjectRow>, StoreError> {
    let sql = format!(
        "SELECT id, authz_id, org_id, name, serialized_object,
                last_updated_by, created_at, updated_at
         FROM {table} ORDER BY id"
    );
    rows(transaction, &sql)
        .await?
        .iter()
        .map(|row| {
            Ok(NamedObjectRow {
                id: text(row, 0)?,
                authz_id: text(row, 1)?,
                org_id: text(row, 2)?,
                name: text(row, 3)?,
                serialized_object: serialized(row, 4)?,
                last_updated_by: text(row, 5)?,
                created_at: text(row, 6)?,
                updated_at: text(row, 7)?,
            })
        })
        .collect()
}

async fn collect_data_bag_items(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<DataBagItemRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, data_bag_name, item_name, serialized_object,
                last_updated_by, created_at, updated_at
         FROM data_bag_items ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(DataBagItemRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            data_bag_name: text(row, 2)?,
            item_name: text(row, 3)?,
            serialized_object: serialized(row, 4)?,
            last_updated_by: text(row, 5)?,
            created_at: text(row, 6)?,
            updated_at: text(row, 7)?,
        })
    })
    .collect()
}

async fn collect_checksums(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<ChecksumRow>, StoreError> {
    rows(
        transaction,
        "SELECT org_id, checksum, created_at FROM checksums ORDER BY org_id, checksum",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(ChecksumRow {
            org_id: text(row, 0)?,
            checksum: text(row, 1)?,
            created_at: text(row, 2)?,
        })
    })
    .collect()
}

async fn collect_sandboxed_checksums(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<SandboxedChecksumRow>, StoreError> {
    rows(
        transaction,
        "SELECT org_id, sandbox_id, checksum, created_at
         FROM sandboxed_checksums ORDER BY org_id, sandbox_id, checksum",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(SandboxedChecksumRow {
            org_id: text(row, 0)?,
            sandbox_id: text(row, 1)?,
            checksum: text(row, 2)?,
            created_at: text(row, 3)?,
        })
    })
    .collect()
}

async fn collect_cookbooks(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, name, authz_id FROM cookbooks ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookRow {
            id: integer(row, 0)?,
            org_id: text(row, 1)?,
            name: text(row, 2)?,
            authz_id: text(row, 3)?,
        })
    })
    .collect()
}

async fn collect_cookbook_versions(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookVersionRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, cookbook_id, org_id, name, major, minor, patch, frozen,
                meta_attributes, meta_deps, meta_long_desc, metadata,
                serialized_object, last_updated_by, created_at, updated_at
         FROM cookbook_versions ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookVersionRow {
            id: text(row, 0)?,
            cookbook_id: integer(row, 1)?,
            org_id: text(row, 2)?,
            name: text(row, 3)?,
            major: integer(row, 4)?,
            minor: integer(row, 5)?,
            patch: integer(row, 6)?,
            frozen: boolean(row, 7)?,
            meta_attributes: opt_text(row, 8)?,
            meta_deps: opt_text(row, 9)?,
            meta_long_desc: opt_text(row, 10)?,
            metadata: opt_text(row, 11)?,
            serialized_object: serialized(row, 12)?,
            last_updated_by: text(row, 13)?,
            created_at: text(row, 14)?,
            updated_at: text(row, 15)?,
        })
    })
    .collect()
}

async fn collect_cookbook_version_checksums(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookVersionChecksumRow>, StoreError> {
    rows(
        transaction,
        "SELECT cookbook_version_id, org_id, checksum
         FROM cookbook_version_checksums
         ORDER BY cookbook_version_id, checksum",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookVersionChecksumRow {
            cookbook_version_id: text(row, 0)?,
            org_id: text(row, 1)?,
            checksum: text(row, 2)?,
        })
    })
    .collect()
}

async fn collect_cookbook_artifacts(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookArtifactRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, name, authz_id FROM cookbook_artifacts ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookArtifactRow {
            id: integer(row, 0)?,
            org_id: text(row, 1)?,
            name: text(row, 2)?,
            authz_id: text(row, 3)?,
        })
    })
    .collect()
}

async fn collect_cookbook_artifact_versions(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookArtifactVersionRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, cookbook_artifact_id, org_id, identifier, metadata,
                serialized_object, created_at, created_by
         FROM cookbook_artifact_versions ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookArtifactVersionRow {
            id: integer(row, 0)?,
            cookbook_artifact_id: integer(row, 1)?,
            org_id: text(row, 2)?,
            identifier: text(row, 3)?,
            metadata: opt_text(row, 4)?,
            serialized_object: serialized(row, 5)?,
            created_at: text(row, 6)?,
            created_by: text(row, 7)?,
        })
    })
    .collect()
}

async fn collect_cookbook_artifact_version_checksums(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<CookbookArtifactVersionChecksumRow>, StoreError> {
    rows(
        transaction,
        "SELECT cookbook_artifact_version_id, org_id, checksum
         FROM cookbook_artifact_version_checksums
         ORDER BY cookbook_artifact_version_id, checksum",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(CookbookArtifactVersionChecksumRow {
            cookbook_artifact_version_id: integer(row, 0)?,
            org_id: text(row, 1)?,
            checksum: text(row, 2)?,
        })
    })
    .collect()
}

async fn collect_policies(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<PolicyRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, name, authz_id, last_updated_by, created_at, updated_at
         FROM policies ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(PolicyRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            name: text(row, 2)?,
            authz_id: text(row, 3)?,
            last_updated_by: text(row, 4)?,
            created_at: text(row, 5)?,
            updated_at: text(row, 6)?,
        })
    })
    .collect()
}

async fn collect_policy_groups(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<PolicyGroupRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, name, authz_id, serialized_object,
                last_updated_by, created_at, updated_at
         FROM policy_groups ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(PolicyGroupRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            name: text(row, 2)?,
            authz_id: text(row, 3)?,
            serialized_object: serialized(row, 4)?,
            last_updated_by: text(row, 5)?,
            created_at: text(row, 6)?,
            updated_at: text(row, 7)?,
        })
    })
    .collect()
}

async fn collect_policy_revisions(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<PolicyRevisionRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, policy_id, revision_id, name, serialized_object,
                last_updated_by, created_at, updated_at
         FROM policy_revisions ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(PolicyRevisionRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            policy_id: text(row, 2)?,
            revision_id: text(row, 3)?,
            name: text(row, 4)?,
            serialized_object: serialized(row, 5)?,
            last_updated_by: text(row, 6)?,
            created_at: text(row, 7)?,
            updated_at: text(row, 8)?,
        })
    })
    .collect()
}

async fn collect_policy_group_revision_associations(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<PolicyGroupRevisionAssociationRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, org_id, policy_revision_id, policy_group_authz_id,
                policy_revision_name, policy_revision_revision_id,
                policy_group_name, last_updated_by, created_at, updated_at
         FROM policy_group_revision_associations ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(PolicyGroupRevisionAssociationRow {
            id: text(row, 0)?,
            org_id: text(row, 1)?,
            policy_revision_id: text(row, 2)?,
            policy_group_authz_id: text(row, 3)?,
            policy_revision_name: text(row, 4)?,
            policy_revision_revision_id: text(row, 5)?,
            policy_group_name: text(row, 6)?,
            last_updated_by: text(row, 7)?,
            created_at: text(row, 8)?,
            updated_at: text(row, 9)?,
        })
    })
    .collect()
}

async fn collect_oauth_applications(
    transaction: &mut PgTransaction<'_>,
) -> Result<Vec<OAuthApplicationRow>, StoreError> {
    rows(
        transaction,
        "SELECT id, name, uid, secret, redirect_uri, scopes, confidential,
                created_at, updated_at
         FROM oauth_applications ORDER BY id",
    )
    .await?
    .iter()
    .map(|row| {
        Ok(OAuthApplicationRow {
            id: integer(row, 0)?,
            name: text(row, 1)?,
            uid: text(row, 2)?,
            secret: text(row, 3)?,
            redirect_uri: text(row, 4)?,
            scopes: text(row, 5)?,
            confidential: boolean(row, 6)?,
            created_at: text(row, 7)?,
            updated_at: text(row, 8)?,
        })
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

async fn import_orgs(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.orgs {
        sqlx::query(
            "INSERT INTO orgs
             (id, authz_id, name, full_name, assigned_at, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 authz_id = EXCLUDED.authz_id,
                 name = EXCLUDED.name,
                 full_name = EXCLUDED.full_name,
                 assigned_at = EXCLUDED.assigned_at,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.authz_id)
        .bind(&row.name)
        .bind(&row.full_name)
        .bind(&row.assigned_at)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_users(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.users {
        sqlx::query(
            "INSERT INTO users
             (id, authz_id, username, email, password_hash, password_salt, hash_type,
              admin, recovery_authentication_enabled, external_authentication_uid,
              display_name, first_name, middle_name, last_name,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
             ON CONFLICT (id) DO UPDATE SET
                 authz_id = EXCLUDED.authz_id,
                 username = EXCLUDED.username,
                 email = EXCLUDED.email,
                 password_hash = EXCLUDED.password_hash,
                 password_salt = EXCLUDED.password_salt,
                 hash_type = EXCLUDED.hash_type,
                 admin = EXCLUDED.admin,
                 recovery_authentication_enabled = EXCLUDED.recovery_authentication_enabled,
                 external_authentication_uid = EXCLUDED.external_authentication_uid,
                 display_name = EXCLUDED.display_name,
                 first_name = EXCLUDED.first_name,
                 middle_name = EXCLUDED.middle_name,
                 last_name = EXCLUDED.last_name,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.authz_id)
        .bind(&row.username)
        .bind(row.email.as_deref())
        .bind(&row.password_hash)
        .bind(&row.password_salt)
        .bind(&row.hash_type)
        .bind(row.admin)
        .bind(row.recovery_authentication_enabled)
        .bind(row.external_authentication_uid.as_deref())
        .bind(row.display_name.as_deref())
        .bind(row.first_name.as_deref())
        .bind(row.middle_name.as_deref())
        .bind(row.last_name.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_org_user_associations(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.org_user_associations {
        sqlx::query(
            "INSERT INTO org_user_associations
             (org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (org_id, user_id) DO UPDATE SET
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.org_id)
        .bind(&row.user_id)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_org_user_invites(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.org_user_invites {
        sqlx::query(
            "INSERT INTO org_user_invites
             (id, org_id, user_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 user_id = EXCLUDED.user_id,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.user_id)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_clients(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.clients {
        sqlx::query(
            "INSERT INTO clients
             (id, org_id, authz_id, name, validator, admin, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 authz_id = EXCLUDED.authz_id,
                 name = EXCLUDED.name,
                 validator = EXCLUDED.validator,
                 admin = EXCLUDED.admin,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.authz_id)
        .bind(&row.name)
        .bind(row.validator)
        .bind(row.admin)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_actor_keys(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.actor_keys {
        sqlx::query(
            "INSERT INTO actor_keys
             (actor_id, actor_type, key_name, public_key, key_version, expires_at,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (actor_id, key_name) DO UPDATE SET
                 actor_type = EXCLUDED.actor_type,
                 public_key = EXCLUDED.public_key,
                 key_version = EXCLUDED.key_version,
                 expires_at = EXCLUDED.expires_at,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.actor_id)
        .bind(&row.actor_type)
        .bind(&row.key_name)
        .bind(&row.public_key)
        .bind(row.key_version)
        .bind(&row.expires_at)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_authz_entities(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.authz_entities {
        sqlx::query(
            "INSERT INTO authz_entities (authz_id, entity_type, org_id, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (authz_id) DO UPDATE SET
                 entity_type = EXCLUDED.entity_type,
                 org_id = EXCLUDED.org_id,
                 created_at = EXCLUDED.created_at",
        )
        .bind(&row.authz_id)
        .bind(&row.entity_type)
        .bind(row.org_id.as_deref())
        .bind(&row.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_authz_acl(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.authz_acl_actor {
        sqlx::query(
            "INSERT INTO authz_acl_actor (target_authz_id, permission, actor_authz_id)
             VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(&row.target_authz_id)
        .bind(&row.permission)
        .bind(&row.actor_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    for row in &snapshot.authz_acl_group {
        sqlx::query(
            "INSERT INTO authz_acl_group (target_authz_id, permission, group_authz_id)
             VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(&row.target_authz_id)
        .bind(&row.permission)
        .bind(&row.group_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_authz_groups(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.authz_group_actors {
        sqlx::query(
            "INSERT INTO authz_group_actors (group_authz_id, actor_authz_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(&row.group_authz_id)
        .bind(&row.actor_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    for row in &snapshot.authz_group_groups {
        sqlx::query(
            "INSERT INTO authz_group_groups (parent_group_authz_id, child_group_authz_id)
             VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(&row.parent_group_authz_id)
        .bind(&row.child_group_authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_containers(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.containers {
        sqlx::query(
            "INSERT INTO containers
             (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 authz_id = EXCLUDED.authz_id,
                 name = EXCLUDED.name,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.authz_id)
        .bind(&row.name)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_groups(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.groups {
        sqlx::query(
            "INSERT INTO groups
             (id, org_id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 authz_id = EXCLUDED.authz_id,
                 name = EXCLUDED.name,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.authz_id)
        .bind(&row.name)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_global_groups(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.global_groups {
        sqlx::query(
            "INSERT INTO global_groups
             (id, authz_id, name, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET
                 authz_id = EXCLUDED.authz_id,
                 name = EXCLUDED.name,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.authz_id)
        .bind(&row.name)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_nodes(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.nodes {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO nodes
             (id, authz_id, org_id, name, environment, policy_name, policy_group,
              serialized_object, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (id) DO UPDATE SET
                 authz_id = EXCLUDED.authz_id,
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 environment = EXCLUDED.environment,
                 policy_name = EXCLUDED.policy_name,
                 policy_group = EXCLUDED.policy_group,
                 serialized_object = EXCLUDED.serialized_object,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.authz_id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(&row.environment)
        .bind(row.policy_name.as_deref())
        .bind(row.policy_group.as_deref())
        .bind(serialized_object.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_named_objects(
    transaction: &mut PgTransaction<'_>,
    table: &str,
    rows: &[NamedObjectRow],
    compression: Compression,
) -> Result<(), StoreError> {
    let sql = format!(
        "INSERT INTO {table}
         (id, authz_id, org_id, name, serialized_object, last_updated_by, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (id) DO UPDATE SET
             authz_id = EXCLUDED.authz_id,
             org_id = EXCLUDED.org_id,
             name = EXCLUDED.name,
             serialized_object = EXCLUDED.serialized_object,
             last_updated_by = EXCLUDED.last_updated_by,
             created_at = EXCLUDED.created_at,
             updated_at = EXCLUDED.updated_at"
    );
    for row in rows {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .bind(&row.id)
            .bind(&row.authz_id)
            .bind(&row.org_id)
            .bind(&row.name)
            .bind(serialized_object.as_deref())
            .bind(&row.last_updated_by)
            .bind(&row.created_at)
            .bind(&row.updated_at)
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
    }
    Ok(())
}

async fn import_data_bag_items(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.data_bag_items {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO data_bag_items
             (id, org_id, data_bag_name, item_name, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 data_bag_name = EXCLUDED.data_bag_name,
                 item_name = EXCLUDED.item_name,
                 serialized_object = EXCLUDED.serialized_object,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.data_bag_name)
        .bind(&row.item_name)
        .bind(serialized_object.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_checksums(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.checksums {
        sqlx::query(
            "INSERT INTO checksums (org_id, checksum, created_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (org_id, checksum) DO UPDATE SET created_at = EXCLUDED.created_at",
        )
        .bind(&row.org_id)
        .bind(&row.checksum)
        .bind(&row.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_sandboxed_checksums(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.sandboxed_checksums {
        sqlx::query(
            "INSERT INTO sandboxed_checksums (org_id, sandbox_id, checksum, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (sandbox_id, org_id, checksum)
             DO UPDATE SET created_at = EXCLUDED.created_at",
        )
        .bind(&row.org_id)
        .bind(&row.sandbox_id)
        .bind(&row.checksum)
        .bind(&row.created_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    // The snapshot format does not carry the explicit sandboxes table or
    // empty sandboxes. Derive rows for in-flight uploads the same way the
    // embedded schema backfill does; empty sandboxes are not restored.
    sqlx::query(
        "INSERT INTO sandboxes (org_id, sandbox_id, created_at)
         SELECT org_id, sandbox_id, MIN(created_at)
         FROM sandboxed_checksums
         GROUP BY org_id, sandbox_id
         ON CONFLICT DO NOTHING",
    )
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(())
}

async fn import_cookbooks(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbooks {
        sqlx::query(
            "INSERT INTO cookbooks (id, org_id, name, authz_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 authz_id = EXCLUDED.authz_id",
        )
        .bind(row.id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(&row.authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_cookbook_versions(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbook_versions {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO cookbook_versions
             (id, cookbook_id, org_id, name, major, minor, patch, frozen,
              meta_attributes, meta_deps, meta_long_desc, metadata, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
             ON CONFLICT (id) DO UPDATE SET
                 cookbook_id = EXCLUDED.cookbook_id,
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 major = EXCLUDED.major,
                 minor = EXCLUDED.minor,
                 patch = EXCLUDED.patch,
                 frozen = EXCLUDED.frozen,
                 meta_attributes = EXCLUDED.meta_attributes,
                 meta_deps = EXCLUDED.meta_deps,
                 meta_long_desc = EXCLUDED.meta_long_desc,
                 metadata = EXCLUDED.metadata,
                 serialized_object = EXCLUDED.serialized_object,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(row.cookbook_id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(row.major)
        .bind(row.minor)
        .bind(row.patch)
        .bind(row.frozen)
        .bind(row.meta_attributes.as_deref())
        .bind(row.meta_deps.as_deref())
        .bind(row.meta_long_desc.as_deref())
        .bind(row.metadata.as_deref())
        .bind(serialized_object.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
        sqlx::query("DELETE FROM cookbook_version_checksums WHERE cookbook_version_id = $1")
            .bind(&row.id)
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
    }
    Ok(())
}

async fn import_cookbook_version_checksums(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbook_version_checksums {
        sqlx::query(
            "INSERT INTO cookbook_version_checksums
             (cookbook_version_id, org_id, checksum)
             VALUES ($1, $2, $3)",
        )
        .bind(&row.cookbook_version_id)
        .bind(&row.org_id)
        .bind(&row.checksum)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_cookbook_artifacts(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbook_artifacts {
        sqlx::query(
            "INSERT INTO cookbook_artifacts (id, org_id, name, authz_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 authz_id = EXCLUDED.authz_id",
        )
        .bind(row.id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(&row.authz_id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_cookbook_artifact_versions(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbook_artifact_versions {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO cookbook_artifact_versions
             (id, cookbook_artifact_id, org_id, identifier, metadata,
              serialized_object, created_at, created_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 cookbook_artifact_id = EXCLUDED.cookbook_artifact_id,
                 org_id = EXCLUDED.org_id,
                 identifier = EXCLUDED.identifier,
                 metadata = EXCLUDED.metadata,
                 serialized_object = EXCLUDED.serialized_object,
                 created_at = EXCLUDED.created_at,
                 created_by = EXCLUDED.created_by",
        )
        .bind(row.id)
        .bind(row.cookbook_artifact_id)
        .bind(&row.org_id)
        .bind(&row.identifier)
        .bind(row.metadata.as_deref())
        .bind(serialized_object.as_deref())
        .bind(&row.created_at)
        .bind(&row.created_by)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
        sqlx::query(
            "DELETE FROM cookbook_artifact_version_checksums
             WHERE cookbook_artifact_version_id = $1",
        )
        .bind(row.id)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_cookbook_artifact_version_checksums(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.cookbook_artifact_version_checksums {
        sqlx::query(
            "INSERT INTO cookbook_artifact_version_checksums
             (cookbook_artifact_version_id, org_id, checksum)
             VALUES ($1, $2, $3)",
        )
        .bind(row.cookbook_artifact_version_id)
        .bind(&row.org_id)
        .bind(&row.checksum)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_policies(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.policies {
        sqlx::query(
            "INSERT INTO policies
             (id, org_id, name, authz_id, last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 authz_id = EXCLUDED.authz_id,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(&row.authz_id)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_policy_groups(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.policy_groups {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO policy_groups
             (id, org_id, name, authz_id, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 name = EXCLUDED.name,
                 authz_id = EXCLUDED.authz_id,
                 serialized_object = EXCLUDED.serialized_object,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.name)
        .bind(&row.authz_id)
        .bind(serialized_object.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_policy_revisions(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
    compression: Compression,
) -> Result<(), StoreError> {
    for row in &snapshot.policy_revisions {
        let serialized_object =
            encode_serialized_optional(row.serialized_object.as_deref(), compression)?;
        sqlx::query(
            "INSERT INTO policy_revisions
             (id, org_id, policy_id, revision_id, name, serialized_object,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 policy_id = EXCLUDED.policy_id,
                 revision_id = EXCLUDED.revision_id,
                 name = EXCLUDED.name,
                 serialized_object = EXCLUDED.serialized_object,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.policy_id)
        .bind(&row.revision_id)
        .bind(&row.name)
        .bind(serialized_object.as_deref())
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_policy_group_revision_associations(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.policy_group_revision_associations {
        sqlx::query(
            "INSERT INTO policy_group_revision_associations
             (id, org_id, policy_revision_id, policy_group_authz_id,
              policy_revision_name, policy_revision_revision_id, policy_group_name,
              last_updated_by, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (id) DO UPDATE SET
                 org_id = EXCLUDED.org_id,
                 policy_revision_id = EXCLUDED.policy_revision_id,
                 policy_group_authz_id = EXCLUDED.policy_group_authz_id,
                 policy_revision_name = EXCLUDED.policy_revision_name,
                 policy_revision_revision_id = EXCLUDED.policy_revision_revision_id,
                 policy_group_name = EXCLUDED.policy_group_name,
                 last_updated_by = EXCLUDED.last_updated_by,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(&row.id)
        .bind(&row.org_id)
        .bind(&row.policy_revision_id)
        .bind(&row.policy_group_authz_id)
        .bind(&row.policy_revision_name)
        .bind(&row.policy_revision_revision_id)
        .bind(&row.policy_group_name)
        .bind(&row.last_updated_by)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn import_oauth_applications(
    transaction: &mut PgTransaction<'_>,
    snapshot: &StoreSnapshot,
) -> Result<(), StoreError> {
    for row in &snapshot.oauth_applications {
        sqlx::query(
            "INSERT INTO oauth_applications
             (id, name, uid, secret, redirect_uri, scopes, confidential, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (id) DO UPDATE SET
                 name = EXCLUDED.name,
                 uid = EXCLUDED.uid,
                 secret = EXCLUDED.secret,
                 redirect_uri = EXCLUDED.redirect_uri,
                 scopes = EXCLUDED.scopes,
                 confidential = EXCLUDED.confidential,
                 created_at = EXCLUDED.created_at,
                 updated_at = EXCLUDED.updated_at",
        )
        .bind(row.id)
        .bind(&row.name)
        .bind(&row.uid)
        .bind(&row.secret)
        .bind(&row.redirect_uri)
        .bind(&row.scopes)
        .bind(row.confidential)
        .bind(&row.created_at)
        .bind(&row.updated_at)
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    }
    Ok(())
}

async fn advance_sequences(transaction: &mut PgTransaction<'_>) -> Result<(), StoreError> {
    for table in [
        "cookbooks",
        "cookbook_artifacts",
        "cookbook_artifact_versions",
        "oauth_applications",
    ] {
        let sql = format!(
            "SELECT setval(
                 pg_get_serial_sequence('{table}', 'id'),
                 GREATEST(COALESCE((SELECT MAX(id) FROM {table}), 1), 1),
                 EXISTS (SELECT 1 FROM {table})
             )"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
    }
    Ok(())
}
