-- Microwave Server unified PostgreSQL schema.
--
-- This mirrors crates/microwave-store/migrations/0001_initial.sql. Timestamps
-- are RFC 3339 UTC TEXT so rows round-trip identically between SQLite and
-- PostgreSQL, and serialized Chef documents stay TEXT/JSON for the same
-- reason. IDs are 32 lowercase hex characters (16 random bytes) unless the
-- column is a PostgreSQL sequence identifier.
--
-- All statements are idempotent so the embedded migration can be executed on
-- every process start. Key synchronization and ACL graph maintenance stay in
-- Rust transactions rather than triggers.

CREATE TABLE IF NOT EXISTS orgs (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL UNIQUE,
    full_name TEXT NOT NULL,
    assigned_at TEXT NOT NULL,
    last_updated_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE,
    username TEXT NOT NULL UNIQUE,
    email TEXT UNIQUE,
    password_hash TEXT NOT NULL DEFAULT '',
    password_salt TEXT NOT NULL DEFAULT '',
    hash_type TEXT NOT NULL DEFAULT 'bcrypt',
    admin BOOLEAN NOT NULL DEFAULT FALSE,
    recovery_authentication_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    external_authentication_uid TEXT,
    display_name TEXT,
    first_name TEXT,
    middle_name TEXT,
    last_name TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS users_lower_email_index ON users (lower(email));
CREATE INDEX IF NOT EXISTS users_external_authentication_uid_index
    ON users (external_authentication_uid);

CREATE TABLE IF NOT EXISTS org_user_associations (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (org_id, user_id)
);

CREATE TABLE IF NOT EXISTS org_user_invites (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, user_id)
);

CREATE TABLE IF NOT EXISTS clients (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    authz_id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    validator BOOLEAN NOT NULL DEFAULT FALSE,
    admin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS actor_keys (
    actor_id TEXT NOT NULL,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'client')),
    key_name TEXT NOT NULL,
    public_key TEXT NOT NULL,
    key_version INTEGER NOT NULL DEFAULT 1,
    expires_at TEXT NOT NULL DEFAULT 'infinity',
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (actor_id, key_name)
);

CREATE INDEX IF NOT EXISTS actor_keys_actor_index ON actor_keys (actor_id, actor_type);

CREATE TABLE IF NOT EXISTS authz_entities (
    authz_id TEXT PRIMARY KEY,
    entity_type TEXT NOT NULL CHECK (entity_type IN ('actor', 'group', 'object', 'container')),
    org_id TEXT REFERENCES orgs(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS authz_entities_org_index ON authz_entities (org_id);

CREATE TABLE IF NOT EXISTS authz_acl_actor (
    target_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    permission TEXT NOT NULL CHECK (permission IN ('create', 'read', 'update', 'delete', 'grant')),
    actor_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    PRIMARY KEY (target_authz_id, permission, actor_authz_id)
);

CREATE INDEX IF NOT EXISTS authz_acl_actor_actor_index
    ON authz_acl_actor (actor_authz_id, target_authz_id);

CREATE TABLE IF NOT EXISTS authz_acl_group (
    target_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    permission TEXT NOT NULL CHECK (permission IN ('create', 'read', 'update', 'delete', 'grant')),
    group_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    PRIMARY KEY (target_authz_id, permission, group_authz_id)
);

CREATE TABLE IF NOT EXISTS authz_group_actors (
    group_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    actor_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    PRIMARY KEY (group_authz_id, actor_authz_id)
);

CREATE INDEX IF NOT EXISTS authz_group_actors_actor_index
    ON authz_group_actors (actor_authz_id);

CREATE TABLE IF NOT EXISTS authz_group_groups (
    parent_group_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    child_group_authz_id TEXT NOT NULL REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    PRIMARY KEY (parent_group_authz_id, child_group_authz_id),
    CHECK (parent_group_authz_id <> child_group_authz_id)
);

CREATE INDEX IF NOT EXISTS authz_group_groups_child_index
    ON authz_group_groups (child_group_authz_id);

CREATE TABLE IF NOT EXISTS containers (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS groups (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '_default',
    policy_name TEXT,
    policy_group TEXT,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE INDEX IF NOT EXISTS nodes_org_environment_index ON nodes (org_id, environment);
CREATE INDEX IF NOT EXISTS nodes_org_policy_name_index ON nodes (org_id, policy_name);
CREATE INDEX IF NOT EXISTS nodes_org_policy_group_index ON nodes (org_id, policy_group);

CREATE TABLE IF NOT EXISTS roles (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS environments (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS data_bags (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS data_bag_items (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    data_bag_name TEXT NOT NULL,
    item_name TEXT NOT NULL,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, data_bag_name, item_name),
    FOREIGN KEY (org_id, data_bag_name)
        REFERENCES data_bags (org_id, name) ON UPDATE CASCADE ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS checksums (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    checksum TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (org_id, checksum)
);

CREATE TABLE IF NOT EXISTS sandboxed_checksums (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    sandbox_id TEXT NOT NULL,
    checksum TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (sandbox_id, org_id, checksum)
);

CREATE INDEX IF NOT EXISTS sandboxed_checksums_sandbox_index
    ON sandboxed_checksums (org_id, sandbox_id);

CREATE TABLE IF NOT EXISTS cookbooks (
    id BIGSERIAL PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS cookbook_versions (
    id TEXT PRIMARY KEY,
    cookbook_id BIGINT NOT NULL REFERENCES cookbooks(id) ON DELETE RESTRICT,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    major BIGINT NOT NULL,
    minor BIGINT NOT NULL,
    patch BIGINT NOT NULL,
    frozen BOOLEAN NOT NULL DEFAULT FALSE,
    meta_attributes TEXT,
    meta_deps TEXT,
    meta_long_desc TEXT,
    metadata TEXT,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (cookbook_id, major, minor, patch)
);

CREATE TABLE IF NOT EXISTS cookbook_version_checksums (
    cookbook_version_id TEXT NOT NULL REFERENCES cookbook_versions(id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    checksum TEXT NOT NULL,
    FOREIGN KEY (org_id, checksum)
        REFERENCES checksums (org_id, checksum) ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS cookbook_version_checksums_by_id
    ON cookbook_version_checksums (cookbook_version_id);

CREATE TABLE IF NOT EXISTS cookbook_artifacts (
    id BIGSERIAL PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS cookbook_artifact_versions (
    id BIGSERIAL PRIMARY KEY,
    cookbook_artifact_id BIGINT NOT NULL REFERENCES cookbook_artifacts(id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    identifier TEXT NOT NULL,
    metadata TEXT,
    serialized_object TEXT,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    UNIQUE (cookbook_artifact_id, identifier)
);

CREATE TABLE IF NOT EXISTS cookbook_artifact_version_checksums (
    cookbook_artifact_version_id BIGINT NOT NULL
        REFERENCES cookbook_artifact_versions(id) ON DELETE CASCADE,
    org_id TEXT NOT NULL,
    checksum TEXT NOT NULL,
    FOREIGN KEY (org_id, checksum)
        REFERENCES checksums (org_id, checksum) ON UPDATE CASCADE ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS policies (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS policy_groups (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS policy_revisions (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    policy_id TEXT NOT NULL REFERENCES policies(id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL,
    name TEXT NOT NULL,
    serialized_object TEXT,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name, revision_id)
);

CREATE TABLE IF NOT EXISTS policy_group_revision_associations (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    policy_revision_id TEXT NOT NULL REFERENCES policy_revisions(id) ON DELETE CASCADE,
    policy_group_authz_id TEXT NOT NULL,
    policy_revision_name TEXT NOT NULL,
    policy_revision_revision_id TEXT NOT NULL,
    policy_group_name TEXT NOT NULL,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, policy_group_name, policy_revision_name)
);

CREATE TABLE IF NOT EXISTS oauth_applications (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    uid TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT '',
    confidential BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_access_grants (
    id BIGSERIAL PRIMARY KEY,
    resource_owner_id TEXT NOT NULL,
    application_id BIGINT NOT NULL REFERENCES oauth_applications(id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    expires_in INTEGER NOT NULL,
    redirect_uri TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE IF NOT EXISTS oauth_access_tokens (
    id BIGSERIAL PRIMARY KEY,
    resource_owner_id TEXT,
    application_id BIGINT REFERENCES oauth_applications(id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    refresh_token TEXT UNIQUE,
    expires_in INTEGER,
    scopes TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE IF NOT EXISTS kvpair (
    key TEXT PRIMARY KEY,
    value TEXT,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS telemetry (
    property TEXT NOT NULL,
    event_timestamp TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS telemetry_property_index ON telemetry (property);
