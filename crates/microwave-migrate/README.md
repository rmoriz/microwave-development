# microwave-migrate

Offline migration support for Microwave Server. This crate owns the versioned
JSON snapshot format and the `microwave-migrate` binary. The live legacy API
crawler is deferred to a later slice (see [Deferred live-API crawler](#deferred-live-api-crawler)).

## Commands

```sh
# Write a snapshot of ./data/microwave.db
microwave-migrate export --data-dir ./data --output migration.json

# Create or merge into ./new-data/microwave.db
microwave-migrate import --data-dir ./new-data --input migration.json

# Import through the operational CLI (uses the configured database path)
microwave-ctl migrate-from-legacy --input migration.json
```

`import` validates the envelope before touching the database. Invalid JSON,
an unexpected `format`, or an unsupported `version` exit non-zero and do not
create or modify the target database.

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
| `source.kind` | `local-store` today; `legacy-api` is reserved for the crawler. |
| `source.url` | Legacy API URL, when applicable. |
| `source.data_dir` | Local data directory the snapshot was exported from. |
| `data` | Table-shaped store rows, described below. |

### Collections

All rows preserve the source primary keys, authz ids, and RFC 3339
timestamps. Fields that are nullable in the store are JSON `null`.

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
  `kvpair`, and OAuth access tokens/grants.** This slice migrates metadata,
  authz, keys, checksums, and OAuth application registrations. Copy or
  re-upload cookbook blobs separately, and let the search index rebuild.

## Import semantics

Import runs in a single SQLite transaction. Rows are upserted by primary key,
so:

- a fresh install receives the source ids, authz ids, timestamps, hashes, and
  keys unchanged;
- importing the same snapshot twice does not duplicate rows;
- importing into an existing store merges by id and updates the snapshot's
  rows. Existing rows that are not in the snapshot are left in place. ACL and
  group-membership rows are inserted idempotently, and cookbook checksum sets
  are replaced per version to avoid duplicate association rows.

Name uniqueness still applies: a row with the same unique name but a different
id is rejected by SQLite rather than silently rewritten.

## Deferred live-API crawler

`from-api --url URL --client-name NAME --key PATH --output FILE` is present so
the command surface does not change later, but it currently exits non-zero with
a clear `from-api is not implemented yet` message and writes no file.

The planned implementation will read the PEM key, sign each request with
`microwave-crypto::sign_request`, and crawl organizations plus each
organization's clients, nodes, roles, environments, data bags/items, cookbooks,
and policies into the same `microwave-server-snapshot` envelope (with
`source.kind = "legacy-api"` and `source.url` set). Until then, the supported
path is a local store export/import.
