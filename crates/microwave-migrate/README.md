# microwave-migrate

Migration support for Microwave Server. This crate owns the versioned JSON
snapshot format and the `microwave-migrate` binary. It can export a local
SQLite store or a PostgreSQL metadata store, import a snapshot into either
backend, crawl a live legacy Chef Infra Server into the same snapshot
envelope, or import a `knife ec backup` repository directly without an HTTP
server.

## Commands

```sh
# Write a snapshot of ./data/microwave.db
microwave-migrate export --data-dir ./data --output migration.json

# Write a snapshot of a PostgreSQL metadata store
microwave-migrate export \
  --database-url postgres://microwave:secret@db/microwave \
  --output migration.json

# Create or merge into ./new-data/microwave.db
microwave-migrate import --data-dir ./new-data --input migration.json --yes

# Import into PostgreSQL (one transaction, ids and timestamps preserved)
microwave-migrate import \
  --database-url postgres://microwave:secret@db/microwave \
  --input migration.json \
  --yes

# Crawl a live legacy server as a superuser
microwave-migrate from-api \
  --url https://chef.example.com \
  --client-name migrator \
  --key migrator.pem \
  --output migration.json

# Crawl one organization with a non-default API version
microwave-migrate from-api \
  --url https://chef.example.com \
  --client-name migrator \
  --key migrator.pem \
  --org acme \
  --api-version 2 \
  --output acme.json

# Import a knife ec backup directory into SQLite and ./data/bookshelf
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir ./new-data \
  --yes

# Import the same layout into PostgreSQL (the local bookshelf root is explicit)
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave-new/bookshelf \
  --yes

# Parse and count without touching the database or object store
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir ./new-data \
  --dry-run

# Import through the operational CLI (uses the configured database path)
microwave-ctl migrate-from-legacy --input migration.json --yes
```

`import` and `import-knife-backup` merge into live metadata and ask for
confirmation; `--yes` is required in scripts and other non-interactive
environments. `--dry-run` never confirms and never writes.

`import` validates the envelope before touching the database. Invalid JSON,
an unexpected `format`, or an unsupported `version` exit non-zero and do not
create or modify the target database.

`from-api` reads the PEM key, signs every request with
`microwave-crypto::sign_request` (`X-Ops-Sign: algorithm=sha256;version=1.3;`),
and writes the snapshot atomically with owner-only permissions. It never
writes the private key or a plaintext user password. If a request fails
authentication, the crawl aborts and no output file is written.

## Snapshot format

Snapshots are UTF-8, pretty-printed JSON. Unknown fields are ignored when
reading, and every collection in `data` is optional. The current writer emits
schema version `1`.

```json
{
  "format": "microwave-server-snapshot",
  "version": 1,
  "generated_at": "2026-10-24T12:00:00Z",
  "source": {
    "kind": "local-store",
    "url": null,
    "data_dir": "./data"
  },
  "data": {
    "orgs": [],
    "users": [],
    "org_user_associations": [],
    "org_user_invites": [],
    "clients": [],
    "actor_keys": [],
    "authz_entities": [],
    "authz_acl_actor": [],
    "authz_acl_group": [],
    "authz_group_actors": [],
    "authz_group_groups": [],
    "containers": [],
    "groups": [],
    "nodes": [],
    "roles": [],
    "environments": [],
    "data_bags": [],
    "data_bag_items": [],
    "checksums": [],
    "sandboxed_checksums": [],
    "cookbooks": [],
    "cookbook_versions": [],
    "cookbook_version_checksums": [],
    "cookbook_artifacts": [],
    "cookbook_artifact_versions": [],
    "cookbook_artifact_version_checksums": [],
    "policies": [],
    "policy_groups": [],
    "policy_revisions": [],
    "policy_group_revision_associations": [],
    "oauth_applications": []
  }
}
```

### Envelope fields

| Field | Meaning |
|---|---|
| `format` | Always `microwave-server-snapshot`; anything else is rejected. |
| `version` | Snapshot schema version. This build accepts `1`. |
| `generated_at` | RFC 3339 UTC export time. |
| `source.kind` | `local-store` for local exports, `legacy-api` for crawler output. |
| `source.url` | Legacy API URL for `legacy-api`; `null` otherwise. |
| `source.data_dir` | Local data directory the snapshot was exported from. |
| `data` | Table-shaped store rows, described below. |

### Collections

All rows preserve the source primary keys, authz ids, and RFC 3339
timestamps. Fields that are nullable in the store are JSON `null`.

Local exports preserve those values exactly. The legacy API does not expose
internal ids, authz ids, or object timestamps, so `from-api` derives stable
32-hex ids from organization and object names. That keeps repeated crawls and
repeated imports idempotent, but the ids will not match a legacy server's
database ids. See [Live legacy API crawler](#live-legacy-api-crawler).

| Collection | Notable fields |
|---|---|
| `orgs` | `id`, `authz_id`, `name`, `full_name`, `assigned_at`, `created_at`, `updated_at` |
| `users` | `id`, `authz_id`, `username`, `email`, `password_hash`, `password_salt`, `hash_type`, `admin`, `recovery_authentication_enabled`, `external_authentication_uid` |
| `org_user_associations` | `org_id`, `user_id`, timestamps |
| `org_user_invites` | `id`, `org_id`, `user_id`, timestamps |
| `clients` | `id`, `org_id`, `authz_id`, `name`, `validator`, `admin` |
| `actor_keys` | `actor_id`, `actor_type` (`user`/`client`), `key_name`, `public_key`, `key_version`, `expires_at` |
| `authz_entities` | `authz_id`, `entity_type` (`actor`/`group`/`object`/`container`), `org_id` |
| `authz_acl_actor`, `authz_acl_group` | `target_authz_id`, `permission`, actor/group authz id |
| `authz_group_actors`, `authz_group_groups` | Group membership and nesting by authz id |
| `containers`, `groups` | `id`, `org_id`, `authz_id`, `name` |
| `nodes` | `id`, `authz_id`, `org_id`, `name`, `environment`, `policy_name`, `policy_group`, `serialized_object` |
| `roles`, `environments`, `data_bags` | `id`, `authz_id`, `org_id`, `name`, `serialized_object` |
| `data_bag_items` | `id`, `org_id`, `data_bag_name`, `item_name`, `serialized_object` |
| `checksums`, `sandboxed_checksums` | Content checksums and pending sandbox uploads |
| `cookbooks` | Integer `id`, `org_id`, `name`, `authz_id` |
| `cookbook_versions` | `id`, `cookbook_id`, semantic version parts, frozen flag, metadata, manifest |
| `cookbook_version_checksums` | `cookbook_version_id`, `org_id`, `checksum` |
| `cookbook_artifacts`, `cookbook_artifact_versions`, `cookbook_artifact_version_checksums` | Policyfile cookbook artifacts |
| `policies`, `policy_groups`, `policy_revisions`, `policy_group_revision_associations` | Policyfile objects and group assignments |
| `oauth_applications` | `id`, `name`, `uid`, `secret`, `redirect_uri`, `scopes`, `confidential` |

`serialized_object`, metadata, and manifest fields are stored as the exact
JSON text held by the local store, so object bodies survive byte-for-byte.

### What is intentionally absent

- **Plaintext passwords.** Only `password_hash`, `password_salt`, and
  `hash_type` are exported. Legacy hash types are preserved; the login path can
  upgrade them later.
- **Private keys.** The store only holds public keys, and the exporter reads
  only database rows. Adjacent PEM files such as `<data_dir>/pivotal.pem` are
  never read.
- **Cookbook blobs / the bookshelf directory, search indexes, telemetry,
  `kvpair`, empty sandboxes, and OAuth access tokens/grants.** This slice
  migrates metadata, authz, keys, in-flight sandbox uploads, and OAuth
  application registrations. Copy or re-upload cookbook blobs separately, and
  let the search index rebuild.

## Import semantics

Import runs in a single database transaction on either backend. Rows are
upserted by primary key, so:

- a fresh install receives the source ids, authz ids, timestamps, hashes, and
  keys unchanged;
- importing the same snapshot twice does not duplicate rows;
- importing into an existing store merges by id and updates the snapshot's
  rows. Existing rows that are not in the snapshot are left in place. ACL and
  group-membership rows are inserted idempotently, and cookbook checksum sets
  are replaced per version to avoid duplicate association rows.

Name uniqueness still applies: a row with the same unique name but a different
id is rejected by the backend rather than silently rewritten. This matters most
when importing into a non-empty PostgreSQL database that already has the
global `server-admins` group from another installation; import into a fresh
database (or reconcile the ids first) for a clean migration.

PostgreSQL's sequence-backed identifiers (`cookbooks`, `cookbook_artifacts`,
`cookbook_artifact_versions`, and `oauth_applications`) are inserted with the
snapshot ids, then their sequences are advanced to the highest imported id.
Later inserts cannot collide with the restored rows.

## `knife ec backup` directory import

`import-knife-backup` reads the repository written by
[`knife-ec-backup`](https://github.com/chef/knife-ec-backup) directly. It does
not start a server and never sends HTTP requests. The importer builds the same
`StoreSnapshot` used by `import`, writes cookbook blobs through the local
object store, and then imports metadata in one store transaction. Its output
contains counts and file paths only; password hashes, public keys, and file
contents are never printed.

### Supported layout

```text
users/<name>.json
user_acls/<name>.json
key_dump.json                  # optional, from --with-user-sql
key_table_dump.json            # optional, from --with-key-sql
organizations/<org>/org.json
organizations/<org>/members.json
organizations/<org>/invitations.json
organizations/<org>/clients/<name>.json
organizations/<org>/containers/<name>.json
organizations/<org>/groups/<name>.json
organizations/<org>/nodes/<name>.json
organizations/<org>/roles/<name>.json
organizations/<org>/environments/<name>.json
organizations/<org>/data_bags/<bag>/<item>
organizations/<org>/cookbooks/<name>-<version>/...
organizations/<org>/acls/<type>/<name>.json
organizations/<org>/acls/organization.json
```

The importer preserves organization `guid`s, user and client public keys,
multi-key rows (`key_name`, `key_version`, `expires_at`), API object bodies,
data bag item bodies, group membership/nesting, and cookbook frozen status.
Internal ids and authz ids are generated deterministically from the backup
scope when the repository has none, so importing the same directory twice is
idempotent. When `key_dump.json` or a user object contains source ids, those
ids are reused.

`key_dump.json` is read as the JSON array exported by `ec_key_export.rb`:
`username`, `hashed_password`, `salt`, `hash_type`, `admin`, and optional
`id`/`authz_id`/`public_key`. `key_table_dump.json` is read as the `keys_by_name`
rows: `type` (`user`/`client`), `name`, `org_name`, `key_name`, `public_key`,
`key_version`, and `expires_at`. Inline `public_key` fields in user/client
JSON become the `default` key only when the key table does not already define
one.

ACLs accept both API shapes: legacy `actors`/`groups` and granular
`users`/`clients`/`groups`. Global `admins` on a user ACL maps to Microwave's
`server-admins` group. Because the backup does not contain global groups,
the importer recreates the standard graph: administrators join `server-admins`
and each organization's `admins`/`users` groups are children of its
`<org>_read_access_group`.

Cookbooks are ChefFS directories, not server manifests. The importer:

- reads `metadata.json`, or a practical subset of `metadata.rb`
  (`name`, `version`, scalar metadata, `depends`, `supports`, related
  constraint maps, and the common `long_description IO.read(README...)` form;
  `attribute`, `recipe`, and `provides` declarations are not reconstructed
  from the Ruby DSL);
- reads `status.json` (`frozen`/`frozen?`) and preserves the frozen flag;
- hashes every regular cookbook file with MD5, excluding `status.json`;
- builds a segmented Chef cookbook manifest and the matching `checksums`,
  `cookbooks`, `cookbook_versions`, and `cookbook_version_checksums` rows;
- copies each blob to `<bookshelf-dir>/organization-<org_id>/checksum-<md5>`;
- refuses symlinks and other non-regular files in cookbook directories.

Environment `cookbook_versions` constraints are preserved in the environment
body, so the existing dependency solver applies them unchanged.

### Intentional gaps

- No HTTP, `--purge`, or deletion sync. Import merges/upserts; objects removed
  from the backup remain in an existing target store.
- The `metadata.rb` parser is not a Ruby interpreter. It does not evaluate
  arbitrary Ruby, conditionals, string interpolation, `eval`, or custom
  helper methods. Prefer a `metadata.json` when the source provides one.
- Cookbook artifacts, policies/policy groups, sandboxes, OAuth applications,
  search indexes, and `kvpair` secrets are not in a knife backup and are not
  imported by this command. Snapshot import still carries policies, artifacts,
  and OAuth registrations when the snapshot contains them.
- Only the local object store is supported for blobs in this pass. Importing
  into an S3/Garage-backed deployment requires a separate blob upload.
- Cookbook files are buffered in memory while the directory is parsed. Very
  large multi-gigabyte backups should be split by organization or handled with
  a streaming importer in a future pass.
- Old `key_dump.json` rows whose password exists only inside the serialized
  user object and that have no `hash_type` are imported with an empty or
  default-typed hash. Modern `hashed_password`/`salt`/`hash_type` rows are
  preserved and upgraded on login like snapshot imports.
- Invalid `status.json`, malformed JSON, and unknown required references abort
  before the metadata transaction. Unknown ACL actors/groups and unsupported
  object ACL types are reported as warnings and skipped.
- `knife-ec-backup` does not include global group membership. The standard
  admin/read-access graph described above is inferred; custom global-group
  membership must be reconciled separately.

## Live legacy API crawler

`from-api` reads the legacy server through its read-only HTTP API. It supports
`--org NAME` to limit the crawl to one organization and `--api-version N`
(default `2`) to set `X-Ops-Server-API-Version`. Every request is signed with
`X-Ops-Sign: algorithm=sha256;version=1.3;` and the canonical path, body hash,
user id, timestamp, and API version required by Chef's signature protocol.

### What is crawled

- organizations, including `guid`, `name`, and `full_name`
- users and their public keys
- organization memberships (`org_user_associations`)
- clients and their public keys
- groups and their actor/group membership
- nodes, roles, and environments
- data bags and data bag items
- cookbooks and cookbook versions, including file checksums and metadata
- policies, policy revisions, policy groups, and group-to-revision assignments
- object ACLs for organizations, users, clients, groups, nodes, roles,
  environments, data bags, cookbooks, policies, and policy groups

Lists may be JSON URL maps, arrays, or `results`/`items` pages with a `next`
link. Optional collections, keys, and ACL endpoints that return `404` are
skipped. A `401`/`403`, an explicit `--org` that does not exist, or any other
unexpected status aborts the crawl without writing the output file.

### Identity and limitations

The legacy API does not expose internal database ids, authz ids, creation or
update timestamps, user password hashes, or the `admin` flag. The crawler:

- preserves organization `guid` values as organization ids;
- derives all other ids and authz ids deterministically from names, so the
  same source produces the same snapshot;
- uses the crawl time for `created_at`/`updated_at`/`assigned_at`;
- leaves user password hashes/salts empty and `admin` false;
- stores only public keys from the keys API (or an inline `public_key` on
  older API versions), never private keys.

When an ACL actor name matches both a user and a client in the organization,
the client is preferred because ACLs are organization scoped.

### Deferred

This slice does not crawl containers, cookbook artifacts (Policyfile
artifacts), sandboxes/sandboxed checksums, OAuth applications, invitation
records, search indexes, or cookbook blobs. It also does not copy user
password hashes, which the legacy API never returns; users will need a
password reset or an external identity provider after migration. Copy
cookbook blobs/bookshelf data separately and let the search index rebuild.
