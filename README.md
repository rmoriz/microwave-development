# Microwave Server

A Rust reimplementation of Chef Infra Server, designed as a drop-in replacement for the Chef Infra API and the surrounding product surfaces (`oc-id`, `chef-server-ctl`, data collector, license, and migration tooling).

This repository currently contains the design work and vendored upstream sources used as the reference implementation.

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored Chef Infra Server `15.10.127` source snapshot (upstream commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`, branch `main`). |
| `chef/` | Vendored Chef Infra Client `19.4.47` source snapshot (upstream commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`, branch `main`). |
| `docs/superpowers/specs/` | Architecture and design specifications. |
| `TODO.md` | Master implementation checklist. |

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
