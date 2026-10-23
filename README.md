# Microwave Server

A Rust reimplementation of the Chef Infra Server API, designed as a drop-in replacement for existing clients and operations tooling. This is an independent project, not affiliated with or endorsed by Progress Software Corporation.

The repository currently contains the design work and the vendored upstream sources used as the reference implementation.

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored upstream server `15.10.127` source snapshot (commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`, branch `main`). |
| `chef/` | Vendored upstream client `19.4.47` source snapshot (commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`, branch `main`). |
| `docs/superpowers/specs/2026-10-24-microwave-server-design.md` | Architecture and design specification. |
| `TODO.md` | Master implementation checklist. |

## Naming and Trademarks

"Chef", "Chef Infra", "Chef Infra Server", and "Chef Infra Client" are trademarks of Progress Software Corporation. They appear in this repository only to identify the upstream software and the compatibility surface.

This project's own crates, binaries, modules, services, and public types use the `microwave-*` prefix or domain-neutral names. The `chef-server-ctl` command name is retained only as a compatibility alias for `microwave-ctl`. Protocol-required identifiers such as the `X-Chef-Version` header, the `chef_type` JSON field, `/etc/opscode/chef-server.rb`, and `CHEF_SECRETS_DATA` are preserved because changing them would break compatibility.

See section 1.1 of the design specification for the full policy.

## Vendored Git Histories

The original upstream `.git` directories are preserved locally at:

- `.git/vendor/chef-server.git`
- `.git/vendor/chef.git`

They are inside the repository's `.git` directory and therefore not tracked or included in clones. To inspect upstream history after this import:

```sh
git --git-dir=.git/vendor/chef-server.git --work-tree=chef-server log --oneline -10
git --git-dir=.git/vendor/chef.git --work-tree=chef log --oneline -10
```

To fetch updates from upstream later:

```sh
git --git-dir=.git/vendor/chef-server.git --work-tree=chef-server fetch origin
git --git-dir=.git/vendor/chef.git --work-tree=chef fetch origin
```

## Status

Design phase complete. Implementation has not started. See `TODO.md` for the phased plan.
