# Microwave Server

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing clients and operations tooling. This is an independent project, not affiliated with or endorsed by Progress Software Corporation.

## Status

The repository contains a working vertical slice:

- **Configuration** — TOML plus `MW_*` environment overrides.
- **Authentication** — `X-Ops-Sign` 1.0/1.1/1.3 canonicalization, RSA signing/verification, key reassembly, and legacy password hash compatibility.
- **Storage** — SQLite schema for identity, ACLs, containers, groups, nodes, roles, environments, data bags, sandboxes, and cookbooks.
- **Authorization** — recursive ACL/group checks, cycle rejection, and the default organization policy.
- **HTTP API** — `/_status`, `/server_api_version`, `/license`, users, organizations, clients, principals, nodes, environments, roles, data bags, sandboxes, bookshelf uploads, cookbooks, and cookbook resolution.
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

- Search endpoints and the embedded search index.
- Key-management, ACL, organization-membership/invitation, and policyfile endpoints.
- `oc-id` OAuth, `chef-server-ctl`, and the migration tool.
- PostgreSQL, OpenSearch, and S3/Garage backends.
- Full dependency-constraint solving; the current resolver selects the latest version per cookbook.
- S3 SigV4 compatibility; the built-in local object store uses HMAC-signed URLs.

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
