//! Offline importer for `knife-ec-backup` repositories.
//!
//! `knife ec backup` writes a ChefFS repository rather than a database dump:
//! users and their ACLs, one directory per organization, API objects as JSON,
//! cookbooks as directories with `metadata.rb`/`metadata.json` and an optional
//! `status.json`, plus optional `key_dump.json`/`key_table_dump.json` files.
//!
//! This module turns that layout into the same [`StoreSnapshot`] used by the
//! snapshot importer. It never calls the HTTP API, never logs file contents,
//! and only writes through the metadata store and local object store APIs.
//! Cookbook files are content-addressed by MD5 and copied into the local
//! `bookshelf/` root before the metadata transaction starts.

use crate::{runtime, validate_postgres_url, Compression, MigrateError, DATABASE_FILE};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use microwave_objectstore::{md5_hex, LocalObjectStore, ObjectStore};
use microwave_store::{
    parse_version, AclEntry, ActorKeyRow, ActorType, AuthzAclActorRow, AuthzAclGroupRow,
    AuthzEntityRow, AuthzGroupActorRow, AuthzGroupGroupRow, AuthzRepository, ChecksumRow,
    ClientRow, ContainerRow, CookbookRow, CookbookVersionChecksumRow, CookbookVersionRow,
    DataBagItemRow, GlobalGroupRow, GroupMembers, GroupRow, IdentityRepository, NamedObjectRow,
    NodeRow, OrgRow, OrgUserAssociationRow, OrgUserInviteRow, Permission, StoreError,
    StoreSnapshot, User, UserRow,
};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

const IMPORTER_SCOPE: &str = "microwave-migrate/knife-ec-backup/v1";
const DEFAULT_CONTAINERS: [&str; 12] = [
    "clients",
    "containers",
    "cookbooks",
    "data",
    "environments",
    "groups",
    "nodes",
    "roles",
    "sandboxes",
    "policies",
    "policy_groups",
    "cookbook_artifacts",
];
const DEFAULT_GROUPS: [&str; 5] = [
    "admins",
    "billing-admins",
    "clients",
    "users",
    "public_key_read_access",
];
const SEGMENTS: [&str; 9] = [
    "recipes",
    "definitions",
    "libraries",
    "attributes",
    "files",
    "templates",
    "resources",
    "providers",
    "root_files",
];
const PERMISSIONS: [&str; 5] = ["create", "read", "update", "delete", "grant"];

/// Counts and warnings produced by parsing one backup directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnifeBackupSummary {
    pub users: usize,
    pub user_acls: usize,
    pub organizations: usize,
    pub org_user_associations: usize,
    pub org_user_invitations: usize,
    pub clients: usize,
    pub actor_keys: usize,
    pub containers: usize,
    pub groups: usize,
    pub nodes: usize,
    pub roles: usize,
    pub environments: usize,
    pub data_bags: usize,
    pub data_bag_items: usize,
    pub cookbooks: usize,
    pub cookbook_versions: usize,
    pub cookbook_files: usize,
    pub checksums: usize,
    pub acls: usize,
    pub blobs: usize,
    pub warnings: Vec<String>,
}

impl KnifeBackupSummary {
    /// Human-readable one-line summary that never includes key material.
    pub fn display_line(&self) -> String {
        format!(
            "{} users, {} organizations, {} clients, {} keys, {} containers, {} groups, \
             {} nodes, {} roles, {} environments, {} data bags ({} items), {} cookbooks \
             ({} versions, {} files), {} checksums, {} ACLs, {} blobs",
            self.users,
            self.organizations,
            self.clients,
            self.actor_keys,
            self.containers,
            self.groups,
            self.nodes,
            self.roles,
            self.environments,
            self.data_bags,
            self.data_bag_items,
            self.cookbooks,
            self.cookbook_versions,
            self.cookbook_files,
            self.checksums,
            self.acls,
            self.blobs
        )
    }
}

/// A parsed backup, ready to apply to a store.
///
/// `snapshot` contains decoded JSON bodies and all authz relationships;
/// `blobs` contains cookbook contents that must be placed in the object store
/// under `(org_id, checksum)` before the snapshot is imported.
#[derive(Clone)]
pub struct KnifeBackupPlan {
    pub snapshot: StoreSnapshot,
    pub blobs: Vec<BackupBlob>,
    pub summary: KnifeBackupSummary,
    /// ACL targets present in the backup, including targets whose ACEs are
    /// empty. Used by `--replace-acls`.
    pub acl_targets: Vec<String>,
    /// Group authz ids read from group files in the backup. Used by
    /// `--replace-acls` to replace group membership.
    pub group_authz_ids: Vec<String>,
}

impl std::fmt::Debug for KnifeBackupPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KnifeBackupPlan")
            .field("summary", &self.summary)
            .field("blobs", &self.blobs.len())
            .field("acl_targets", &self.acl_targets.len())
            .field("group_authz_ids", &self.group_authz_ids.len())
            .finish()
    }
}

/// Apply-time behavior for a parsed knife backup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KnifeBackupApplyOptions {
    /// Reuse an existing target user with the same username instead of failing
    /// on the unique username constraint.
    pub merge_users: bool,
    /// Replace ACLs and group membership for targets present in the backup.
    pub replace_acls: bool,
    /// Replace a merged user's existing credentials, admin flag, profile, and
    /// default key with backup values.
    pub replace_user_credentials: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct BackupBlob {
    pub org_id: String,
    pub checksum: String,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for BackupBlob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackupBlob")
            .field("org_id", &self.org_id)
            .field("checksum", &self.checksum)
            .field("bytes", &self.data.len())
            .finish()
    }
}

#[derive(Debug, Clone, Default)]
struct KeyDumpUser {
    id: Option<String>,
    authz_id: Option<String>,
    password_hash: String,
    password_salt: String,
    hash_type: String,
    admin: Option<bool>,
    public_key: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone)]
struct KeyTableRow {
    name: String,
    actor_type: String,
    org_name: Option<String>,
    key_name: String,
    public_key: String,
    key_version: i64,
    expires_at: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct InlineKey {
    actor_id: String,
    actor_type: &'static str,
    actor_name: String,
    org_name: Option<String>,
    public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeyIdentity {
    actor_type: String,
    actor_name: String,
    org_name: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingAcl {
    path: PathBuf,
    org_name: Option<String>,
    kind: String,
    name: String,
}

/// Parse a `knife-ec-backup` directory without touching a database or object
/// store. This is also the dry-run path.
pub fn prepare_knife_backup(input: &Path) -> Result<KnifeBackupPlan, MigrateError> {
    if !input.is_dir() {
        return Err(MigrateError::InvalidBackup {
            path: input.to_path_buf(),
            message: "not a directory".into(),
        });
    }
    let users = input.join("users");
    let organizations = input.join("organizations");
    for directory in [&users, &organizations] {
        if directory.exists() {
            reject_symlink(directory)?;
        }
    }
    if !users.is_dir() && !organizations.is_dir() {
        return Err(MigrateError::InvalidBackup {
            path: input.to_path_buf(),
            message: "expected a users/ or organizations/ directory".into(),
        });
    }

    let mut builder = Builder::new(input);
    builder.read_key_dump()?;
    builder.read_users()?;
    builder.read_user_acls()?;
    builder.read_organizations()?;
    builder.read_key_table()?;
    builder.add_inline_default_keys();
    builder.ensure_global_defaults();
    builder.apply_acls()?;
    Ok(builder.finish())
}

/// Import a backup into `<data_dir>/microwave.db` and
/// `<data_dir>/bookshelf`, using the default compression.
pub fn import_knife_backup(
    data_dir: &Path,
    input: &Path,
) -> Result<KnifeBackupSummary, MigrateError> {
    import_knife_backup_to(
        data_dir,
        input,
        &data_dir.join("bookshelf"),
        Compression::default(),
    )
}

/// Import a backup into a SQLite data directory with an explicit compression
/// mode and object-store root.
pub fn import_knife_backup_to(
    data_dir: &Path,
    input: &Path,
    bookshelf_dir: &Path,
    compression: Compression,
) -> Result<KnifeBackupSummary, MigrateError> {
    let plan = prepare_knife_backup(input)?;
    apply_knife_backup_plan_sqlite(data_dir, &plan, bookshelf_dir, compression)
}

/// Import a backup into PostgreSQL using the default compression.
pub fn import_knife_backup_postgres(
    database_url: &str,
    input: &Path,
    bookshelf_dir: &Path,
) -> Result<KnifeBackupSummary, MigrateError> {
    import_knife_backup_postgres_with_compression(
        database_url,
        input,
        bookshelf_dir,
        Compression::default(),
    )
}

/// Import a backup into PostgreSQL with an explicit compression mode.
pub fn import_knife_backup_postgres_with_compression(
    database_url: &str,
    input: &Path,
    bookshelf_dir: &Path,
    compression: Compression,
) -> Result<KnifeBackupSummary, MigrateError> {
    let plan = prepare_knife_backup(input)?;
    apply_knife_backup_plan_postgres(database_url, &plan, bookshelf_dir, compression)
}

/// Apply an already-parsed backup to SQLite with default merge behavior.
pub fn apply_knife_backup_plan_sqlite(
    data_dir: &Path,
    plan: &KnifeBackupPlan,
    bookshelf_dir: &Path,
    compression: Compression,
) -> Result<KnifeBackupSummary, MigrateError> {
    apply_knife_backup_plan_sqlite_with_options(
        data_dir,
        plan,
        bookshelf_dir,
        compression,
        &KnifeBackupApplyOptions::default(),
    )
}

/// Apply an already-parsed backup to SQLite with explicit merge behavior.
pub fn apply_knife_backup_plan_sqlite_with_options(
    data_dir: &Path,
    plan: &KnifeBackupPlan,
    bookshelf_dir: &Path,
    compression: Compression,
    options: &KnifeBackupApplyOptions,
) -> Result<KnifeBackupSummary, MigrateError> {
    let store = microwave_store::SqliteStore::open_with_compression(
        &data_dir.join(DATABASE_FILE),
        compression,
    )?;
    let existing_groups: Vec<(String, String)> = store
        .list_global_groups()?
        .into_iter()
        .map(|group| (group.name, group.authz_id))
        .collect();
    let existing_users = store.list_users()?;
    let mut existing_default_keys = HashSet::new();
    for user in &existing_users {
        if store
            .list_actor_keys(&user.id, ActorType::User)?
            .iter()
            .any(|key| key.key_name == "default")
        {
            existing_default_keys.insert(user.id.clone());
        }
    }
    let plan = reconcile_global_groups(plan, &existing_groups);
    let plan = reconcile_users(&plan, &existing_users, &existing_default_keys, options)?;
    write_blobs(bookshelf_dir, &plan.blobs)?;
    let runtime = runtime()?;
    let acl_state = if options.replace_acls {
        Some(runtime.block_on(capture_existing_acl_state(&store, &plan))?)
    } else {
        None
    };
    if let Some(state) = &acl_state {
        if let Err(error) = runtime.block_on(clear_existing_acl_state(&store, &plan)) {
            return Err(restore_after_failure(&runtime, &store, state, error));
        }
    }
    if let Err(error) = store.import_snapshot(&plan.snapshot) {
        if let Some(state) = &acl_state {
            return Err(restore_after_failure(&runtime, &store, state, error.into()));
        }
        return Err(error.into());
    }
    Ok(plan.summary)
}

/// Apply an already-parsed backup to PostgreSQL with default merge behavior.
pub fn apply_knife_backup_plan_postgres(
    database_url: &str,
    plan: &KnifeBackupPlan,
    bookshelf_dir: &Path,
    compression: Compression,
) -> Result<KnifeBackupSummary, MigrateError> {
    apply_knife_backup_plan_postgres_with_options(
        database_url,
        plan,
        bookshelf_dir,
        compression,
        &KnifeBackupApplyOptions::default(),
    )
}

/// Apply an already-parsed backup to PostgreSQL with explicit merge behavior.
pub fn apply_knife_backup_plan_postgres_with_options(
    database_url: &str,
    plan: &KnifeBackupPlan,
    bookshelf_dir: &Path,
    compression: Compression,
    options: &KnifeBackupApplyOptions,
) -> Result<KnifeBackupSummary, MigrateError> {
    validate_postgres_url(database_url)?;
    let runtime = runtime()?;
    let store = runtime.block_on(
        microwave_store_postgres::PostgresStore::connect_with_compression(
            database_url,
            compression,
        ),
    )?;
    let existing_groups: Vec<(String, String)> = runtime
        .block_on(store.list_global_groups())?
        .into_iter()
        .map(|group| (group.name, group.authz_id))
        .collect();
    let existing_users = runtime.block_on(store.list_users())?;
    let mut existing_default_keys = HashSet::new();
    for user in &existing_users {
        if runtime
            .block_on(store.list_actor_keys(&user.id, ActorType::User))?
            .iter()
            .any(|key| key.key_name == "default")
        {
            existing_default_keys.insert(user.id.clone());
        }
    }
    let plan = reconcile_global_groups(plan, &existing_groups);
    let plan = reconcile_users(&plan, &existing_users, &existing_default_keys, options)?;
    write_blobs(bookshelf_dir, &plan.blobs)?;
    let acl_state = if options.replace_acls {
        Some(runtime.block_on(capture_existing_acl_state(&store, &plan))?)
    } else {
        None
    };
    if let Some(state) = &acl_state {
        if let Err(error) = runtime.block_on(clear_existing_acl_state(&store, &plan)) {
            return Err(restore_after_failure(&runtime, &store, state, error));
        }
    }
    if let Err(error) = runtime.block_on(store.import_snapshot(&plan.snapshot)) {
        if let Some(state) = &acl_state {
            return Err(restore_after_failure(&runtime, &store, state, error.into()));
        }
        return Err(error.into());
    }
    Ok(plan.summary)
}

/// Reuse global groups that already exist under the same name.
///
/// Snapshot rows are keyed by authz id but `global_groups.name` is unique. A
/// target installation normally already has `server-admins` and may have the
/// per-organization read-access groups. Rewriting the plan's authz ids to the
/// existing ones keeps the import mergeable instead of failing on the unique
/// name constraint. The same rewrite also covers custom global groups.
fn reconcile_global_groups(
    plan: &KnifeBackupPlan,
    existing: &[(String, String)],
) -> KnifeBackupPlan {
    let mut snapshot = plan.snapshot.clone();
    let mut replacements: HashMap<String, String> = HashMap::new();
    for planned in &snapshot.global_groups {
        if let Some((_, existing_authz)) = existing.iter().find(|(name, _)| name == &planned.name) {
            if existing_authz != &planned.authz_id {
                replacements.insert(planned.authz_id.clone(), existing_authz.clone());
            }
        }
    }
    let existing_names: HashSet<&str> = existing.iter().map(|(name, _)| name.as_str()).collect();
    snapshot
        .global_groups
        .retain(|group| !existing_names.contains(group.name.as_str()));
    let mut acl_targets = plan.acl_targets.clone();
    let mut group_authz_ids = plan.group_authz_ids.clone();
    if replacements.is_empty() {
        return KnifeBackupPlan {
            snapshot,
            blobs: plan.blobs.clone(),
            summary: plan.summary.clone(),
            acl_targets,
            group_authz_ids,
        };
    }

    let rewrite = |value: &mut String| {
        if let Some(replacement) = replacements.get(value) {
            *value = replacement.clone();
        }
    };
    for target in &mut acl_targets {
        rewrite(target);
    }
    for group in &mut group_authz_ids {
        rewrite(group);
    }
    snapshot
        .authz_entities
        .retain(|entity| !replacements.contains_key(&entity.authz_id));
    for row in &mut snapshot.authz_acl_actor {
        rewrite(&mut row.target_authz_id);
        rewrite(&mut row.actor_authz_id);
    }
    for row in &mut snapshot.authz_acl_group {
        rewrite(&mut row.target_authz_id);
        rewrite(&mut row.group_authz_id);
    }
    for row in &mut snapshot.authz_group_actors {
        rewrite(&mut row.group_authz_id);
        rewrite(&mut row.actor_authz_id);
    }
    for row in &mut snapshot.authz_group_groups {
        rewrite(&mut row.parent_group_authz_id);
        rewrite(&mut row.child_group_authz_id);
    }

    KnifeBackupPlan {
        snapshot,
        blobs: plan.blobs.clone(),
        summary: plan.summary.clone(),
        acl_targets,
        group_authz_ids,
    }
}

/// Reuse existing users with the same username.
///
/// This rewrites the source user's id/authz id and every reference to them so
/// snapshot import updates the existing row instead of colliding on the unique
/// username. Without `merge`, a collision is an explicit error.
fn reconcile_users(
    plan: &KnifeBackupPlan,
    existing: &[User],
    existing_default_keys: &HashSet<String>,
    options: &KnifeBackupApplyOptions,
) -> Result<KnifeBackupPlan, MigrateError> {
    let mut id_replacements: HashMap<String, String> = HashMap::new();
    let mut authz_replacements: HashMap<String, String> = HashMap::new();
    let mut merged_users: Vec<User> = Vec::new();
    for planned in &plan.snapshot.users {
        let Some(existing) = existing
            .iter()
            .find(|user| user.username == planned.username)
        else {
            continue;
        };
        if existing.id == planned.id && existing.authz_id == planned.authz_id {
            continue;
        }
        if !options.merge_users {
            return Err(MigrateError::ExistingUser {
                username: planned.username.clone(),
            });
        }
        merged_users.push(existing.clone());
        if existing.id != planned.id {
            id_replacements.insert(planned.id.clone(), existing.id.clone());
        }
        if existing.authz_id != planned.authz_id {
            authz_replacements.insert(planned.authz_id.clone(), existing.authz_id.clone());
        }
    }
    if id_replacements.is_empty() && authz_replacements.is_empty() {
        return Ok(plan.clone());
    }

    let mut snapshot = plan.snapshot.clone();
    let mut acl_targets = plan.acl_targets.clone();
    let mut summary = plan.summary.clone();
    let replace_id = |value: &mut String| {
        if let Some(replacement) = id_replacements.get(value) {
            *value = replacement.clone();
        }
    };
    let replace_authz = |value: &mut String| {
        if let Some(replacement) = authz_replacements.get(value) {
            *value = replacement.clone();
        }
    };

    for user in &mut snapshot.users {
        replace_id(&mut user.id);
        replace_authz(&mut user.authz_id);
        if let Some(existing) = merged_users
            .iter()
            .find(|existing| existing.username == user.username)
        {
            if !options.replace_user_credentials {
                preserve_user_profile(user, existing);
            }
        }
    }
    for key in &mut snapshot.actor_keys {
        if key.actor_type == "user" {
            replace_id(&mut key.actor_id);
        }
    }
    if !options.replace_user_credentials {
        snapshot.actor_keys.retain(|key| {
            !(key.actor_type == "user"
                && key.key_name == "default"
                && existing_default_keys.contains(&key.actor_id))
        });
    }
    for existing in &merged_users {
        if !snapshot
            .actor_keys
            .iter()
            .any(|key| key.actor_type == "user" && key.actor_id == existing.id)
        {
            summary.warnings.push(format!(
                "merged user {:?} has no imported key material; existing target keys were kept",
                existing.username
            ));
        }
    }
    for association in &mut snapshot.org_user_associations {
        replace_id(&mut association.user_id);
    }
    for invite in &mut snapshot.org_user_invites {
        replace_id(&mut invite.user_id);
    }
    snapshot
        .authz_entities
        .retain(|entity| !authz_replacements.contains_key(&entity.authz_id));
    for row in &mut snapshot.authz_acl_actor {
        replace_authz(&mut row.target_authz_id);
        replace_authz(&mut row.actor_authz_id);
    }
    for row in &mut snapshot.authz_acl_group {
        replace_authz(&mut row.target_authz_id);
    }
    for row in &mut snapshot.authz_group_actors {
        replace_authz(&mut row.actor_authz_id);
    }
    for target in &mut acl_targets {
        replace_authz(target);
    }
    summary.warnings.sort();
    summary.warnings.dedup();

    Ok(KnifeBackupPlan {
        snapshot,
        blobs: plan.blobs.clone(),
        summary,
        acl_targets,
        group_authz_ids: plan.group_authz_ids.clone(),
    })
}

fn preserve_user_profile(user: &mut UserRow, existing: &User) {
    if !existing.password_hash.is_empty() {
        user.password_hash = existing.password_hash.clone();
        user.password_salt = existing.password_salt.clone();
        if !existing.hash_type.is_empty() {
            user.hash_type = existing.hash_type.clone();
        }
    }
    user.admin = existing.admin;
    user.recovery_authentication_enabled = existing.recovery_authentication_enabled;
    preserve_option(&mut user.email, &existing.email);
    preserve_option(
        &mut user.external_authentication_uid,
        &existing.external_authentication_uid,
    );
    preserve_option(&mut user.display_name, &existing.display_name);
    preserve_option(&mut user.first_name, &existing.first_name);
    preserve_option(&mut user.middle_name, &existing.middle_name);
    preserve_option(&mut user.last_name, &existing.last_name);
}

fn preserve_option(target: &mut Option<String>, existing: &Option<String>) {
    if existing.as_deref().is_some_and(|value| !value.is_empty()) {
        *target = existing.clone();
    }
}

struct CapturedAclState {
    targets: Vec<(String, Vec<AclEntry>)>,
    groups: Vec<(String, String, GroupMembers)>,
}

async fn capture_existing_acl_state<S: AuthzRepository + ?Sized>(
    store: &S,
    plan: &KnifeBackupPlan,
) -> Result<CapturedAclState, MigrateError> {
    let mut targets = Vec::with_capacity(plan.acl_targets.len());
    for target in &plan.acl_targets {
        match store.get_acl_entries(target).await {
            Ok(entries) => targets.push((target.clone(), entries)),
            Err(StoreError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    let mut groups = Vec::with_capacity(plan.group_authz_ids.len());
    for group_authz_id in &plan.group_authz_ids {
        let org_id = plan
            .snapshot
            .groups
            .iter()
            .find(|group| &group.authz_id == group_authz_id)
            .map(|group| group.org_id.clone())
            .unwrap_or_default();
        let members = store.list_group_members(&org_id, group_authz_id).await?;
        groups.push((org_id, group_authz_id.clone(), members));
    }
    Ok(CapturedAclState { targets, groups })
}

async fn clear_existing_acl_state<S: AuthzRepository + ?Sized>(
    store: &S,
    plan: &KnifeBackupPlan,
) -> Result<(), MigrateError> {
    for target in &plan.acl_targets {
        match store.get_acl_entries(target).await {
            Ok(_) => {
                for permission in Permission::ALL {
                    store
                        .set_acl_for_permission_split(target, permission, &[], &[], &[])
                        .await?;
                }
            }
            Err(StoreError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    for group_authz_id in &plan.group_authz_ids {
        let org_id = plan
            .snapshot
            .groups
            .iter()
            .find(|group| &group.authz_id == group_authz_id)
            .map(|group| group.org_id.clone())
            .unwrap_or_default();
        store
            .replace_group_members(&org_id, group_authz_id, Some(&[]), Some(&[]), Some(&[]))
            .await?;
    }
    Ok(())
}

async fn restore_acl_state<S: AuthzRepository + ?Sized>(
    store: &S,
    state: &CapturedAclState,
) -> Result<(), MigrateError> {
    for (target, entries) in &state.targets {
        for entry in entries {
            store
                .set_acl_for_permission_split(
                    target,
                    entry.permission,
                    &entry.users,
                    &entry.clients,
                    &entry.groups,
                )
                .await?;
        }
    }
    for (org_id, group_authz_id, members) in &state.groups {
        store
            .replace_group_members(
                org_id,
                group_authz_id,
                Some(&members.users),
                Some(&members.clients),
                Some(&members.groups),
            )
            .await?;
    }
    Ok(())
}

fn restore_after_failure<S: AuthzRepository + ?Sized>(
    runtime: &tokio::runtime::Runtime,
    store: &S,
    state: &CapturedAclState,
    error: MigrateError,
) -> MigrateError {
    match runtime.block_on(restore_acl_state(store, state)) {
        Ok(()) => error,
        Err(restore_error) => MigrateError::AclRollback(format!(
            "import failed: {error}; restoring the previous ACL state also failed: {restore_error}"
        )),
    }
}

fn write_blobs(root: &Path, blobs: &[BackupBlob]) -> Result<(), MigrateError> {
    if blobs.is_empty() {
        return Ok(());
    }
    let store = LocalObjectStore::new(root, "", "http://localhost");
    for blob in blobs {
        store.put(&blob.org_id, &blob.checksum, &blob.data)?;
    }
    Ok(())
}

struct Builder {
    input: PathBuf,
    generated_at: String,
    data: StoreSnapshot,
    blobs: BTreeMap<(String, String), Vec<u8>>,
    summary: KnifeBackupSummary,
    key_dump: HashMap<String, KeyDumpUser>,
    user_authz: HashMap<String, String>,
    org_ids: HashMap<String, String>,
    org_authz: HashMap<String, String>,
    client_ids: HashMap<(String, String), String>,
    client_authz: HashMap<(String, String), String>,
    group_authz: HashMap<(String, String), String>,
    global_group_authz: HashMap<String, String>,
    container_authz: HashMap<(String, String), String>,
    object_authz: HashMap<(String, String, String), String>,
    cookbook_ids: HashMap<(String, String), i64>,
    cookbook_authz: HashMap<(String, String), String>,
    pending_acls: Vec<PendingAcl>,
    acl_targets: HashSet<String>,
    backup_group_authz_ids: HashSet<String>,
    skipped_default_keys: HashSet<KeyIdentity>,
    inline_keys: Vec<InlineKey>,
    seen_users: HashSet<String>,
    seen_orgs: HashSet<String>,
    seen_authz: HashSet<String>,
    seen_clients: HashSet<(String, String)>,
    seen_containers: HashSet<(String, String)>,
    seen_groups: HashSet<(String, String)>,
    seen_group_actors: HashSet<(String, String)>,
    seen_group_groups: HashSet<(String, String)>,
    seen_objects: HashSet<(String, String, String)>,
    seen_acl_actor: HashSet<(String, String, String)>,
    seen_acl_group: HashSet<(String, String, String)>,
    seen_checksums: HashSet<(String, String)>,
    seen_cookbooks: HashSet<(String, String)>,
    seen_cookbook_versions: HashSet<(String, String)>,
    warning_set: HashSet<String>,
}

impl Builder {
    fn new(input: &Path) -> Self {
        Self {
            input: input.to_path_buf(),
            generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            data: StoreSnapshot::default(),
            blobs: BTreeMap::new(),
            summary: KnifeBackupSummary::default(),
            key_dump: HashMap::new(),
            user_authz: HashMap::new(),
            org_ids: HashMap::new(),
            org_authz: HashMap::new(),
            client_ids: HashMap::new(),
            client_authz: HashMap::new(),
            group_authz: HashMap::new(),
            global_group_authz: HashMap::new(),
            container_authz: HashMap::new(),
            object_authz: HashMap::new(),
            cookbook_ids: HashMap::new(),
            cookbook_authz: HashMap::new(),
            pending_acls: Vec::new(),
            acl_targets: HashSet::new(),
            backup_group_authz_ids: HashSet::new(),
            skipped_default_keys: HashSet::new(),
            inline_keys: Vec::new(),
            seen_users: HashSet::new(),
            seen_orgs: HashSet::new(),
            seen_authz: HashSet::new(),
            seen_clients: HashSet::new(),
            seen_containers: HashSet::new(),
            seen_groups: HashSet::new(),
            seen_group_actors: HashSet::new(),
            seen_group_groups: HashSet::new(),
            seen_objects: HashSet::new(),
            seen_acl_actor: HashSet::new(),
            seen_acl_group: HashSet::new(),
            seen_checksums: HashSet::new(),
            seen_cookbooks: HashSet::new(),
            seen_cookbook_versions: HashSet::new(),
            warning_set: HashSet::new(),
        }
    }

    fn finish(mut self) -> KnifeBackupPlan {
        self.summary.blobs = self.blobs.len();
        self.summary.warnings = self.warning_set.into_iter().collect();
        self.summary.warnings.sort();
        let blobs = self
            .blobs
            .into_iter()
            .map(|((org_id, checksum), data)| BackupBlob {
                org_id,
                checksum,
                data,
            })
            .collect();
        let mut acl_targets: Vec<String> = self.acl_targets.into_iter().collect();
        acl_targets.sort();
        let mut group_authz_ids: Vec<String> = self.backup_group_authz_ids.into_iter().collect();
        group_authz_ids.sort();
        KnifeBackupPlan {
            snapshot: self.data,
            blobs,
            summary: self.summary,
            acl_targets,
            group_authz_ids,
        }
    }

    fn warn(&mut self, message: impl Into<String>) {
        self.warning_set.insert(message.into());
    }

    fn ensure_authz(&mut self, authz_id: &str, entity_type: &str, org_id: Option<&str>) {
        if self.seen_authz.insert(authz_id.to_string()) {
            self.data.authz_entities.push(AuthzEntityRow {
                authz_id: authz_id.to_string(),
                entity_type: entity_type.to_string(),
                org_id: org_id.map(str::to_string),
                created_at: self.generated_at.clone(),
            });
        }
    }

    // ------------------------------------------------------------------
    // Users, passwords, and keys
    // ------------------------------------------------------------------

    fn read_key_dump(&mut self) -> Result<(), MigrateError> {
        let path = self.input.join("key_dump.json");
        if !path.is_file() {
            self.warn("key_dump.json is absent; legacy user password hashes cannot be imported");
            return Ok(());
        }
        let rows = json_rows(read_json(&path)?);
        for row in rows {
            let Some(username) = string_field(&row, "username") else {
                self.warn("key_dump.json contains a row without a username");
                continue;
            };
            let password_hash = string_field(&row, "hashed_password")
                .or_else(|| string_field(&row, "password_hash"))
                .unwrap_or_default();
            let password_salt = string_field(&row, "salt")
                .or_else(|| string_field(&row, "password_salt"))
                .unwrap_or_default();
            let entry = KeyDumpUser {
                id: usable_id(string_field(&row, "id")),
                authz_id: usable_id(string_field(&row, "authz_id")),
                password_hash,
                password_salt,
                hash_type: string_field(&row, "hash_type").unwrap_or_default(),
                admin: bool_field(&row, "admin"),
                public_key: non_empty_string(&row, "public_key"),
                created_at: timestamp_field(&row, "created_at"),
                updated_at: timestamp_field(&row, "updated_at"),
            };
            if self.key_dump.insert(username.clone(), entry).is_some() {
                self.warn(format!(
                    "key_dump.json contains more than one row for user {username:?}; using the last"
                ));
            }
        }
        Ok(())
    }

    fn read_users(&mut self) -> Result<(), MigrateError> {
        let dir = self.input.join("users");
        for path in json_files_within(&self.input, &dir)? {
            let value = read_json(&path)?;
            let fallback = file_stem(&path);
            let Some(username) = string_field(&value, "username").or(fallback) else {
                self.warn(format!(
                    "skipping user file {} without a name",
                    path.display()
                ));
                continue;
            };
            if !self.seen_users.insert(username.clone()) {
                self.warn(format!("duplicate user {username:?}; keeping the first"));
                continue;
            }

            let dump = self.key_dump.get(&username).cloned();
            let id = dump
                .as_ref()
                .and_then(|dump| dump.id.clone())
                .or_else(|| usable_id(string_field(&value, "id")))
                .unwrap_or_else(|| stable_id(&format!("user:id:{username}")));
            let authz_id = dump
                .as_ref()
                .and_then(|dump| dump.authz_id.clone())
                .or_else(|| usable_id(string_field(&value, "authz_id")))
                .unwrap_or_else(|| stable_id(&format!("user:authz:{username}")));

            let (password_hash, password_salt, hash_type) = match dump.as_ref() {
                Some(dump) => (
                    dump.password_hash.clone(),
                    dump.password_salt.clone(),
                    if dump.hash_type.is_empty() {
                        string_field(&value, "hash_type").unwrap_or_else(|| "bcrypt".into())
                    } else {
                        dump.hash_type.clone()
                    },
                ),
                None => (
                    string_field(&value, "password_hash")
                        .or_else(|| string_field(&value, "hashed_password"))
                        .unwrap_or_default(),
                    string_field(&value, "password_salt")
                        .or_else(|| string_field(&value, "salt"))
                        .unwrap_or_default(),
                    string_field(&value, "hash_type").unwrap_or_else(|| "bcrypt".into()),
                ),
            };

            let created_at = dump
                .as_ref()
                .and_then(|dump| dump.created_at.clone())
                .or_else(|| timestamp_field(&value, "created_at"))
                .unwrap_or_else(|| self.generated_at.clone());
            let updated_at = dump
                .as_ref()
                .and_then(|dump| dump.updated_at.clone())
                .or_else(|| timestamp_field(&value, "updated_at"))
                .unwrap_or_else(|| created_at.clone());

            self.ensure_authz(&authz_id, "actor", None);
            self.user_authz.insert(username.clone(), authz_id.clone());
            self.data.users.push(UserRow {
                id: id.clone(),
                authz_id: authz_id.clone(),
                username: username.clone(),
                email: email_field(&value),
                password_hash,
                password_salt,
                hash_type,
                admin: dump
                    .as_ref()
                    .and_then(|dump| dump.admin)
                    .or_else(|| bool_field(&value, "admin"))
                    .unwrap_or(false),
                recovery_authentication_enabled: bool_field(
                    &value,
                    "recovery_authentication_enabled",
                )
                .unwrap_or(false),
                external_authentication_uid: non_empty_string(
                    &value,
                    "external_authentication_uid",
                ),
                display_name: non_empty_string(&value, "display_name"),
                first_name: non_empty_string(&value, "first_name"),
                middle_name: non_empty_string(&value, "middle_name"),
                last_name: non_empty_string(&value, "last_name"),
                last_updated_by: String::new(),
                created_at,
                updated_at,
            });
            self.summary.users += 1;

            if let Some(public_key) = dump
                .as_ref()
                .and_then(|dump| dump.public_key.clone())
                .or_else(|| non_empty_string(&value, "public_key"))
            {
                self.inline_keys.push(InlineKey {
                    actor_id: id,
                    actor_type: "user",
                    actor_name: username.clone(),
                    org_name: None,
                    public_key,
                });
            }
        }
        Ok(())
    }

    fn read_user_acls(&mut self) -> Result<(), MigrateError> {
        let dir = self.input.join("user_acls");
        for path in json_files_within(&self.input, &dir)? {
            let Some(username) = file_stem(&path) else {
                continue;
            };
            self.pending_acls.push(PendingAcl {
                path,
                org_name: None,
                kind: "users".into(),
                name: username,
            });
        }
        Ok(())
    }

    fn read_key_table(&mut self) -> Result<(), MigrateError> {
        let path = self.input.join("key_table_dump.json");
        if !path.is_file() {
            self.warn(
                "key_table_dump.json is absent; only inline default public keys can be imported",
            );
            return Ok(());
        }
        for row in json_rows(read_json(&path)?) {
            let Some(name) = string_field(&row, "name") else {
                self.warn("key_table_dump.json contains a row without a name");
                continue;
            };
            let actor_type = string_field(&row, "type").unwrap_or_default();
            let key_name = non_empty_string(&row, "key_name").unwrap_or_else(|| "default".into());
            let org_name = non_empty_string(&row, "org_name");
            let Some(public_key) = non_empty_string(&row, "public_key") else {
                self.warn(format!(
                    "key_table_dump.json entry for {name:?} has no public key; skipping"
                ));
                continue;
            };
            let expires_at = match normalize_expires_at(row.get("expires_at")) {
                Ok(expires_at) => expires_at,
                Err(message) => {
                    if key_name == "default" {
                        self.skipped_default_keys.insert(KeyIdentity {
                            actor_type: actor_type.clone(),
                            actor_name: name.clone(),
                            org_name: org_name.clone(),
                        });
                    }
                    self.warn(format!(
                        "key_table_dump.json entry for {name:?} has invalid expires_at {message}; skipping"
                    ));
                    continue;
                }
            };
            let created_at =
                timestamp_field(&row, "created_at").unwrap_or_else(|| self.generated_at.clone());
            let updated_at =
                timestamp_field(&row, "updated_at").unwrap_or_else(|| created_at.clone());
            let key = KeyTableRow {
                name,
                actor_type,
                org_name,
                key_name,
                public_key,
                key_version: integer_field(&row, "key_version").unwrap_or(1),
                expires_at,
                created_at,
                updated_at,
            };
            self.append_key_table_row(key);
        }
        Ok(())
    }

    fn append_key_table_row(&mut self, key: KeyTableRow) {
        let actor_id = match key.actor_type.as_str() {
            "user" => self.user_authz.contains_key(&key.name).then(|| {
                self.data
                    .users
                    .iter()
                    .find(|user| user.username == key.name)
                    .map(|user| user.id.clone())
            }),
            "client" => {
                let Some(org_name) = key.org_name.as_deref() else {
                    self.warn(format!(
                        "client key for {:?} has no org_name; skipping",
                        key.name
                    ));
                    return;
                };
                self.client_ids
                    .get(&(org_name.to_string(), key.name.clone()))
                    .cloned()
                    .map(Some)
            }
            other => {
                self.warn(format!(
                    "key_table_dump.json has an unsupported actor type {other:?}; skipping"
                ));
                return;
            }
        };
        let Some(Some(actor_id)) = actor_id else {
            self.warn(format!(
                "key_table_dump.json references unknown {} {:?}; skipping",
                key.actor_type, key.name
            ));
            return;
        };
        if self
            .data
            .actor_keys
            .iter()
            .any(|existing| existing.actor_id == actor_id && existing.key_name == key.key_name)
        {
            self.warn(format!(
                "duplicate key {} for {} {:?}; using the first",
                key.key_name, key.actor_type, key.name
            ));
            return;
        }
        self.data.actor_keys.push(ActorKeyRow {
            actor_id,
            actor_type: key.actor_type,
            key_name: key.key_name,
            public_key: key.public_key,
            key_version: key.key_version,
            expires_at: key.expires_at,
            last_updated_by: String::new(),
            created_at: key.created_at,
            updated_at: key.updated_at,
        });
        self.summary.actor_keys += 1;
    }

    fn add_inline_default_keys(&mut self) {
        for key in std::mem::take(&mut self.inline_keys) {
            let identity = KeyIdentity {
                actor_type: key.actor_type.to_string(),
                actor_name: key.actor_name.clone(),
                org_name: key.org_name.clone(),
            };
            if self.skipped_default_keys.contains(&identity)
                || self.data.actor_keys.iter().any(|existing| {
                    existing.actor_id == key.actor_id && existing.key_name == "default"
                })
            {
                continue;
            }
            self.data.actor_keys.push(ActorKeyRow {
                actor_id: key.actor_id,
                actor_type: key.actor_type.into(),
                key_name: "default".into(),
                public_key: key.public_key,
                key_version: 1,
                expires_at: "infinity".into(),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.actor_keys += 1;
        }
    }

    // ------------------------------------------------------------------
    // Organizations and their objects
    // ------------------------------------------------------------------

    fn read_organizations(&mut self) -> Result<(), MigrateError> {
        let dir = self.input.join("organizations");
        for path in sorted_entries_within(&self.input, &dir)? {
            reject_symlink(&path)?;
            if !path.is_dir() {
                continue;
            }
            self.read_organization(&path)?;
        }
        Ok(())
    }

    fn read_organization(&mut self, dir: &Path) -> Result<(), MigrateError> {
        let org_json = dir.join("org.json");
        let value = if org_json.is_file() {
            read_json(&org_json)?
        } else {
            Value::Object(Map::new())
        };
        let Some(name) = string_field(&value, "name").or_else(|| dir_name(dir)) else {
            self.warn(format!(
                "skipping organization directory {} without a name",
                dir.display()
            ));
            return Ok(());
        };
        if !self.seen_orgs.insert(name.clone()) {
            self.warn(format!(
                "duplicate organization {name:?}; keeping the first"
            ));
            return Ok(());
        }

        let id = match usable_id(string_field(&value, "guid")) {
            Some(guid) => guid,
            None => stable_id(&format!("org:id:{name}")),
        };
        let authz_id = stable_id(&format!("org:authz:{name}"));
        let full_name = string_field(&value, "full_name").unwrap_or_else(|| name.clone());
        let created_at =
            timestamp_field(&value, "created_at").unwrap_or_else(|| self.generated_at.clone());
        let updated_at =
            timestamp_field(&value, "updated_at").unwrap_or_else(|| created_at.clone());
        let assigned_at = timestamp_field(&value, "assigned_at")
            .or_else(|| timestamp_field(&value, "created_at"))
            .unwrap_or_else(|| self.generated_at.clone());

        self.ensure_authz(&authz_id, "actor", Some(&id));
        self.org_ids.insert(name.clone(), id.clone());
        self.org_authz.insert(name.clone(), authz_id.clone());
        self.data.orgs.push(OrgRow {
            id,
            authz_id,
            name: name.clone(),
            full_name,
            assigned_at,
            last_updated_by: String::new(),
            created_at,
            updated_at,
        });
        self.summary.organizations += 1;

        self.read_clients(&name, dir)?;
        self.read_containers(&name, dir)?;
        self.read_groups(&name, dir)?;
        self.ensure_default_policy(&name)?;
        self.read_nodes(&name, dir)?;
        self.read_named_objects(&name, dir)?;
        self.read_data_bags(&name, dir)?;
        self.read_cookbooks(&name, dir)?;
        self.read_members(&name, dir)?;
        self.read_invitations(&name, dir)?;
        self.collect_org_acls(&name, dir)?;
        Ok(())
    }

    fn read_clients(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        for path in json_files_within(dir, &dir.join("clients"))? {
            let value = read_json(&path)?;
            let Some(name) = string_field(&value, "name")
                .or_else(|| string_field(&value, "clientname"))
                .or_else(|| file_stem(&path))
            else {
                self.warn(format!(
                    "skipping client file {} without a name",
                    path.display()
                ));
                continue;
            };
            if !self
                .seen_clients
                .insert((org_name.to_string(), name.clone()))
            {
                self.warn(format!(
                    "duplicate client {name:?} in organization {org_name:?}; keeping the first"
                ));
                continue;
            }
            let id = stable_id(&format!("client:id:{org_name}:{name}"));
            let authz_id = stable_id(&format!("client:authz:{org_name}:{name}"));
            self.ensure_authz(&authz_id, "actor", Some(&org_id));
            self.client_ids
                .insert((org_name.to_string(), name.clone()), id.clone());
            self.client_authz
                .insert((org_name.to_string(), name.clone()), authz_id.clone());
            let created_at =
                timestamp_field(&value, "created_at").unwrap_or_else(|| self.generated_at.clone());
            let updated_at =
                timestamp_field(&value, "updated_at").unwrap_or_else(|| created_at.clone());
            self.data.clients.push(ClientRow {
                id: id.clone(),
                org_id: org_id.clone(),
                authz_id,
                name: name.clone(),
                validator: bool_field(&value, "validator").unwrap_or(false),
                admin: bool_field(&value, "admin").unwrap_or(false),
                created_at,
                updated_at,
            });
            self.summary.clients += 1;
            if let Some(public_key) = non_empty_string(&value, "public_key") {
                self.inline_keys.push(InlineKey {
                    actor_id: id,
                    actor_type: "client",
                    actor_name: name.clone(),
                    org_name: Some(org_name.to_string()),
                    public_key,
                });
            }
        }
        Ok(())
    }

    fn read_containers(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        for path in json_files_within(dir, &dir.join("containers"))? {
            let value = read_json(&path)?;
            let Some(name) = string_field(&value, "containername")
                .or_else(|| string_field(&value, "name"))
                .or_else(|| file_stem(&path))
            else {
                self.warn(format!(
                    "skipping container file {} without a name",
                    path.display()
                ));
                continue;
            };
            if !self
                .seen_containers
                .insert((org_name.to_string(), name.clone()))
            {
                continue;
            }
            let id = stable_id(&format!("container:id:{org_name}:{name}"));
            let authz_id = stable_id(&format!("container:authz:{org_name}:{name}"));
            self.ensure_authz(&authz_id, "container", Some(&org_id));
            self.container_authz
                .insert((org_name.to_string(), name.clone()), authz_id.clone());
            self.data.containers.push(ContainerRow {
                id,
                org_id: org_id.clone(),
                authz_id,
                name,
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.containers += 1;
        }
        Ok(())
    }

    fn read_groups(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        let mut parsed = Vec::new();
        for path in json_files_within(dir, &dir.join("groups"))? {
            let value = read_json(&path)?;
            let Some(name) = string_field(&value, "groupname")
                .or_else(|| string_field(&value, "name"))
                .or_else(|| file_stem(&path))
            else {
                self.warn(format!(
                    "skipping group file {} without a name",
                    path.display()
                ));
                continue;
            };
            if !self
                .seen_groups
                .insert((org_name.to_string(), name.clone()))
            {
                self.warn(format!(
                    "duplicate group {name:?} in organization {org_name:?}; keeping the first"
                ));
                continue;
            }
            let id = stable_id(&format!("group:id:{org_name}:{name}"));
            let authz_id = stable_id(&format!("group:authz:{org_name}:{name}"));
            self.ensure_authz(&authz_id, "group", Some(&org_id));
            self.backup_group_authz_ids.insert(authz_id.clone());
            self.group_authz
                .insert((org_name.to_string(), name.clone()), authz_id.clone());
            self.data.groups.push(GroupRow {
                id,
                org_id: org_id.clone(),
                authz_id: authz_id.clone(),
                name: name.clone(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.groups += 1;
            parsed.push((name, authz_id, value));
        }

        for (group_name, group_authz, value) in parsed {
            for actor in string_array(&value, "actors") {
                if let Some(actor_authz) = self.resolve_actor(org_name, &actor) {
                    self.add_group_actor(&group_authz, &actor_authz);
                } else {
                    self.warn(format!(
                        "group {group_name:?} references unknown actor {actor:?}; skipping"
                    ));
                }
            }
            for user in string_array(&value, "users") {
                if let Some(authz) = self.user_authz.get(&user).cloned() {
                    self.add_group_actor(&group_authz, &authz);
                } else {
                    self.warn(format!(
                        "group {group_name:?} references unknown user {user:?}; skipping"
                    ));
                }
            }
            for client in string_array(&value, "clients") {
                if let Some(authz) = self
                    .client_authz
                    .get(&(org_name.to_string(), client.clone()))
                    .cloned()
                {
                    self.add_group_actor(&group_authz, &authz);
                } else {
                    self.warn(format!(
                        "group {group_name:?} references unknown client {client:?}; skipping"
                    ));
                }
            }
            for child in string_array(&value, "groups") {
                if let Some(child_authz) = self.resolve_group_authz(Some(org_name), &child) {
                    if self
                        .seen_group_groups
                        .insert((group_authz.clone(), child_authz.clone()))
                    {
                        self.data.authz_group_groups.push(AuthzGroupGroupRow {
                            parent_group_authz_id: group_authz.clone(),
                            child_group_authz_id: child_authz,
                        });
                    }
                } else {
                    self.warn(format!(
                        "group {group_name:?} references unknown child group {child:?}; skipping"
                    ));
                }
            }
        }
        Ok(())
    }

    fn add_group_actor(&mut self, group_authz: &str, actor_authz: &str) {
        if self
            .seen_group_actors
            .insert((group_authz.to_string(), actor_authz.to_string()))
        {
            self.data.authz_group_actors.push(AuthzGroupActorRow {
                group_authz_id: group_authz.to_string(),
                actor_authz_id: actor_authz.to_string(),
            });
        }
    }

    fn ensure_default_policy(&mut self, org_name: &str) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        for container in DEFAULT_CONTAINERS {
            if self
                .seen_containers
                .insert((org_name.to_string(), container.to_string()))
            {
                let id = stable_id(&format!("container:id:{org_name}:{container}"));
                let authz_id = stable_id(&format!("container:authz:{org_name}:{container}"));
                self.ensure_authz(&authz_id, "container", Some(&org_id));
                self.container_authz.insert(
                    (org_name.to_string(), container.to_string()),
                    authz_id.clone(),
                );
                self.data.containers.push(ContainerRow {
                    id,
                    org_id: org_id.clone(),
                    authz_id,
                    name: container.into(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
                self.summary.containers += 1;
            }
        }
        for group in DEFAULT_GROUPS {
            if self
                .seen_groups
                .insert((org_name.to_string(), group.to_string()))
            {
                let id = stable_id(&format!("group:id:{org_name}:{group}"));
                let authz_id = stable_id(&format!("group:authz:{org_name}:{group}"));
                self.ensure_authz(&authz_id, "group", Some(&org_id));
                self.group_authz
                    .insert((org_name.to_string(), group.to_string()), authz_id.clone());
                self.data.groups.push(GroupRow {
                    id,
                    org_id: org_id.clone(),
                    authz_id,
                    name: group.into(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
                self.summary.groups += 1;
            }
        }
        Ok(())
    }

    fn read_nodes(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        for path in json_files_within(dir, &dir.join("nodes"))? {
            let (value, body) = read_object(&path)?;
            let Some(name) = string_field(&value, "name").or_else(|| file_stem(&path)) else {
                continue;
            };
            if !self
                .seen_objects
                .insert((org_name.to_string(), "node".into(), name.clone()))
            {
                self.warn(format!(
                    "duplicate node {name:?} in organization {org_name:?}; keeping the first"
                ));
                continue;
            }
            let id = stable_id(&format!("node:id:{org_name}:{name}"));
            let authz_id = stable_id(&format!("node:authz:{org_name}:{name}"));
            self.ensure_authz(&authz_id, "object", Some(&org_id));
            self.object_authz.insert(
                (org_name.to_string(), "node".into(), name.clone()),
                authz_id.clone(),
            );
            self.data.nodes.push(NodeRow {
                id,
                authz_id,
                org_id: org_id.clone(),
                name,
                environment: string_field(&value, "chef_environment")
                    .unwrap_or_else(|| "_default".into()),
                policy_name: non_empty_string(&value, "policy_name"),
                policy_group: non_empty_string(&value, "policy_group"),
                serialized_object: Some(body),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.nodes += 1;
        }
        Ok(())
    }

    fn read_named_objects(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        for (kind, collection) in [("role", "roles"), ("environment", "environments")] {
            for path in json_files_within(dir, &dir.join(collection))? {
                let (value, body) = read_object(&path)?;
                let Some(name) = string_field(&value, "name").or_else(|| file_stem(&path)) else {
                    continue;
                };
                if !self
                    .seen_objects
                    .insert((org_name.to_string(), kind.to_string(), name.clone()))
                {
                    self.warn(format!(
                        "duplicate {kind} {name:?} in organization {org_name:?}; keeping the first"
                    ));
                    continue;
                }
                let id = stable_id(&format!("{kind}:id:{org_name}:{name}"));
                let authz_id = stable_id(&format!("{kind}:authz:{org_name}:{name}"));
                self.ensure_authz(&authz_id, "object", Some(&org_id));
                self.object_authz.insert(
                    (org_name.to_string(), kind.to_string(), name.clone()),
                    authz_id.clone(),
                );
                let row = NamedObjectRow {
                    id,
                    authz_id,
                    org_id: org_id.clone(),
                    name,
                    serialized_object: Some(body),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                };
                match kind {
                    "role" => {
                        self.data.roles.push(row);
                        self.summary.roles += 1;
                    }
                    "environment" => {
                        self.data.environments.push(row);
                        self.summary.environments += 1;
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }

    fn read_data_bags(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        let data_bags_root = dir.join("data_bags");
        for bag_dir in sorted_entries_within(dir, &data_bags_root)? {
            reject_symlink(&bag_dir)?;
            if !bag_dir.is_dir() {
                continue;
            }
            let Some(bag_name) = dir_name(&bag_dir) else {
                continue;
            };
            let bag_id = stable_id(&format!("data_bag:id:{org_name}:{bag_name}"));
            let bag_authz = stable_id(&format!("data_bag:authz:{org_name}:{bag_name}"));
            self.ensure_authz(&bag_authz, "object", Some(&org_id));
            self.object_authz.insert(
                (org_name.to_string(), "data_bag".into(), bag_name.clone()),
                bag_authz.clone(),
            );
            self.data.data_bags.push(NamedObjectRow {
                id: bag_id,
                authz_id: bag_authz,
                org_id: org_id.clone(),
                name: bag_name.clone(),
                serialized_object: Some(
                    serde_json::to_string(&json!({
                        "name": bag_name,
                        "json_class": "Chef::DataBag",
                        "chef_type": "data_bag"
                    }))
                    .map_err(MigrateError::InvalidJson)?,
                ),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.data_bags += 1;

            for path in sorted_entries_within(&data_bags_root, &bag_dir)? {
                if !path.is_file() {
                    continue;
                }
                let (value, body) = read_object(&path)?;
                let Some(item_name) = string_field(&value, "id").or_else(|| file_stem(&path))
                else {
                    self.warn(format!(
                        "skipping data bag item {} without an id",
                        path.display()
                    ));
                    continue;
                };
                let id = stable_id(&format!(
                    "data_bag_item:id:{org_name}:{bag_name}:{item_name}"
                ));
                if !self.seen_objects.insert((
                    org_name.to_string(),
                    "data_bag_item".into(),
                    format!("{bag_name}/{item_name}"),
                )) {
                    continue;
                }
                self.data.data_bag_items.push(DataBagItemRow {
                    id,
                    org_id: org_id.clone(),
                    data_bag_name: bag_name.clone(),
                    item_name,
                    serialized_object: Some(body),
                    last_updated_by: String::new(),
                    created_at: self.generated_at.clone(),
                    updated_at: self.generated_at.clone(),
                });
                self.summary.data_bag_items += 1;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Cookbooks
    // ------------------------------------------------------------------

    fn read_cookbooks(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        for cookbook_dir in sorted_entries_within(dir, &dir.join("cookbooks"))? {
            reject_symlink(&cookbook_dir)?;
            if !cookbook_dir.is_dir() {
                continue;
            }
            self.read_cookbook(org_name, &cookbook_dir)?;
        }
        Ok(())
    }

    fn read_cookbook(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        let dir_name = dir_name(dir).unwrap_or_default();
        let (dir_cookbook, dir_version) = split_cookbook_dir(&dir_name);
        let metadata_path_json = dir.join("metadata.json");
        let metadata_path_rb = dir.join("metadata.rb");
        let metadata_source = if metadata_path_json.is_file() {
            let value = read_json(&metadata_path_json)?;
            value
                .as_object()
                .ok_or_else(|| MigrateError::InvalidBackup {
                    path: metadata_path_json.clone(),
                    message: "metadata.json is not an object".into(),
                })?;
            value
        } else if metadata_path_rb.is_file() {
            parse_metadata_rb(&metadata_path_rb)?
        } else {
            self.warn(format!(
                "cookbook {} has no metadata.rb or metadata.json; using the directory name",
                dir.display()
            ));
            Value::Object(Map::new())
        };

        let Some(name) = string_field(&metadata_source, "name")
            .or_else(|| dir_cookbook.clone())
            .or_else(|| Some(dir_name.clone()))
        else {
            self.warn(format!(
                "skipping cookbook {} without a name",
                dir.display()
            ));
            return Ok(());
        };
        let Some(version) =
            string_field(&metadata_source, "version").or_else(|| dir_version.clone())
        else {
            self.warn(format!(
                "skipping cookbook {name:?} at {} without a version",
                dir.display()
            ));
            return Ok(());
        };
        if parse_version(&version).is_err() {
            self.warn(format!(
                "skipping cookbook {name:?} at {} with invalid version {version:?}",
                dir.display()
            ));
            return Ok(());
        }
        if dir_cookbook
            .as_deref()
            .is_some_and(|dir_name| dir_name != name)
        {
            self.warn(format!(
                "cookbook directory {dir_name:?} contains metadata name {name:?}; using the metadata name"
            ));
        }
        if dir_version
            .as_deref()
            .is_some_and(|dir_version| dir_version != version)
        {
            self.warn(format!(
                "cookbook directory {dir_name:?} contains metadata version {version:?}; using the metadata version"
            ));
        }

        let mut metadata = metadata_source.as_object().cloned().unwrap_or_default();
        metadata.insert("name".into(), json!(name));
        metadata.insert("version".into(), json!(version));
        metadata.entry("attributes").or_insert_with(|| json!({}));
        metadata.entry("dependencies").or_insert_with(|| json!({}));
        metadata
            .entry("long_description")
            .or_insert_with(|| json!(""));

        let frozen = read_frozen_status(dir)?;
        let files = collect_cookbook_files(dir)?;

        let cookbook_id = *self
            .cookbook_ids
            .entry((org_name.to_string(), name.clone()))
            .or_insert_with(|| synthetic_i64(&format!("cookbook:id:{org_name}:{name}")));
        let cookbook_authz = self
            .cookbook_authz
            .entry((org_name.to_string(), name.clone()))
            .or_insert_with(|| stable_id(&format!("cookbook:authz:{org_name}:{name}")))
            .clone();
        self.ensure_authz(&cookbook_authz, "object", Some(&org_id));
        self.object_authz.insert(
            (org_name.to_string(), "cookbook".into(), name.clone()),
            cookbook_authz.clone(),
        );
        if self
            .seen_cookbooks
            .insert((org_name.to_string(), name.clone()))
        {
            self.data.cookbooks.push(CookbookRow {
                id: cookbook_id,
                org_id: org_id.clone(),
                name: name.clone(),
                authz_id: cookbook_authz,
            });
            self.summary.cookbooks += 1;
        }
        if !self
            .seen_cookbook_versions
            .insert((org_name.to_string(), format!("{name}:{version}")))
        {
            self.warn(format!(
                "duplicate cookbook version {name} {version}; keeping the first"
            ));
            return Ok(());
        }

        let mut segments: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
        let mut checksums = Vec::new();
        for (relative, bytes) in &files {
            let checksum = md5_hex(bytes);
            checksums.push(checksum.clone());
            if self
                .seen_checksums
                .insert((org_id.clone(), checksum.clone()))
            {
                self.data.checksums.push(ChecksumRow {
                    org_id: org_id.clone(),
                    checksum: checksum.clone(),
                    created_at: self.generated_at.clone(),
                });
                self.summary.checksums += 1;
            }
            self.blobs
                .entry((org_id.clone(), checksum.clone()))
                .or_insert_with(|| bytes.clone());
            let segment = cookbook_segment(relative);
            let filename = relative.rsplit('/').next().unwrap_or(relative);
            segments.entry(segment).or_default().push(json!({
                "name": filename,
                "path": relative,
                "checksum": checksum,
                "specificity": cookbook_specificity(relative),
            }));
            self.summary.cookbook_files += 1;
        }
        checksums.sort();
        checksums.dedup();

        let (major, minor, patch) = parse_version(&version)?;
        let mut manifest = Map::new();
        manifest.insert("name".into(), json!(format!("{name}-{version}")));
        manifest.insert("cookbook_name".into(), json!(name));
        manifest.insert("version".into(), json!(version));
        manifest.insert("json_class".into(), json!("Chef::CookbookVersion"));
        manifest.insert("chef_type".into(), json!("cookbook_version"));
        manifest.insert("frozen?".into(), json!(frozen));
        manifest.insert("metadata".into(), Value::Object(metadata.clone()));
        for segment in SEGMENTS {
            manifest.insert(
                segment.into(),
                Value::Array(segments.remove(segment).unwrap_or_default()),
            );
        }
        let manifest_text =
            serde_json::to_string(&Value::Object(manifest)).map_err(MigrateError::InvalidJson)?;

        let metadata_text = serde_json::to_string(&Value::Object(metadata.clone()))
            .map_err(MigrateError::InvalidJson)?;
        let meta_attributes = metadata
            .get("attributes")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::to_string(value).ok());
        let meta_deps = metadata
            .get("dependencies")
            .filter(|value| !value.is_null())
            .and_then(|value| serde_json::to_string(value).ok());
        let meta_long_desc = match metadata.get("long_description") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(value) if !value.is_null() => serde_json::to_string(value).ok(),
            _ => None,
        };
        self.data.cookbook_versions.push(CookbookVersionRow {
            id: stable_id(&format!("cookbook_version:id:{org_name}:{name}:{version}")),
            cookbook_id,
            org_id: org_id.clone(),
            name,
            major,
            minor,
            patch,
            frozen,
            meta_attributes,
            meta_deps,
            meta_long_desc,
            metadata: Some(metadata_text),
            serialized_object: Some(manifest_text),
            last_updated_by: String::new(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        });
        let version_id = self
            .data
            .cookbook_versions
            .last()
            .map(|version| version.id.clone())
            .unwrap_or_default();
        for checksum in checksums {
            self.data
                .cookbook_version_checksums
                .push(CookbookVersionChecksumRow {
                    cookbook_version_id: version_id.clone(),
                    org_id: org_id.clone(),
                    checksum,
                });
        }
        self.summary.cookbook_versions += 1;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Membership and invitations
    // ------------------------------------------------------------------

    fn read_members(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let path = dir.join("members.json");
        if !path.is_file() {
            return Ok(());
        }
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        let rows = json_rows_or_named(read_json(&path)?, "users");
        for row in rows {
            let Some(username) = member_username(&row) else {
                self.warn(format!(
                    "members.json for {org_name:?} has an entry without a username; skipping"
                ));
                continue;
            };
            let Some(user) = self
                .data
                .users
                .iter()
                .find(|user| user.username == username)
            else {
                self.warn(format!(
                    "members.json for {org_name:?} references unknown user {username:?}; skipping"
                ));
                continue;
            };
            if self
                .data
                .org_user_associations
                .iter()
                .any(|row| row.org_id == org_id && row.user_id == user.id)
            {
                continue;
            }
            self.data.org_user_associations.push(OrgUserAssociationRow {
                org_id: org_id.clone(),
                user_id: user.id.clone(),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.org_user_associations += 1;
        }
        Ok(())
    }

    fn read_invitations(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let path = dir.join("invitations.json");
        if !path.is_file() {
            return Ok(());
        }
        let Some(org_id) = self.org_ids.get(org_name).cloned() else {
            return Ok(());
        };
        let rows = json_rows_or_named(read_json(&path)?, "invitations");
        for row in rows {
            let Some(username) = member_username(&row) else {
                self.warn(format!(
                    "invitations.json for {org_name:?} has an entry without a username; skipping"
                ));
                continue;
            };
            let Some(user) = self
                .data
                .users
                .iter()
                .find(|user| user.username == username)
            else {
                self.warn(format!(
                    "invitations.json for {org_name:?} references unknown user {username:?}; skipping"
                ));
                continue;
            };
            let id = usable_id(string_field(&row, "id"))
                .unwrap_or_else(|| stable_id(&format!("invitation:id:{org_name}:{username}")));
            if self.data.org_user_invites.iter().any(|existing| {
                existing.id == id || (existing.org_id == org_id && existing.user_id == user.id)
            }) {
                continue;
            }
            self.data.org_user_invites.push(OrgUserInviteRow {
                id,
                org_id: org_id.clone(),
                user_id: user.id.clone(),
                last_updated_by: String::new(),
                created_at: self.generated_at.clone(),
                updated_at: self.generated_at.clone(),
            });
            self.summary.org_user_invitations += 1;
        }
        Ok(())
    }

    /// Rebuild the global-group scaffolding that `knife ec backup` does not
    /// include in its repository.
    ///
    /// The upstream default policy puts every administrator in the global
    /// `server-admins` group and makes each organization's `admins` and `users`
    /// groups children of its global `<org>_read_access_group`. The backup
    /// contains neither global group nor global membership rows, so recreate
    /// that standard graph. User and organization ACLs supply the rest.
    fn ensure_global_defaults(&mut self) {
        let user_ids: Vec<String> = self
            .data
            .users
            .iter()
            .map(|user| user.authz_id.clone())
            .collect();
        if !user_ids.is_empty() {
            if let Some(users_group) = self.resolve_global_group("users") {
                for authz_id in &user_ids {
                    self.add_group_actor(&users_group, authz_id);
                }
            }
        }
        let admin_ids: Vec<String> = self
            .data
            .users
            .iter()
            .filter(|user| user.admin)
            .map(|user| user.authz_id.clone())
            .collect();
        if !admin_ids.is_empty() {
            if let Some(server_admins) = self.resolve_global_group("server-admins") {
                for authz_id in admin_ids {
                    self.add_group_actor(&server_admins, &authz_id);
                }
            }
        }

        let orgs: Vec<String> = self.data.orgs.iter().map(|org| org.name.clone()).collect();
        for org_name in orgs {
            let Some(read_access) =
                self.resolve_global_group(&format!("{org_name}_read_access_group"))
            else {
                continue;
            };
            for group_name in ["admins", "users"] {
                let child = self
                    .group_authz
                    .get(&(org_name.clone(), group_name.to_string()))
                    .cloned();
                if let Some(child) = child {
                    if self
                        .seen_group_groups
                        .insert((read_access.clone(), child.clone()))
                    {
                        self.data.authz_group_groups.push(AuthzGroupGroupRow {
                            parent_group_authz_id: read_access.clone(),
                            child_group_authz_id: child,
                        });
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // ACLs
    // ------------------------------------------------------------------

    fn collect_org_acls(&mut self, org_name: &str, dir: &Path) -> Result<(), MigrateError> {
        let acl_root = dir.join("acls");
        for entry in sorted_entries_within(dir, &acl_root)? {
            if entry.is_file() && entry.extension().and_then(|ext| ext.to_str()) == Some("json") {
                let Some(kind) = file_stem(&entry) else {
                    continue;
                };
                self.pending_acls.push(PendingAcl {
                    path: entry,
                    org_name: Some(org_name.to_string()),
                    kind,
                    name: org_name.to_string(),
                });
                continue;
            }
            if !entry.is_dir() {
                continue;
            }
            let Some(kind) = dir_name(&entry) else {
                continue;
            };
            for path in json_files_within(&acl_root, &entry)? {
                let Some(name) = file_stem(&path) else {
                    continue;
                };
                self.pending_acls.push(PendingAcl {
                    path,
                    org_name: Some(org_name.to_string()),
                    kind: kind.clone(),
                    name,
                });
            }
        }
        Ok(())
    }

    fn apply_acls(&mut self) -> Result<(), MigrateError> {
        let pending = std::mem::take(&mut self.pending_acls);
        for acl in pending {
            let value = read_json(&acl.path)?;
            let Some((target_authz, is_user_acl)) = self.resolve_acl_target(&acl) else {
                self.warn(format!(
                    "ACL {} targets an object that was not imported; skipping",
                    acl.path.display()
                ));
                continue;
            };
            self.apply_acl_value(
                &value,
                &target_authz,
                acl.org_name.as_deref(),
                is_user_acl,
                &acl.path,
            )?;
            if is_user_acl {
                self.summary.user_acls += 1;
            } else {
                self.summary.acls += 1;
            }
        }
        Ok(())
    }

    fn resolve_acl_target(&self, acl: &PendingAcl) -> Option<(String, bool)> {
        let kind = normalize_acl_kind(&acl.kind)?;
        match kind {
            "users" => self
                .user_authz
                .get(&acl.name)
                .cloned()
                .map(|authz| (authz, true)),
            "organizations" => {
                let org_name = acl.org_name.as_deref().unwrap_or(&acl.name);
                self.org_authz
                    .get(org_name)
                    .cloned()
                    .map(|authz| (authz, false))
            }
            "clients" => self
                .client_authz
                .get(&(acl.org_name.clone()?, acl.name.clone()))
                .cloned()
                .map(|authz| (authz, false)),
            "containers" => self
                .container_authz
                .get(&(acl.org_name.clone()?, acl.name.clone()))
                .cloned()
                .map(|authz| (authz, false)),
            "groups" => self
                .group_authz
                .get(&(acl.org_name.clone()?, acl.name.clone()))
                .cloned()
                .map(|authz| (authz, false)),
            "nodes" | "roles" | "environments" | "data_bags" | "cookbooks" => self
                .object_authz
                .get(&(
                    acl.org_name.clone()?,
                    kind.trim_end_matches('s').to_string(),
                    acl.name.clone(),
                ))
                .cloned()
                .map(|authz| (authz, false)),
            _ => None,
        }
    }

    fn apply_acl_value(
        &mut self,
        value: &Value,
        target_authz: &str,
        org_name: Option<&str>,
        user_acl: bool,
        path: &Path,
    ) -> Result<(), MigrateError> {
        let Some(permissions) = value.as_object() else {
            return Err(MigrateError::InvalidBackup {
                path: path.to_path_buf(),
                message: "ACL body is not an object".into(),
            });
        };
        self.acl_targets.insert(target_authz.to_string());
        for permission in PERMISSIONS {
            let Some(entries) = permissions.get(permission) else {
                continue;
            };
            let Some(entries) = entries.as_object() else {
                return Err(MigrateError::InvalidBackup {
                    path: path.to_path_buf(),
                    message: format!("permission {permission:?} is not an object"),
                });
            };
            let actors = acl_string_array(entries, "actors", path)?;
            let users = acl_string_array(entries, "users", path)?;
            let clients = acl_string_array(entries, "clients", path)?;
            let groups = acl_string_array(entries, "groups", path)?;

            for actor in actors.iter().chain(users.iter()) {
                if let Some(actor_authz) = if user_acl || org_name.is_none() {
                    self.user_authz.get(actor).cloned()
                } else {
                    self.resolve_actor(org_name.unwrap_or_default(), actor)
                } {
                    self.add_acl_actor(target_authz, permission, &actor_authz);
                } else {
                    self.warn(format!(
                        "ACL {} references unknown actor {actor:?}; skipping",
                        path.display()
                    ));
                }
            }
            if let Some(org_name) = org_name {
                for client in clients {
                    if let Some(actor_authz) = self
                        .client_authz
                        .get(&(org_name.to_string(), client.clone()))
                        .cloned()
                    {
                        self.add_acl_actor(target_authz, permission, &actor_authz);
                    } else {
                        self.warn(format!(
                            "ACL {} references unknown client {client:?}; skipping",
                            path.display()
                        ));
                    }
                }
            } else if !clients.is_empty() {
                self.warn(format!(
                    "user ACL {} contains client actors; skipping them",
                    path.display()
                ));
            }

            for group in groups {
                let group = group.trim();
                if group.is_empty()
                    || group
                        .strip_prefix("::")
                        .is_some_and(|name| name.trim().is_empty())
                {
                    continue;
                }
                if let Some(group_authz) = if user_acl {
                    self.resolve_global_group(group)
                } else {
                    self.resolve_group_authz(org_name, group)
                } {
                    self.add_acl_group(target_authz, permission, &group_authz);
                } else {
                    self.warn(format!(
                        "ACL {} references unknown group {group:?}; skipping",
                        path.display()
                    ));
                }
            }
        }
        Ok(())
    }

    fn add_acl_actor(&mut self, target_authz: &str, permission: &str, actor_authz: &str) {
        if self.seen_acl_actor.insert((
            target_authz.to_string(),
            permission.to_string(),
            actor_authz.to_string(),
        )) {
            self.data.authz_acl_actor.push(AuthzAclActorRow {
                target_authz_id: target_authz.to_string(),
                permission: permission.to_string(),
                actor_authz_id: actor_authz.to_string(),
            });
        }
    }

    fn add_acl_group(&mut self, target_authz: &str, permission: &str, group_authz: &str) {
        if self.seen_acl_group.insert((
            target_authz.to_string(),
            permission.to_string(),
            group_authz.to_string(),
        )) {
            self.data.authz_acl_group.push(AuthzAclGroupRow {
                target_authz_id: target_authz.to_string(),
                permission: permission.to_string(),
                group_authz_id: group_authz.to_string(),
            });
        }
    }

    fn resolve_actor(&self, org_name: &str, name: &str) -> Option<String> {
        self.client_authz
            .get(&(org_name.to_string(), name.to_string()))
            .cloned()
            .or_else(|| self.user_authz.get(name).cloned())
    }

    fn resolve_group_authz(&mut self, org_name: Option<&str>, name: &str) -> Option<String> {
        if let Some(global) = name.strip_prefix("::") {
            return self.resolve_global_group(global);
        }
        if let Some((scope, group)) = name.split_once("::") {
            return self
                .group_authz
                .get(&(scope.to_string(), group.to_string()))
                .cloned();
        }
        if let Some(org_name) = org_name {
            return self
                .group_authz
                .get(&(org_name.to_string(), name.to_string()))
                .cloned();
        }
        self.resolve_global_group(name)
    }

    fn resolve_global_group(&mut self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        // Upstream's global admins group maps to Microwave's server-admins
        // flag/group. Preserve every other non-empty global group by name.
        let normalized = if name == "admins" {
            "server-admins"
        } else {
            name
        };
        if let Some(authz_id) = self.global_group_authz.get(normalized) {
            return Some(authz_id.clone());
        }
        let id = stable_id(&format!("global_group:id:{normalized}"));
        let authz_id = stable_id(&format!("global_group:authz:{normalized}"));
        self.ensure_authz(&authz_id, "group", None);
        self.data.global_groups.push(GlobalGroupRow {
            id,
            authz_id: authz_id.clone(),
            name: normalized.to_string(),
            created_at: self.generated_at.clone(),
            updated_at: self.generated_at.clone(),
        });
        self.global_group_authz
            .insert(normalized.to_string(), authz_id.clone());
        Some(authz_id)
    }
}

// ----------------------------------------------------------------------
// File and JSON helpers
// ----------------------------------------------------------------------

fn read_json(path: &Path) -> Result<Value, MigrateError> {
    let text = read_text_file(path)?;
    serde_json::from_str(&text).map_err(|error| MigrateError::InvalidBackup {
        path: path.to_path_buf(),
        message: format!("invalid JSON: {error}"),
    })
}

fn read_object(path: &Path) -> Result<(Value, String), MigrateError> {
    let text = read_text_file(path)?;
    let value = serde_json::from_str(&text).map_err(|error| MigrateError::InvalidBackup {
        path: path.to_path_buf(),
        message: format!("invalid JSON: {error}"),
    })?;
    Ok((value, text))
}

fn reject_symlink(path: &Path) -> Result<(), MigrateError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: "backup contains a symlink; refusing to follow it".into(),
        });
    }
    Ok(())
}

fn read_text_file(path: &Path) -> Result<String, MigrateError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: "backup file is a symlink; refusing to follow it".into(),
        });
    }
    if !metadata.is_file() {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: "expected a regular file".into(),
        });
    }
    std::fs::read_to_string(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MigrateError::InvalidBackup {
                path: dir.to_path_buf(),
                message: "backup directory is a symlink; refusing to follow it".into(),
            })
        }
        Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(MigrateError::Read {
                path: dir.to_path_buf(),
                source,
            })
        }
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|source| MigrateError::Read {
            path: dir.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| MigrateError::Read {
                    path: dir.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    for path in &entries {
        reject_symlink(path)?;
    }
    Ok(entries)
}

fn json_files(dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    Ok(sorted_entries(dir)?
        .into_iter()
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("json")
        })
        .collect())
}

fn ensure_existing_within(root: &Path, path: &Path) -> Result<(), MigrateError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(MigrateError::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: "backup path is a symlink; refusing to follow it".into(),
        });
    }
    let root = std::fs::canonicalize(root).map_err(|source| MigrateError::Read {
        path: root.to_path_buf(),
        source,
    })?;
    let resolved = std::fs::canonicalize(path).map_err(|source| MigrateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !resolved.starts_with(&root) {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: "backup path resolves outside its repository root".into(),
        });
    }
    Ok(())
}

fn sorted_entries_within(root: &Path, dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    ensure_existing_within(root, dir)?;
    sorted_entries(dir)
}

fn json_files_within(root: &Path, dir: &Path) -> Result<Vec<PathBuf>, MigrateError> {
    ensure_existing_within(root, dir)?;
    json_files(dir)
}

fn json_rows(value: Value) -> Vec<Value> {
    match value {
        Value::Array(rows) => rows,
        Value::Object(mut map) => {
            for key in ["rows", "users", "keys", "items", "results"] {
                if let Some(Value::Array(rows)) = map.remove(key) {
                    return rows;
                }
            }
            if map.contains_key("username") || map.contains_key("name") {
                vec![Value::Object(map)]
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

fn json_rows_or_named(value: Value, array_key: &str) -> Vec<Value> {
    match value {
        Value::Array(rows) => rows,
        Value::Object(mut map) => match map.remove(array_key) {
            Some(Value::Array(rows)) => rows,
            Some(Value::Object(row)) => vec![Value::Object(row)],
            _ if !map.is_empty() => map
                .keys()
                .map(|username| json!({ "username": username }))
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn dir_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    string_field(value, key).filter(|text| !text.is_empty())
}

fn bool_field(value: &Value, key: &str) -> Option<bool> {
    let value = value.get(key)?;
    value
        .as_bool()
        .or_else(|| value.as_i64().map(|number| number != 0))
        .or_else(|| {
            value
                .as_str()
                .and_then(|text| match text.to_ascii_lowercase().as_str() {
                    "true" | "t" | "1" => Some(true),
                    "false" | "f" | "0" => Some(false),
                    _ => None,
                })
        })
}

fn integer_field(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_str()?.parse().ok()))
}

fn email_field(value: &Value) -> Option<String> {
    non_empty_string(value, "email")
}

fn timestamp_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| match value {
        Value::String(text) => normalize_time(text),
        Value::Number(number) => number
            .as_i64()
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
            .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true)),
        _ => None,
    })
}

fn normalize_expires_at(value: Option<&Value>) -> Result<String, String> {
    match value {
        None | Some(Value::Null) => Ok("infinity".into()),
        Some(Value::String(text)) if text.trim().is_empty() => Err("empty value".into()),
        Some(Value::String(text)) if text.trim().eq_ignore_ascii_case("infinity") => {
            Ok("infinity".into())
        }
        Some(Value::String(text)) => {
            normalize_time(text).ok_or_else(|| format!("{text:?} is not a recognized timestamp"))
        }
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
            .map(|time| time.to_rfc3339_opts(SecondsFormat::Secs, true))
            .ok_or_else(|| "numeric timestamp is out of range".to_string()),
        Some(other) => Err(format!("{other} is not a timestamp")),
    }
}

fn normalize_time(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if value.eq_ignore_ascii_case("infinity") {
        return Some("infinity".into());
    }
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Some(time.to_rfc3339_opts(SecondsFormat::Secs, true));
    }
    for format in ["%Y-%m-%d %H:%M:%S%.f %z", "%Y-%m-%d %H:%M:%S %z"] {
        if let Ok(time) = DateTime::parse_from_str(value, format) {
            return Some(
                time.with_timezone(&Utc)
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            );
        }
    }
    for format in [
        "%Y-%m-%d %H:%M:%S%.f UTC",
        "%Y-%m-%d %H:%M:%S UTC",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(time) = NaiveDateTime::parse_from_str(value, format) {
            return Some(
                Utc.from_utc_datetime(&time)
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            );
        }
    }
    None
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn acl_string_array(
    entries: &Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<Vec<String>, MigrateError> {
    let Some(value) = entries.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(MigrateError::InvalidBackup {
            path: path.to_path_buf(),
            message: format!("ACL field {key:?} is not an array"),
        });
    };
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| MigrateError::InvalidBackup {
                    path: path.to_path_buf(),
                    message: format!("ACL field {key:?} contains a non-string value"),
                })
        })
        .collect()
}

fn member_username(value: &Value) -> Option<String> {
    string_field(value, "username")
        .or_else(|| {
            value
                .get("user")
                .and_then(|user| string_field(user, "username"))
        })
        .or_else(|| {
            value
                .get("organization_user")
                .and_then(|user| string_field(user, "username"))
        })
}

fn normalize_acl_kind(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "users" | "user" => "users",
        "organizations" | "organization" => "organizations",
        "clients" | "client" => "clients",
        "containers" | "container" => "containers",
        "groups" | "group" => "groups",
        "nodes" | "node" => "nodes",
        "roles" | "role" => "roles",
        "environments" | "environment" => "environments",
        "data_bags" | "data_bag" | "data" => "data_bags",
        "cookbooks" | "cookbook" => "cookbooks",
        _ => return None,
    })
}

fn usable_id(value: Option<String>) -> Option<String> {
    value.filter(|value| {
        !value.is_empty()
            && !value.contains('/')
            && !value.contains('\\')
            && value != "."
            && value != ".."
    })
}

fn stable_id(scope: &str) -> String {
    let digest = Sha256::digest(format!("{IMPORTER_SCOPE}/{scope}").as_bytes());
    hex::encode(&digest[..16])
}

fn synthetic_i64(scope: &str) -> i64 {
    let digest = Sha256::digest(format!("{IMPORTER_SCOPE}/{scope}").as_bytes());
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    (u64::from_be_bytes(bytes) & 0x7fff_ffff_ffff_ffff) as i64
}

fn split_cookbook_dir(name: &str) -> (Option<String>, Option<String>) {
    if let Some((cookbook, version)) = name.rsplit_once('-') {
        if parse_version(version).is_ok() {
            return (Some(cookbook.to_string()), Some(version.to_string()));
        }
    }
    (Some(name.to_string()), None)
}

fn cookbook_segment(path: &str) -> &'static str {
    let first = path.split('/').next().unwrap_or_default();
    SEGMENTS
        .iter()
        .copied()
        .find(|segment| *segment == first)
        .unwrap_or("root_files")
}

fn cookbook_specificity(path: &str) -> String {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some("files" | "templates"), Some(specificity), Some(_)) => specificity.to_string(),
        _ => "default".to_string(),
    }
}

fn read_frozen_status(dir: &Path) -> Result<bool, MigrateError> {
    let path = dir.join("status.json");
    if !path.is_file() {
        return Ok(false);
    }
    let value = read_json(&path)?;
    Ok(bool_field(&value, "frozen")
        .or_else(|| bool_field(&value, "frozen?"))
        .unwrap_or(false))
}

fn collect_cookbook_files(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, MigrateError> {
    fn walk(
        root: &Path,
        dir: &Path,
        output: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), MigrateError> {
        ensure_existing_within(root, dir)?;
        for path in sorted_entries(dir)? {
            let metadata =
                std::fs::symlink_metadata(&path).map_err(|source| MigrateError::Read {
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(MigrateError::InvalidBackup {
                    path: path.clone(),
                    message: "cookbook contains a symlink; refusing to copy it".into(),
                });
            }
            if metadata.is_dir() {
                walk(root, &path, output)?;
                continue;
            }
            if !metadata.is_file() {
                return Err(MigrateError::InvalidBackup {
                    path: path.clone(),
                    message: "cookbook contains a non-regular file".into(),
                });
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| MigrateError::InvalidBackup {
                    path: path.clone(),
                    message: "cookbook path escaped its directory".into(),
                })?
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if relative == "status.json" {
                continue;
            }
            let data = std::fs::read(&path).map_err(|source| MigrateError::Read {
                path: path.clone(),
                source,
            })?;
            output.push((relative, data));
        }
        Ok(())
    }

    let mut output = Vec::new();
    walk(dir, dir, &mut output)?;
    output.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(output)
}

// ----------------------------------------------------------------------
// metadata.rb parsing
// ----------------------------------------------------------------------

/// Parse the common subset of a cookbook `metadata.rb` DSL.
///
/// This is deliberately not a Ruby evaluator. It supports scalar calls
/// (`name`, `version`, `description`, ...), `depends`/`supports` and the
/// related constraint maps, and the common `long_description IO.read(...)`
/// form. Arbitrary Ruby, interpolation, conditionals, and custom methods are
/// left for a future pass; a missing value is simply omitted.
fn parse_metadata_rb(path: &Path) -> Result<Value, MigrateError> {
    let text = read_text_file(path)?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let base = std::fs::canonicalize(base).map_err(|source| MigrateError::Read {
        path: base.to_path_buf(),
        source,
    })?;
    let mut metadata = Map::new();
    let mut dependencies = Map::new();
    let mut platforms = Map::new();
    let mut recommendations = Map::new();
    let mut suggestions = Map::new();
    let mut conflicting = Map::new();
    let mut replacing = Map::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("if ") || line == "end" {
            continue;
        }
        let Some((method, args)) = split_ruby_call(line) else {
            continue;
        };
        match method {
            "name" | "maintainer" | "maintainer_email" | "license" | "description"
            | "source_url" | "issues_url" | "chef_version" | "version" => {
                if let Some(value) = first_ruby_string(args) {
                    metadata.insert(method.into(), json!(value));
                }
            }
            "long_description" => {
                if let Some(value) = first_ruby_string(args) {
                    metadata.insert("long_description".into(), json!(value));
                } else if args.contains("README") {
                    if let Some(filename) = first_ruby_string_containing(args, "README") {
                        if let Some(value) = read_cookbook_readme(&base, &filename, path)? {
                            metadata.insert("long_description".into(), json!(value));
                        }
                    }
                }
            }
            "depends" | "supports" | "recommends" | "suggests" | "conflicts" | "replaces" => {
                let values = ruby_string_args(args);
                let Some(name) = values.first() else {
                    continue;
                };
                let constraint = values
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| ">= 0.0.0".to_string());
                let map = match method {
                    "depends" => &mut dependencies,
                    "supports" => &mut platforms,
                    "recommends" => &mut recommendations,
                    "suggests" => &mut suggestions,
                    "conflicts" => &mut conflicting,
                    "replaces" => &mut replacing,
                    _ => unreachable!(),
                };
                map.insert(name.clone(), json!(constraint));
            }
            _ => {}
        }
    }

    if !dependencies.is_empty() {
        metadata.insert("dependencies".into(), Value::Object(dependencies));
    }
    if !platforms.is_empty() {
        metadata.insert("platforms".into(), Value::Object(platforms));
    }
    for (key, map) in [
        ("recommendations", recommendations),
        ("suggestions", suggestions),
        ("conflicting", conflicting),
        ("replacing", replacing),
    ] {
        if !map.is_empty() {
            metadata.insert(key.into(), Value::Object(map));
        }
    }
    Ok(Value::Object(metadata))
}

fn read_cookbook_readme(
    cookbook_root: &Path,
    filename: &str,
    metadata_path: &Path,
) -> Result<Option<String>, MigrateError> {
    let mut name = None;
    for component in Path::new(filename).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) if name.is_none() => name = Some(value),
            _ => {
                return Err(MigrateError::InvalidBackup {
                    path: metadata_path.to_path_buf(),
                    message: format!(
                        "long_description filename {filename:?} is not a single normal file"
                    ),
                })
            }
        }
    }
    let Some(name) = name else {
        return Err(MigrateError::InvalidBackup {
            path: metadata_path.to_path_buf(),
            message: format!("long_description filename {filename:?} is not a single normal file"),
        });
    };
    let candidate = cookbook_root.join(name);
    let metadata = match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MigrateError::Read {
                path: candidate,
                source,
            })
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(MigrateError::InvalidBackup {
            path: metadata_path.to_path_buf(),
            message: format!("long_description file {filename:?} is a symlink"),
        });
    }
    if !metadata.is_file() {
        return Err(MigrateError::InvalidBackup {
            path: metadata_path.to_path_buf(),
            message: format!("long_description file {filename:?} is not a regular file"),
        });
    }
    let resolved = std::fs::canonicalize(&candidate).map_err(|source| MigrateError::Read {
        path: candidate.clone(),
        source,
    })?;
    if resolved.parent() != Some(cookbook_root) {
        return Err(MigrateError::InvalidBackup {
            path: metadata_path.to_path_buf(),
            message: format!("long_description file {filename:?} resolves outside the cookbook"),
        });
    }
    std::fs::read_to_string(&resolved)
        .map(Some)
        .map_err(|source| MigrateError::Read {
            path: resolved,
            source,
        })
}

fn split_ruby_call(line: &str) -> Option<(&str, &str)> {
    let end = line
        .find(|character: char| character.is_whitespace() || character == '(')
        .unwrap_or(line.len());
    let method = &line[..end];
    if method.is_empty()
        || !method
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let args = line[end..].trim();
    Some((method, args))
}

fn first_ruby_string(args: &str) -> Option<String> {
    ruby_string_args(args).into_iter().next()
}

fn first_ruby_string_containing(args: &str, needle: &str) -> Option<String> {
    let mut rest = args;
    while !rest.is_empty() {
        let starts_string = rest.starts_with('\'')
            || rest.starts_with('"')
            || rest.starts_with("%q")
            || rest.starts_with("%Q");
        if starts_string {
            if let Some((value, remainder)) = parse_ruby_string(rest) {
                if value.contains(needle) {
                    return Some(value);
                }
                rest = remainder;
                continue;
            }
        }
        let character = rest.chars().next()?;
        rest = &rest[character.len_utf8()..];
    }
    None
}

fn ruby_string_args(args: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = args.trim();
    loop {
        rest = rest.trim_start_matches(|character: char| {
            character.is_whitespace() || character == ',' || character == '('
        });
        let Some((value, remainder)) = parse_ruby_string(rest) else {
            break;
        };
        values.push(value);
        rest = remainder;
    }
    values
}

fn parse_ruby_string(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    let (quote, rest) = match input.chars().next()? {
        '\'' => ('\'', &input[1..]),
        '"' => ('"', &input[1..]),
        '%' => {
            let mut chars = input.chars();
            chars.next()?;
            let kind = chars.next()?;
            if kind != 'q' && kind != 'Q' {
                return None;
            }
            let delimiter = chars.next()?;
            let close = match delimiter {
                '(' => ')',
                '[' => ']',
                '{' => '}',
                '<' => '>',
                other => other,
            };
            let body_start = 2 + delimiter.len_utf8();
            let rest = &input[body_start..];
            let end = rest.find(close)?;
            return Some((rest[..end].to_string(), &rest[end + close.len_utf8()..]));
        }
        _ => return None,
    };
    let mut value = String::new();
    let mut escaped = false;
    for (index, character) in rest.char_indices() {
        if escaped {
            value.push(match character {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                other => other,
            });
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == quote {
            return Some((value, &rest[index + character.len_utf8()..]));
        }
        value.push(character);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_metadata_rb, parse_ruby_string};
    use serde_json::Value;

    #[test]
    fn parses_quoted_and_percent_literals() {
        assert_eq!(
            parse_ruby_string("'it\\'s here'").unwrap().0,
            "it's here".to_string()
        );
        assert_eq!(
            parse_ruby_string("%q{long description}").unwrap().0,
            "long description".to_string()
        );
        assert_eq!(
            parse_ruby_string("%Q(interpolated)").unwrap().0,
            "interpolated".to_string()
        );
    }

    #[test]
    fn parses_common_metadata_rb_and_reads_the_readme() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("README.md"), "readme body").unwrap();
        let metadata = directory.path().join("metadata.rb");
        std::fs::write(
            &metadata,
            r#"name 'demo'
version '2.3.4'
description %q{A demo cookbook}
long_description IO.read(File.join(File.dirname(__FILE__), './README.md'))
depends 'apt', '~> 7.0'
supports 'ubuntu', '>= 20.04'
"#,
        )
        .unwrap();
        let value = parse_metadata_rb(&metadata).unwrap();
        assert_eq!(value["name"], "demo");
        assert_eq!(value["version"], "2.3.4");
        assert_eq!(value["description"], "A demo cookbook");
        assert_eq!(value["long_description"], "readme body");
        assert_eq!(value["dependencies"]["apt"], "~> 7.0");
        assert_eq!(value["platforms"]["ubuntu"], ">= 20.04");
        assert_eq!(value.as_object().unwrap().get("attributes"), None);
        assert!(matches!(value, Value::Object(_)));
    }
}
