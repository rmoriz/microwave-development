# Goiardi migration importer (`from-goiardi`)

**Date:** 2026-11-02
**Status:** design, awaiting approval

## Intent

Move a real, long-lived `goiardi` installation onto Microwave Server. The
source is the operator's own server, a single-organization
goiardi with 16 nodes, 30 clients, 20 roles, 5 data bags, 79 cookbooks in 116
versions, and one user (`admin`). The migration is one-way and happens once;
goiardi is retired afterwards.

The importer reads the source through its HTTP API, read-only, and writes the
existing versioned snapshot. It does **not** replace goiardi and does not keep
it in sync.

## Why a dedicated path

`microwave-migrate from-api` already crawls a legacy Chef Infra Server, but
goiardi is not path-compatible with one. Its API is flat (`/clients`,
`/nodes`, `/roles`, `/environments`, `/data`, `/cookbooks`, `/users`,
`/file_store/<md5>`) and has no `/organizations` route at all, so `from-api`
fails on its first request. Goiardi also has no groups, no `_acl` endpoint, no
keys endpoints, no policies and no cookbook artifacts. Adding a flag to
`from-api` would fork most of the crawl.

Goiardi does target compatibility with Chef Server 11.1.7 for object shapes,
but not for the namespace; a future `from-api --layout chef11` can reuse this
module's plumbing. The command is therefore named after the source, not after
a Chef version.

## Goals

- Import users (name, email, admin flag, public key), clients (public key,
  validator/admin flag), nodes, roles, environments, data bags and items,
  cookbooks with every version and file.
- Map the whole source into exactly one target organization named by `--org`.
- Keep every existing client public key unchanged, so hosts keep their
  current `client.pem` and only need the new server URL.
- Copy cookbook blobs into a local object-store tree when `--bookshelf-dir` is
  given, MD5-verified.
- Produce Microwave's default organization policy (12 containers, default
  groups, global groups, ACL matrix, per-object ACLs, `_default` environment)
  deterministically, because goiardi has none.
- Be re-runnable: deterministic ids and stable collection order, so repeated
  crawls produce the same snapshot. The import itself needs an empty target.
- Never write to the source.

## Non-goals

- No password hashes. The API does not expose them; `admin` is imported with
  an empty hash and gets a password reset after the migration.
- No policies, policy groups, cookbook artifacts, groups or ACLs from the
  source — the source has none.
- No server-owned secrets (`pivotal.pem`, object-store secret, TLS material).
  Those belong to a fresh Microwave install.
- No shovey, log events, reports, node status or sandboxes.
- No S3/Garage target for blobs; the local object store only.
- No goiardi export-file or SQL backend. The API is the supported path.

## Anti-goals

- A second object model. Everything goes into `StoreSnapshot` and through the
  existing `microwave-ctl migrate-from-legacy` import.
- Silent partial migrations. A crawl that hits an unexpected shape or a
  checksum mismatch fails loudly; a `--dry-run` reports counts without
  writing.
- Treating goiardi as multi-org. One run produces one org; several orgs mean
  several snapshots imported into one target.

## Constraints

- The API is reachable only from one bastion host; the OpenResty vhost answers
  403 from anywhere else. The crawler needs either a SOCKS5 tunnel
  (`--socks5`) or to run on that host.
- Every request is signed with the operator's key. Sign 1.3 works and the
  server ignores `X-Ops-Server-API-Version`, so the existing signer is used
  unchanged.
- `/file_store/*` is unauthenticated and returns absolute URLs; blob fetches
  are unsigned like in `from-api`.
- Cookbook blobs are capped at 64 MiB each and 4 GiB per run (existing
  limits).
- The snapshot envelope stays version 1. `source.kind` stays `legacy-api`;
  a new optional `source.layout = "goiardi"` marks the producer. Unknown
  fields are ignored by older readers.

## Approaches considered

1. **Dedicated `from-goiardi` module and subcommand (chosen).** Shares the
   low-level crawl plumbing with `from-api`, writes the common snapshot, and
   keeps goiardi-specific mapping isolated. No branching in the Chef path.
2. **`from-api --source goiardi`.** One tool and one entry point, but the
   Chef-specific org, group, ACL, key and policy steps would each need a
   mode switch inside a path with a large existing test suite.
3. **Crawl straight into the store.** No snapshot, but it loses dry-run,
   replay and backend parity, and couples the crawler to store traits.

The operator picked 1.

## Design

### Command

```
microwave-migrate from-goiardi \
  --url https://chef.example.com \
  --client-name admin --key admin.pem \
  --org acme --output snapshot.json \
  [--org-full-name "Acme"] \
  [--bookshelf-dir ./bookshelf] \
  [--socks5 127.0.0.1:1080] \
  [--skip-orphan-clients] \
  [--no-client-data-bag-writes] \
  [--dry-run]
```

`--url`, `--client-name`, `--key` and `--output` are required; `--org` is
required and must match `[a-z0-9-]+`. `--dry-run` crawls and prints a count
table without writing the snapshot; it also skips blob GETs and reports only
the referenced-checksum count. `--socks5` accepts `host:port` and builds a
`socks5://` proxy for every request, blob fetches included.

Blob URLs are taken from the manifest. When an entry carries a checksum but
no `url`, or a relative `/file_store/<checksum>`, the crawler derives the
URL from `--url` and fetches it. Relative references are signed like the API
calls, absolute ones unsigned, matching `from-api`; goiardi accepts both.

Add `socks-proxy` to the workspace `ureq` features.

### Module layout

- `crates/microwave-migrate/src/crawl.rs` (new, `pub(crate)`): signed and
  unsigned GET, collection parsing, deterministic ids, key/ACL row helpers,
  blob MD5 verification and atomic local write. Moved out of `from_api`
  without behavior changes; `from_api_test` is the guard.
- `crates/microwave-migrate/src/goiardi.rs` (new): flat crawl, mapping, and
  the policy builder.
- `main.rs`: `FromGoiardi` subcommand and its clap arguments.

### Crawl order

1. `/users` — username, email, `admin`, `public_key`.
2. `/clients` — name, `validator`, `admin`, `public_key`; `node_name` is
   ignored (Microwave has no such column).
3. `/nodes`, `/roles`, `/environments`, `/data` + items, `/cookbooks` +
   versions.
4. Policy synthesis (below), ACL rows last.
5. Blob copy, unless `--dry-run` or no `--bookshelf-dir`.

Lists arrive as name→URL maps; the shared parser already handles that.
Bodies are stored raw, like `from-api`, so cookbook manifests keep their
checksums and node JSON stays byte-identical.

`_default` is taken from the policy, not from the crawl.

### Identity mapping

Ids keep the `from-api` scheme (`synthetic_id("client:<org>:<name>")` and
friends) so both paths produce the same ids for the same objects:

| Source | Row |
| --- | --- |
| every user | `UserRow`, `password_hash: ""`, `hash_type: "bcrypt"`, `admin` copied |
| every client | `ClientRow` with `validator`/`admin`, `ActorKeyRow` with the public key as `default` |
| `admin` | org association, member of `admins` and `users` |
| every client | member of the `clients` group |
| nodes, roles, environments, data bags | rows with raw bodies, `_default` skipped |
| cookbooks | `CookbookRow`, `CookbookVersionRow`, `ChecksumRow`, version/checksum links |

Orphan clients (no node of the same name) are imported by default;
`--skip-orphan-clients` drops them. `chef-validator` and `chef-webui` are
always kept, because new hosts need the validator. The operator currently has
14 orphan clients and one orphan node (`itsjustintv`), which stays.

`chef-validator` and `chef-webui` are imported as ordinary clients.
Microwave does not require the `<org>-validator` naming.

### Policy synthesis

The builder emits exactly the rows `microwave-authz::apply_default_policy`
creates, with deterministic ids:

- one `OrgRow`, `authz_entities` for the org, containers, groups and objects
- 12 containers, the five default groups, the global `server-admins` and
  `<org>_read_access_group`
- the membership graph (`admin`→admins/users, clients→clients,
  read-access→admins/users, `public_key_read_access`→users/clients)
- the full container, group and org ACL matrix
- the `_default` environment and its ACLs
- one object ACL per imported object, copied from its container ACL plus a
  creator grant for `admin`

The `clients` group gets create, read, update and delete on `data`, so
node-written bag items (`chef_data_bag_item`) keep working as on the source.
Node saves need no container grant: the node handler lets a client update and
delete the node named like itself, as upstream does, so `nodes` keeps the
upstream read/create matrix. `--no-client-data-bag-writes` narrows `data` to
read, mirroring `org.client_data_bag_writes = false`.

A test creates a real org through `create_organization` in a temporary store,
exports it, and compares the normalized rows against the builder, so server
policy drift fails the suite. `microwave-authz` becomes a dev-dependency.

### Safety and verification

- Only GET requests; `--dry-run` asserts that in the test harness.
- Every blob is MD5-checked before the atomic write; a mismatch aborts.
- Structural failures abort: unreadable detail body, collection shape,
  unparsable cookbook version, HTTP error. Junk that a valid source can
  plausibly contain is skipped with a warning and listed in a closing skip
  summary: an actor without `public_key`, a data-bag item without `id`, a
  node naming an environment that does not exist (the node is kept with the
  name it had). A run with skips still exits `0` and names them.
- After the crawl, counts per collection and the skip summary are printed;
  `--dry-run` prints them without writing.
- A snapshot with an object but no ACL row, or a requested checksum with no
  blob, is a hard error.
- A goiardi snapshot refuses a non-empty target before writing anything.
- Snapshot written through the existing atomic `write_snapshot`.

### Target preconditions

The target instance must be freshly initialized and empty: no organizations,
one superuser named `pivotal`. The source user is literally `admin`, so a
superuser also named `admin`, or a pre-created target org, collides on the
`users`, `orgs.name`, `global_groups.name` and `clients` unique constraints;
the snapshot importer reconciles only by id.

There is no merge path, and it is enforced, not just documented. A snapshot
with `source.layout = "goiardi"` makes the import refuse before the first
write when the target is not empty. Empty means zero organizations and at
most one user; the message names the counts and says to use a fresh data
directory or database. Both backends implement the same check, and
`microwave-migrate import`, `microwave-migrate import --database-url`,
`microwave-ctl migrate-from-legacy` all run it. Non-goiardi snapshots keep
their current merging behavior.

### Import preflight

`SqliteStore` and `PostgresStore` gain an `import_target_counts()` query
returning `{ orgs, users }`. `import_database_with_compression` and
`import_postgres_with_compression` read the snapshot first, and when
`source.layout == "goiardi"` run the check before opening or writing the
target. Failure is `MigrateError::TargetNotEmpty` naming both counts.

### Post-migration steps (docs)

`microwave-ctl migrate-from-legacy --input snapshot.json --yes`, then
`microwave-ctl password admin`, then `microwave-ctl reindex`, then the hosts'
`client.rb`/`knife.rb` move to the new URL. Clients keep working with their
existing keys; no re-registration.

## Risks

- **Policy drift** between `org.rs` and the builder — pinned by the
  comparison test.
- **Raw bodies** may carry goiardi-only fields (`orgname`). Microwave's
  snapshot import stores them verbatim; a follow-up can normalize if pedant
  or the client complains. Node bodies are the realistic case and Chef
  ignores unknown keys.
- **Absolute blob URLs** bypass the proxy when the host name is unresolvable
  from the workstation; `--socks5` sends them through the tunnel anyway. If
  a URL points somewhere else, the fetch fails loudly.
- **SOCKS dependency** adds a ureq feature; no runtime dependency on `ssh`.
- **A 403 instead of 401** is reported as a plain API error; the docs name
  the `--socks5` remedy.

## Tests

- Unit: list-map parsing, key extraction, `_default` skip, orphan handling,
  policy rows, id determinism, `--dry-run` writes nothing.
- Integration: axum fixture serving the flat paths (clients map, nodes, roles,
  environment, bags, cookbook with `file_store` URL); crawl, import into a
  temporary SQLite store, then assert client keys, ACLs for `admin` and a
  client, blob MD5, and no duplicate rows on a second import.
- CLI: required flags, bad org name, `--skip-orphan-clients`, `--socks5`
  parse errors, `--help`.
- Preflight: goiardi snapshot into an empty SQLite target succeeds, into a
  target with an org or a second user fails with the count message; the same
  snapshot imported twice fails the second time; a non-goiardi snapshot still
  merges. Dual-backend coverage for the preflight query.
- Policy comparison test described above.

## Documentation

- `docs/deployment.md` § 11: new "Migrating from goiardi" subsection with the
  command, the SOCKS note, the password reset, the missing password hashes
  and the reindex step.
- `crates/microwave-migrate/README.md`: one paragraph plus an example.
- `README.md` migration section: name the goiardi path.
- `docs/claims.toml`: add `gap-goiardi-password-hashes` while the empty-hash
  limitation exists, with `docs/deployment.md` and
  `crates/microwave-migrate/README.md` as surfaces, and re-render
  `docs/remaining-work.md` and the README block. Drop the claim if the import
  ever carries hashes. The homepage stays untouched; the existing
  `gap-from-api-credentials` already carries the same limitation for the Chef
  path.

## Open items left to the builder

- Exact ACL grants for each object type follow the container ACL verbatim;
  the builder helper decides whether object ACLs also need a creator grant
  for clients acting as objects.
- Error wording and whether the skip summary belongs on stdout or stderr.
- Whether the count table belongs on stdout or stderr.
- Capture a handful of real goiardi responses through the tunnel before
  writing the mapper, and keep them as fixtures.
