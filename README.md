# Microwave Server

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing clients and operations tooling. This is an independent project, not affiliated with or endorsed by Progress Software Corporation.

## Status

The repository contains a working vertical slice:

- **Configuration** — TOML plus `MW_*` environment overrides.
- **Authentication** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing/verification, key reassembly, and legacy password hash compatibility.
- **Storage** — SQLite schema for identity, ACLs, containers, groups, nodes, roles, environments, data bags, sandboxes, and cookbooks.
- **Authorization** — recursive ACL/group checks, cycle rejection, the default organization policy, and ACL read/write endpoints with granular `users`/`clients` responses.
- **HTTP API** — `/_status`, `/server_api_version`, `/license`, users, organizations, clients, principals, key management, organization memberships/invitations, nodes, environments, roles, data bags, sandboxes, bookshelf uploads, cookbooks, cookbook resolution, policyfiles/policy groups, ACL endpoints, and search (node/role/environment/client/data-bag indexes with a query subset and partial search).
- **Operations CLI** — `microwave-ctl` with a `chef-server-ctl` compatibility binary: user/org administration, org membership, password reset, status, local backup/restore, `migrate-from-legacy` snapshot import, and a clear not-implemented response for reindex.
- **Migration** — `microwave-migrate` exports a versioned JSON snapshot of the local store (users with password hashes, organizations, clients, keys, authz ACLs, nodes, roles, environments, data bags, checksums, cookbooks, policies, and OAuth applications) and imports it with original ids, authz ids, timestamps, and keys intact. The live legacy API crawler (`from-api`) is deferred and reports a clear not-implemented error.
- **Object storage** — local content-addressed store with signed upload/download URLs.
- **Server** — `microwave-server serve` bootstraps a `pivotal` superuser and listens on the configured address.

Testing and formatting are green:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Run Locally

```sh
cargo run -p microwave-server -- serve --check
cargo run -p microwave-server -- serve
```

The first run creates `./data/pivotal.pem` (superuser private key), `./data/pivotal.password`, and `./data/objectstore.secret`. Use `MW_PATHS__DATA_DIR` to choose another data directory.

## Not Yet Implemented

- `oc-id` OAuth and the live legacy API crawler (`microwave-migrate from-api`). Local snapshot export/import and `microwave-ctl migrate-from-legacy` are available.
- The remaining `chef-server-ctl` command surface (key rotation, secrets, maintenance, HA, pedant runner, full tar backup format).
- PostgreSQL, OpenSearch, and S3/Garage backends.
- Full dependency-constraint solving; the current resolver selects the latest version per cookbook.
- S3 SigV4 compatibility; the built-in local object store uses HMAC-signed URLs.
- A full `oc-chef-pedant` pass and end-to-end `chef-client` converge verification.

See `docs/superpowers/specs/2026-10-24-microwave-server-design.md` and `TODO.md` for the full target and remaining work.

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored upstream server `15.10.127` source snapshot (commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`). |
| `chef/` | Vendored upstream client `19.4.47` source snapshot (commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`). |
| `crates/` | Rust workspace crates. |
| `bin/` | Server and compatibility binaries. |
| `docs/superpowers/specs/` | Architecture and design specifications. |
| `TODO.md` | Master implementation checklist. |

## Naming and Trademarks

"Chef", "Chef Infra", "Chef Infra Server", and "Chef Infra Client" are trademarks of Progress Software Corporation. They appear in this repository only to identify the upstream software and the compatibility surface.

This project's own crates, binaries, modules, services, and public types use the `microwave-*` prefix or domain-neutral names. The `chef-server-ctl` command name is retained only as a compatibility alias for `microwave-ctl`. Protocol-required identifiers such as the `X-Chef-Version` header, the `chef_type` JSON field, `/etc/opscode/chef-server.rb`, and `CHEF_SECRETS_DATA` are preserved because changing them would break compatibility.

## Vendored Git Histories

The original upstream `.git` directories are preserved locally at:

- `.git/vendor/chef-server.git`
- `.git/vendor/chef.git`

They are inside the repository's `.git` directory and therefore not tracked or included in clones. To inspect upstream history after this import:

```sh
git --git-dir=.git/vendor/chef-server.git --work-tree=chef-server log --oneline -10
git --git-dir=.git/vendor/chef.git --work-tree=chef log --oneline -10
```
