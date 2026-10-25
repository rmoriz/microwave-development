# microwave-migrate

Migration support for Microwave Server. This crate owns the versioned JSON
snapshot format and the `microwave-migrate` binary. It can export a local
SQLite store or a PostgreSQL metadata store, import a snapshot into either
backend, or crawl a live legacy Chef Infra Server into the same snapshot
envelope.

## Commands

```sh
# Write a snapshot of ./data/microwave.db (secrets redacted)
microwave-migrate export --data-dir ./data --output migration.json

# Include kvpair, OAuth grants/tokens, and OAuth application secrets
microwave-migrate export --data-dir ./data --output migration.json --include-secrets

# Write a PostgreSQL metadata snapshot
microwave-migrate export \
  --database-url postgres://microwave:secret@db/microwave \
  --output migration.json

# Write a self-contained archive: snapshot.json + local bookshelf/ blobs.
# Database secrets and on-disk private keys are both excluded by default.
microwave-migrate export --data-dir ./data --archive migration.tar.gz

# Complete self-migration: include DB secret rows and private key files.
# The result is credential material; protect and transfer it accordingly.
microwave-migrate export --data-dir ./data --archive migration.tar.gz \
  --include-secrets --include-private-keys \
  --config /etc/microwave/microwave.toml

# PostgreSQL metadata + a local bookshelf tree
microwave-migrate export \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave/bookshelf \
  --archive migration.tar.gz

# Create or merge into ./new-data/microwave.db
microwave-migrate import --data-dir ./new-data --input migration.json --yes

# Restore snapshot + bookshelf/ from an archive. Existing key files abort
# unless --force is given; --secrets-dir puts all private files elsewhere.
microwave-migrate import --data-dir ./new-data --archive migration.tar.gz --yes
microwave-migrate import --data-dir ./new-data --archive migration.tar.gz \
  --secrets-dir /safe/staging/private --yes
microwave-migrate import --data-dir ./new-data --archive migration.tar.gz \
  --config /etc/microwave/microwave.toml --force --yes

# PostgreSQL archive import, restoring the local bookshelf tree too
microwave-migrate import \
  --database-url postgres://microwave:secret@db/microwave \
  --bookshelf-dir /var/lib/microwave/bookshelf \
  --archive migration.tar.gz \
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

# Import through the operational CLI (uses the configured database path)
microwave-ctl migrate-from-legacy --input migration.json --yes
```

`import` merges into live metadata and asks for confirmation; `--yes` is
required in scripts and other non-interactive environments.

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
  "secrets_included": false,
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
    "oauth_applications": [],
    "oauth_access_grants": [],
    "oauth_access_tokens": [],
    "kvpair": [],
    "sandboxes": [],
    "sandbox_checksums": [],
    "telemetry": []
  }
}
```

Every collection is optional on read (`serde(default)`). A snapshot written by
an older build omits the new collections and imports as empty; a new snapshot
written without `--include-secrets` contains empty secret collections.

### Envelope fields

| Field | Meaning |
|---|---|
| `format` | Always `microwave-server-snapshot`; anything else is rejected. |
| `version` | Snapshot schema version. This build accepts `1`. |
| `generated_at` | RFC 3339 UTC export time. |
| `source.kind` | `local-store` for local exports, `legacy-api` for crawler output. |
| `source.url` | Legacy API URL for `legacy-api`; `null` otherwise. |
| `source.data_dir` | Local data directory the snapshot was exported from. |
| `secrets_included` | `true` only when `--include-secrets` was used. Older snapshots without the field are treated as `false`. |
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
| `oauth_access_grants`, `oauth_access_tokens` | Pending authorization codes and bearer/refresh tokens, including ids, timestamps, expiry, and revocation |
| `kvpair` | Raw key/value rows: operator secrets (`secret:*`), browser sessions, password-reset tokens, custom keys, and `maintenance:enabled` |
| `sandboxes` | Explicit sandbox rows, including empty in-flight sandboxes with no uploads |
| `sandbox_checksums` | SQLite's pending-upload rows used by `commit_sandbox`; PostgreSQL derives these from `sandboxed_checksums` |
| `telemetry` | Append-only `property` / `event_timestamp` rows when the source table exists |

`serialized_object`, metadata, and manifest fields are stored as the exact
JSON text held by the local store, so object bodies survive byte-for-byte.

### Secrets and what remains intentionally absent

Two independent opt-ins control sensitive data:

| Flag | What it adds | Default |
|---|---|---|
| `--include-secrets` | `kvpair` (operator secrets, sessions, reset tokens, maintenance flag), OAuth access grants/tokens, and OAuth application secrets in `snapshot.json` | omitted/blank |
| `--include-private-keys` | On-disk key/certificate material under `secrets/` | omitted |

For a truly complete self-migration, pass **both** flags. `--include-secrets`
alone still leaves `pivotal.pem`, TLS keys, and the other files on disk behind;
`--include-private-keys` alone copies those files but leaves database-backed
secrets redacted.

`--include-private-keys` uses an explicit manifest, never a glob. It reads only
files that exist from these lists:

- `<data-dir>/pivotal.pem`
- `<data-dir>/pivotal.password`
- `<data-dir>/objectstore.secret`
- `<data-dir>/private-chef-secrets.json` or `private_chef_secrets.json`
- WebUI/oc-id keys, when present: `webui_priv.pem`, `webui_pub.pem`,
  `oc_id_priv.pem`, `oc_id_pub.pem`, `ocid_priv.pem`, `ocid_pub.pem`,
  `oc-id.pem`, `oc_id.pem`
- the configured `server.tls.cert_path` and `server.tls.key_path` from
  `--config` / `MW_CONFIG` / `./microwave.toml`. `cert_path` may be a PEM
  bundle containing the chain.

Unrecognized files, glob matches, and public data are never included.
Symlinked data-dir candidates abort the export rather than being followed;
explicitly configured TLS paths may be managed symlinks (for example Let's
Encrypt `live/current`) and are read through.

**Never printed.** The CLI reports only `Secrets: included/excluded` and
`Private keys: included/excluded`; it never echoes key contents, passwords, or
tokens. Snapshot and archive files are mode `0600`; extracted private files are
mode `0600` and newly created parent directories mode `0700`.

- **Plaintext passwords.** Only `password_hash`, `password_salt`, and
  `hash_type` are exported. Legacy hash types are preserved; the login path can
  upgrade them later.
- **Search indexes.** They are derived data; rebuild them after migration.
- **Remote object stores.** Archives copy a local `bookshelf/` directory only.
  For S3, Garage, or MinIO, replicate the bucket with your object-store
  tooling; `microwave-migrate` does not enumerate remote objects.

The snapshot still contains password hashes, public keys, ACLs, and user data,
so even a safe export is sensitive.

## Archive format

`--archive PATH.tar.gz` writes a gzip-compressed tar with these members:

```text
snapshot.json
bookshelf/
  organization-<org_id>/
    checksum-<md5>
secrets/                     # only with --include-private-keys
  manifest.json
  files/
    0001-pivotal.pem
    0002-site.crt
```

The snapshot is byte-identical to the JSON export for the same secret mode.
`bookshelf/` is the local object-store root (`<data_dir>/bookshelf` for
SQLite). For PostgreSQL, point `--bookshelf-dir` at the local tree; the
metadata comes from `--database-url`.

`secrets/manifest.json` is a JSON document mapping each `secrets/files/...`
member to its `kind` (`data-dir`, `tls-certificate`, `tls-private-key`), source
`original_path`, data-dir `relative_path`, `suggested_name`, and source `mode`.
The manifest is metadata only; it never contains file contents.

Archive import extracts and validates into a private staging directory before
touching live data:

- only `snapshot.json`, `bookshelf/`, and `secrets/` entries are allowed;
- absolute paths, `..`, empty names, backslash-separated names, symlinks, hard
  links, devices, FIFOs, and duplicate entries are rejected;
- every private payload must be listed exactly once in the manifest; an
  unreferenced `secrets/files/` member or a missing manifest is rejected;
- the snapshot is parsed and its envelope validated before the database
  transaction starts;
- private destinations and overwrite conflicts are checked before the database
  import, so a refused import leaves metadata untouched;
- the database import runs in one transaction, then the staged `bookshelf/`
  replaces the target tree, then private files are written.

Restore destinations:

- **Without `--secrets-dir`:** data-dir files return to the target data
  directory using their original `relative_path`; TLS material returns to the
  target `server.tls.cert_path` / `server.tls.key_path` from `--config`. If the
  archive has TLS material but the target config has no matching path, the
  import fails before touching the database.
- **With `--secrets-dir DIR`:** every private file is written under `DIR` using
  its `suggested_name` (for example `DIR/pivotal.pem`, `DIR/site.key`). Existing
  files are still protected by the overwrite check.
- Existing key files are never replaced without `--force`. Overwriting a key
  can invalidate clients that still trust the old key; use `--force` only when
  intentionally rotating or replacing the installation.

The database and bookshelf are separate stores, so the bookshelf swap happens
after the database transaction commits. A crash between them leaves the new
metadata with the old blobs; re-run the import from the same archive to finish.
Private files are written after that, so a storage failure can similarly leave
metadata restored with some private files not yet written; the archive remains
safe to re-run with `--force` where necessary. Keep the archive until the
restored server has been verified.

Import remains confirmation-gated. In scripts pass `--yes`; without it a
non-interactive stdin is refused before any change. A non-local object store
has no local bookshelf to export or replace, so archive export/import fails
with an instruction to use bucket replication (`rclone`, `aws s3 sync`,
Garage's replication, and so on).

## Import semantics

Import runs in a single database transaction on either backend. Rows are
upserted by primary key, so:

- a fresh install receives the source ids, authz ids, timestamps, hashes, and
  keys unchanged;
- importing the same snapshot twice does not duplicate rows;
- importing into an existing store merges by id and updates the snapshot's
  rows. Existing rows that are not in the snapshot are left in place. ACL and
  group-membership rows are inserted idempotently, and cookbook checksum sets
  are replaced per version to avoid duplicate association rows;
- explicit `sandboxes` rows restore empty in-flight sandboxes. Older snapshots
  that only carry `sandboxed_checksums` still derive both explicit sandboxes and
  SQLite's `sandbox_checksums` rows;
- non-empty `telemetry` collections replace the target event log so repeated
  imports do not duplicate rows. An absent/empty collection leaves target
  telemetry alone, matching the merge semantics used elsewhere;
- a safe snapshot leaves existing target secrets intact: omitted `kvpair` and
  OAuth grant/token rows are not deleted, and an empty OAuth application
  `secret` does not overwrite a non-empty stored secret;
- OAuth grants and tokens keep their ids, timestamps, expiry, and revocation
  state; PostgreSQL advances those sequences after import.

Name uniqueness still applies: a row with the same unique name but a different
id is rejected by the backend rather than silently rewritten. This matters most
when importing into a non-empty PostgreSQL database that already has the
global `server-admins` group from another installation; import into a fresh
database (or reconcile the ids first) for a clean migration.

PostgreSQL's sequence-backed identifiers (`cookbooks`, `cookbook_artifacts`,
`cookbook_artifact_versions`, `oauth_applications`, `oauth_access_grants`, and
`oauth_access_tokens`) are inserted with the snapshot ids, then their sequences
are advanced to the highest imported id. Later inserts cannot collide with the
restored rows.

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
