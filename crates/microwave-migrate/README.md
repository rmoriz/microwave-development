# microwave-migrate

Migration support for Microwave Server. This crate owns the versioned JSON
snapshot format and the `microwave-migrate` binary. It can export a local
SQLite store or a PostgreSQL metadata store, import a snapshot into either
backend, crawl a live legacy Chef Infra Server into the same snapshot
envelope, or import a `knife ec backup` repository directly without an HTTP
server.

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

# Restore snapshot + bookshelf/ from an archive. Existing key files and
# empty-over-populated bookshelf restores abort unless --force is given;
# --secrets-dir puts all private files elsewhere.
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

# Also import server-owned private material from an operator-managed directory
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir ./new-data \
  --secrets-dir /root/chef-server-secrets \
  --yes

# The same material can be supplied as a bounded .tar/.tar.gz
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir ./new-data \
  --secrets-archive /root/chef-server-secrets.tar.gz \
  --yes

# Reuse a bootstrapped pivotal user and replace ACLs/membership from the backup
microwave-migrate import-knife-backup \
  --input /var/backups/chef-knife \
  --data-dir ./new-data \
  --merge-users \
  --replace-acls \
  --yes

# Parse, validate, and count without writing metadata, blobs, or secrets
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
- `data-dir` manifest entries are restricted to the fixed filename allowlist
  used by export. Nested paths (`nested/evil.conf`), database files
  (`microwave.db`), absolute paths, `..`, and inconsistent `suggested_name`
  values are rejected;
- the snapshot is parsed and its envelope validated before the database
  transaction starts;
- private destinations, parent directories, and overwrite conflicts are
  checked and all private bytes are staged to mode-`0600` temporary files
  before the database import, so a refused or unstageable import leaves
  metadata untouched;
- an empty archive `bookshelf/` never replaces a populated live bookshelf
  without `--force`;
- the database import runs in one transaction, then the staged `bookshelf/`
  replaces the target tree, then the staged private files are renamed into
  place as a set. If the private commit fails, the previous bookshelf is
  restored from its retained backup.

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
  intentionally rotating or replacing the installation. `--force` is also
  required to replace a populated live bookshelf with an empty archive
  bookshelf.
- If the target bookshelf is a symlink, the symlink itself is moved aside and
  replaced with the restored directory. Its target is never followed or
  modified.

The database and bookshelf are separate stores, so the bookshelf swap happens
after the database transaction commits. The previous bookshelf is retained
under a sidecar name until private files are committed; a private-file commit
failure restores it. A crash can still leave the new database with the old or
newly swapped bookshelf; re-run the import from the same archive to finish.
Keep the archive until the restored server has been verified.

Archives are capped at 1,000,000 entries and 16 GiB total uncompressed bytes.
A safe import (`secrets_included: false`) prints a warning that database secret
values were not restored; re-export with `--include-secrets` for a complete
migration.

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
- telemetry merges by `(property, event_timestamp)`: target-local events are
  preserved, repeated imports do not duplicate snapshot rows, and an
  absent/empty collection leaves target telemetry alone;
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

The importer canonicalizes collection paths and rejects symlinked collection
or intermediate directories. `long_description IO.read(...)` only accepts a
contained relative path inside the canonical cookbook directory (for example
`docs/README.md`); absolute paths, `..`, prefixes, and symlinks are rejected.

Legacy `expires_at` values (`YYYY-MM-DD HH:MM:SS UTC`, offset timestamps, and
RFC 3339) are normalized to RFC 3339 so expired keys remain expired. An
unparseable non-infinity expiry skips that key with a warning instead of
turning it into a non-expiring key. Missing `key_dump.json` or
`key_table_dump.json` also produces a warning.

Imports are additive by default: existing users with the same username cause a
clear error, and existing ACL grants/group members survive. Use
`--merge-users` to reuse an existing user id/authz id (for example a
bootstrapped `pivotal`). By default a merged user keeps non-empty target
credentials, admin/recovery state, profile fields, and an existing target
default key; the importer warns when the backup has no key material for that
user and then reconciles the effective `server-admins` membership to the final
admin flag (excluding a non-admin target user and demoting a target admin).
Pass `--replace-user-credentials` to apply credentials/admin/profile and the
default key from the backup instead; if the backup supplies no default key for
a merged user, the target default key is removed so revocation actually
revokes. That flag requires `--merge-users`. `--replace-acls` replaces ACLs for
targets present in the backup and membership only for groups read from group
files, so target-only/synthesized groups such as global `users` are not
cleared. Failed imports restore the captured previous ACL/group state. All
flags affect only the selected import and are echoed in the confirmation
prompt.

### Server-side private material

Chef Infra Server (Erlang) stores only public keys for users and clients. A
user's or client's private key is owned by that user/client and is not in a
`knife ec backup` repository. This importer therefore cannot migrate client
private keys; clients keep or regenerate their own keys and must be configured
with the new server URL separately. Only server-owned private material can be
migrated, and only when the operator explicitly supplies it.

Use `--secrets-dir DIR` or `--secrets-archive FILE.tar[.gz]`. Without one of
those flags no private server file is read or written. The importer maps:

| Source | Default target | Mode |
|---|---|---|
| `pivotal.pem` | `<data>/pivotal.pem` | `0600` |
| `pivotal.password` | `<data>/pivotal.password` | `0600` |
| `webui_priv.pem` | `<data>/webui_priv.pem` | `0600` |
| `webui_pub.pem` | `<data>/webui_pub.pem` | `0644` |
| `objectstore.secret` | `<data>/objectstore.secret` | `0600` |
| `private-chef-secrets.json` or `private_chef_secrets.json` | `<data>/private-chef-secrets.json` | `0600` |
| `ssl/cert.pem` (preferred) or `cert.pem` | configured `server.tls.cert_path`, `--tls-cert`, or the same relative path under `<data>` | `0644` |
| `ssl/key.pem` (preferred) or `key.pem` | configured `server.tls.key_path`, `--tls-key`, or the same relative path under `<data>` | `0600` |
| `nginx/ca/<fqdn>.crt` | configured TLS certificate path, `--tls-cert`, or the same relative path under `<data>` | `0644` |
| `nginx/ca/<fqdn>.key` | configured TLS key path, `--tls-key`, or the same relative path under `<data>` | `0600` |
| `ca/**`, other `nginx/ca/**` certificates | `<data>/<same relative path>` | `0644` |
| `ca/**/*.key`, CA-tree PEM private keys | `<data>/<same relative path>` | `0600` |

`<data>` is `--data-dir` for SQLite, or `--secrets-target-dir` when supplied.
With `--database-url`, the default is `paths.data_dir` from `--config`
(otherwise the configured default, normally `./data`). Newly created parent
directories are mode `0700`; existing directories are left unchanged. When
more than one TLS candidate exists, selection order is `ssl/cert.pem`/
`ssl/key.pem`, then `cert.pem`/`key.pem`, then the first sorted
`nginx/ca/*.crt`/`*.key` pair. Additional `nginx/ca/*.crt|*.key` pairs are
kept as CA material at their relative paths instead of being dropped. Private
CA detection uses a `.key` extension or a PEM private-key marker; DER-encoded
keys without either marker are not inferred.

Existing target files are refused unless `--force` is supplied. Symlink
targets are always refused. All secret files are staged before any target is
replaced; commit then renames the staged set, keeping backups of overwritten
files and rolling the set back if a commit fails. A process crash during the
commit window can still leave a partial set, so keep an operator backup and
re-run with `--force` after recovery. The command prints counts, target paths,
and modes only; it never prints file contents, password hashes, or key bytes.

`--secrets-archive` accepts `.tar`, `.tar.gz`, and `.tgz`. Only regular files
and directories are allowed. Paths must be relative with no `..` component,
the archive must contain only the names in the table above (plus safe `ca/`
and `nginx/ca/` trees), and symlinks, hardlinks, devices, and FIFOs are
rejected. Directory entries outside those allowlisted prefixes are rejected.
A single legacy top-level directory wrapping the allowlisted tree (for example
`chef-secrets/pivotal.pem`) is accepted. Individual files are limited to
16 MiB, the archive to 128 MiB, the expanded set to 64 MiB, and the source to
at most 4096 files; limits are enforced while reading, before each file is
buffered.

`webui_priv.pem`/`webui_pub.pem` and `private-chef-secrets.json` are preserved
for external tooling and operator reference. Microwave does not consume the
web-UI keypair or parse/merge the private-chef secret document at runtime.

### Intentional gaps

- No HTTP, `--purge`, or deletion sync. Import merges/upserts; objects removed
  from the backup remain in an existing target store. `--replace-acls` only
  replaces ACLs and membership for ACL targets and groups read from the
  backup; inferred/target-only groups are left alone. It captures and restores
  previous ACL/group state when the import fails, but is not one cross-target
  transaction (a crash during the operation can still require operator
  recovery). Merged-user `server-admins` reconciliation and default-key
  removal also run after the metadata transaction; a failure restores the
  captured membership/admin state, but a partial key removal is completed by
  re-running the same command.
- The `metadata.rb` parser is not a Ruby interpreter. It does not evaluate
  arbitrary Ruby, conditionals, string interpolation, `eval`, or custom
  helper methods. Prefer a `metadata.json` when the source provides one.
- Cookbook artifacts, policies/policy groups, sandboxes, OAuth applications,
  search indexes, and `kvpair` secrets are not in a knife backup and are not
  imported by this command. Snapshot import still carries policies, artifacts,
  and OAuth registrations when the snapshot contains them.
- Only the local object store is supported for blobs in this pass. Importing
  into an S3/Garage-backed deployment requires a separate blob upload.
- Server-secret import is a validated file copy. It does not verify that a TLS
  certificate/key pair matches, rotate or generate keys, or restart a running
  server. Archives are not decrypted; wrapper tooling must protect the
  operator-supplied archive at rest. The staged set is rolled back on an
  ordinary commit error, but a crash during commit can still leave a partial
  set; recovery is to restore the operator's secret backup and re-run with
  `--force`.
- User/key timestamps are preserved where the backup/export exposes them
  (organization records, key dumps, user and client JSON). API object bodies
  normally carry no timestamps, so those rows use the import time.
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
