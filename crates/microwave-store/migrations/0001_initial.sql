-- Microwave Server unified SQLite schema.
-- All timestamps are RFC 3339 UTC text. All IDs are 32 lowercase hex characters.

PRAGMA foreign_keys = ON;

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
    admin INTEGER NOT NULL DEFAULT 0 CHECK (admin IN (0, 1)),
    recovery_authentication_enabled INTEGER NOT NULL DEFAULT 0
        CHECK (recovery_authentication_enabled IN (0, 1)),
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
    validator INTEGER NOT NULL DEFAULT 0 CHECK (validator IN (0, 1)),
    admin INTEGER NOT NULL DEFAULT 0 CHECK (admin IN (0, 1)),
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

-- Keep the unified authz relation tables honest about the type of entity each
-- column may reference. PostgreSQL has the same checks as row triggers below.
CREATE TRIGGER IF NOT EXISTS authz_group_actors_actor_type_insert
BEFORE INSERT ON authz_group_actors
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.actor_authz_id), '') <> 'actor'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_actors.actor_authz_id must reference an actor');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_actors_actor_type_update
BEFORE UPDATE OF actor_authz_id ON authz_group_actors
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.actor_authz_id), '') <> 'actor'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_actors.actor_authz_id must reference an actor');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_actors_group_type_insert
BEFORE INSERT ON authz_group_actors
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_actors.group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_actors_group_type_update
BEFORE UPDATE OF group_authz_id ON authz_group_actors
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_actors.group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_groups_parent_type_insert
BEFORE INSERT ON authz_group_groups
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.parent_group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_groups.parent_group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_groups_parent_type_update
BEFORE UPDATE OF parent_group_authz_id ON authz_group_groups
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.parent_group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_groups.parent_group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_groups_child_type_insert
BEFORE INSERT ON authz_group_groups
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.child_group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_groups.child_group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_group_groups_child_type_update
BEFORE UPDATE OF child_group_authz_id ON authz_group_groups
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.child_group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_group_groups.child_group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_actor_actor_type_insert
BEFORE INSERT ON authz_acl_actor
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.actor_authz_id), '') <> 'actor'
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_actor.actor_authz_id must reference an actor');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_actor_actor_type_update
BEFORE UPDATE OF actor_authz_id ON authz_acl_actor
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.actor_authz_id), '') <> 'actor'
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_actor.actor_authz_id must reference an actor');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_group_group_type_insert
BEFORE INSERT ON authz_acl_group
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_group.group_authz_id must reference a group');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_group_group_type_update
BEFORE UPDATE OF group_authz_id ON authz_acl_group
WHEN COALESCE((SELECT entity_type FROM authz_entities WHERE authz_id = NEW.group_authz_id), '') <> 'group'
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_group.group_authz_id must reference a group');
END;

-- ACL targets may be any authz entity type (upstream keeps one ACL table per
-- target type; this schema unifies them). Require that the target actually
-- exists and has a known type even when a caller disabled foreign keys.
CREATE TRIGGER IF NOT EXISTS authz_acl_actor_target_type_insert
BEFORE INSERT ON authz_acl_actor
WHEN COALESCE(
         (SELECT entity_type FROM authz_entities WHERE authz_id = NEW.target_authz_id),
         ''
     ) NOT IN ('actor', 'group', 'object', 'container')
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_actor.target_authz_id must reference an authz entity');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_actor_target_type_update
BEFORE UPDATE OF target_authz_id ON authz_acl_actor
WHEN COALESCE(
         (SELECT entity_type FROM authz_entities WHERE authz_id = NEW.target_authz_id),
         ''
     ) NOT IN ('actor', 'group', 'object', 'container')
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_actor.target_authz_id must reference an authz entity');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_group_target_type_insert
BEFORE INSERT ON authz_acl_group
WHEN COALESCE(
         (SELECT entity_type FROM authz_entities WHERE authz_id = NEW.target_authz_id),
         ''
     ) NOT IN ('actor', 'group', 'object', 'container')
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_group.target_authz_id must reference an authz entity');
END;

CREATE TRIGGER IF NOT EXISTS authz_acl_group_target_type_update
BEFORE UPDATE OF target_authz_id ON authz_acl_group
WHEN COALESCE(
         (SELECT entity_type FROM authz_entities WHERE authz_id = NEW.target_authz_id),
         ''
     ) NOT IN ('actor', 'group', 'object', 'container')
BEGIN
    SELECT RAISE(ABORT, 'authz_acl_group.target_authz_id must reference an authz entity');
END;

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

-- Groups outside any organization. Upstream renders these as `::name` in ACLs;
-- `server-admins` and each organization's `<org>_read_access_group` live here.
CREATE TABLE IF NOT EXISTS global_groups (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS nodes (
    id TEXT PRIMARY KEY,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    environment TEXT NOT NULL DEFAULT '_default',
    policy_name TEXT,
    policy_group TEXT,
    serialized_object BLOB,
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
    serialized_object BLOB,
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
    serialized_object BLOB,
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
    serialized_object BLOB,
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
    serialized_object BLOB,
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

CREATE TABLE IF NOT EXISTS sandboxes (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    sandbox_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (org_id, sandbox_id)
);

CREATE TABLE IF NOT EXISTS sandbox_checksums (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    sandbox_id TEXT NOT NULL,
    checksum TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (sandbox_id, org_id, checksum)
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
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS cookbook_versions (
    id TEXT PRIMARY KEY,
    cookbook_id INTEGER NOT NULL REFERENCES cookbooks(id) ON DELETE RESTRICT,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    major INTEGER NOT NULL,
    minor INTEGER NOT NULL,
    patch INTEGER NOT NULL,
    frozen INTEGER NOT NULL DEFAULT 0 CHECK (frozen IN (0, 1)),
    meta_attributes BLOB,
    meta_deps TEXT,
    meta_long_desc BLOB,
    metadata BLOB,
    serialized_object BLOB,
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
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    authz_id TEXT NOT NULL UNIQUE REFERENCES authz_entities(authz_id) ON DELETE CASCADE,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS cookbook_artifact_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    cookbook_artifact_id INTEGER NOT NULL REFERENCES cookbook_artifacts(id) ON DELETE RESTRICT,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    identifier TEXT NOT NULL,
    metadata BLOB,
    serialized_object BLOB,
    created_at TEXT NOT NULL,
    created_by TEXT NOT NULL DEFAULT '',
    UNIQUE (cookbook_artifact_id, identifier)
);

-- Older databases were created with ON DELETE CASCADE. SQLite cannot alter an
-- existing foreign key, so the trigger enforces the upstream RESTRICT behavior
-- for those databases as well.
CREATE TRIGGER IF NOT EXISTS cookbook_artifacts_restrict_delete
BEFORE DELETE ON cookbook_artifacts
WHEN EXISTS (
    SELECT 1 FROM cookbook_artifact_versions
    WHERE cookbook_artifact_id = OLD.id
)
BEGIN
    SELECT RAISE(ABORT, 'cookbook_artifacts cannot be deleted while versions exist');
END;

CREATE TABLE IF NOT EXISTS cookbook_artifact_version_checksums (
    cookbook_artifact_version_id INTEGER NOT NULL
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
    serialized_object BLOB,
    last_updated_by TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (org_id, name),
    UNIQUE (org_id, authz_id, name)
);

CREATE TABLE IF NOT EXISTS policy_revisions (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    policy_id TEXT NOT NULL REFERENCES policies(id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL,
    name TEXT NOT NULL,
    serialized_object BLOB,
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
    UNIQUE (org_id, policy_group_name, policy_revision_name),
    FOREIGN KEY (org_id, policy_group_authz_id, policy_group_name)
        REFERENCES policy_groups (org_id, authz_id, name)
        ON UPDATE CASCADE ON DELETE CASCADE,
    FOREIGN KEY (org_id, policy_revision_name, policy_revision_revision_id)
        REFERENCES policy_revisions (org_id, name, revision_id)
        ON UPDATE CASCADE ON DELETE CASCADE
);

-- The initial schema predates the composite associations above. Add the
-- equivalent guards for databases that were created before those foreign keys
-- existed (SQLite cannot add a foreign key with ALTER TABLE).
CREATE TRIGGER IF NOT EXISTS policy_group_revision_associations_policy_group_insert
BEFORE INSERT ON policy_group_revision_associations
WHEN NOT EXISTS (
    SELECT 1 FROM policy_groups
    WHERE org_id = NEW.org_id
      AND authz_id = NEW.policy_group_authz_id
      AND name = NEW.policy_group_name
)
BEGIN
    SELECT RAISE(ABORT, 'policy group association references an unknown policy group');
END;

CREATE TRIGGER IF NOT EXISTS policy_group_revision_associations_policy_group_update
BEFORE UPDATE OF org_id, policy_group_authz_id, policy_group_name
ON policy_group_revision_associations
WHEN NOT EXISTS (
    SELECT 1 FROM policy_groups
    WHERE org_id = NEW.org_id
      AND authz_id = NEW.policy_group_authz_id
      AND name = NEW.policy_group_name
)
BEGIN
    SELECT RAISE(ABORT, 'policy group association references an unknown policy group');
END;

CREATE TRIGGER IF NOT EXISTS policy_group_revision_associations_policy_revision_insert
BEFORE INSERT ON policy_group_revision_associations
WHEN NOT EXISTS (
    SELECT 1 FROM policy_revisions
    WHERE org_id = NEW.org_id
      AND name = NEW.policy_revision_name
      AND revision_id = NEW.policy_revision_revision_id
)
BEGIN
    SELECT RAISE(ABORT, 'policy group association references an unknown policy revision');
END;

CREATE TRIGGER IF NOT EXISTS policy_group_revision_associations_policy_revision_update
BEFORE UPDATE OF org_id, policy_revision_name, policy_revision_revision_id
ON policy_group_revision_associations
WHEN NOT EXISTS (
    SELECT 1 FROM policy_revisions
    WHERE org_id = NEW.org_id
      AND name = NEW.policy_revision_name
      AND revision_id = NEW.policy_revision_revision_id
)
BEGIN
    SELECT RAISE(ABORT, 'policy group association references an unknown policy revision');
END;

CREATE TRIGGER IF NOT EXISTS policy_groups_cascade_associations
AFTER DELETE ON policy_groups
BEGIN
    DELETE FROM policy_group_revision_associations
    WHERE org_id = OLD.org_id
      AND policy_group_authz_id = OLD.authz_id
      AND policy_group_name = OLD.name;
END;

CREATE TABLE IF NOT EXISTS oauth_applications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    uid TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT '',
    confidential INTEGER NOT NULL DEFAULT 1 CHECK (confidential IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oauth_access_grants (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    resource_owner_id TEXT NOT NULL,
    application_id INTEGER NOT NULL REFERENCES oauth_applications(id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    expires_in INTEGER NOT NULL,
    redirect_uri TEXT NOT NULL,
    scopes TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE TABLE IF NOT EXISTS oauth_access_tokens (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    resource_owner_id TEXT,
    application_id INTEGER REFERENCES oauth_applications(id) ON DELETE CASCADE,
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
