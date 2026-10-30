# Microwave Server

![A glittering rainbow microwave under neon light](docs/images/microwave-hero.png)

An independent, open-source Chef Infra Server API implementation in Rust, designed as a drop-in replacement
for existing `chef-client` fleets and operations tooling: same API, same request signing, same CLI
surface.

**Website:** <https://rmoriz.github.io/microwave-development/> · **Documentation:**
<https://rmoriz.github.io/microwave-development/docs/>

- [Why](#why)
- [Try it in five minutes](#try-it-in-five-minutes)
- [What works](#what-works)
- [Known gaps](#known-gaps)
- [Migrate from Chef Infra Server](#migrate-from-chef-infra-server)
- [Pedant compatibility](#pedant-compatibility)
- [Layout](#layout)

## Why

- **Drop-in** — it implements the Chef Infra Server API, request signing, and authorization that
  existing `chef-client` runs and operations tooling already speak.
- **Proven with a real client** — a stock `chef-client` converges against it end to end: the
  harness starts a throwaway server, uploads a cookbook, runs the client, and checks the marker
  file the recipe wrote.
- **Pedant-checked** — the full `scripts/run-pedant.sh strict` set passes; see
  [Pedant compatibility](#pedant-compatibility).
- **Independent** — not affiliated with or endorsed by Progress, so compatibility follows the API
  rather than a vendor roadmap.

## Try it in five minutes

Start the server:

```sh
cargo run -p microwave-server -- serve --check
cargo run -p microwave-server -- serve
```

`--check` validates the configuration and initializes the database without binding a listener. The
first run writes the superuser credentials to `./data/pivotal.pem`, `./data/pivotal.password` and
`./data/objectstore.secret`. Use `MW_PATHS__DATA_DIR` to choose another data directory. TLS, SMTP,
Docker, systemd, backup and restore, and the external backends are in the
[deployment guide](https://rmoriz.github.io/microwave-development/docs/deployment/).

To prove that a stock `chef-client` converges against the server end to end, run the harness:

```sh
scripts/run-chef-client-e2e.sh
```

It needs `curl`, a `chef-client` 18+ binary, and Ruby with `mixlib-authentication`. Point
`MW_E2E_CHEF_CLIENT` and `MW_E2E_RUBY` at them when they are not on `PATH`; a missing piece exits `2`
and names itself. The harness builds a throwaway server on a free loopback port with a temporary data
directory, so it leaves anything you already run untouched. Success is exit `0` with
`chef-client e2e: PASS`, the converge recipe's `marker.txt` left in the artifact directory, and the
saved node the harness verifies. Set `MW_E2E_COMPAT=1` to grant the default environment's read ACE
and synthesize the v2 `all_files` manifest for a server that returns neither.

## What works

Everything below points at the file in this repository that backs it; nothing here is a roadmap
promise.

| Surface | Proof |
| --- | --- |
| Signed HTTP API — `/_status`, `/_stats`, `/server_api_version`, `/license`, users, organizations, clients, keys, nodes, roles, environments, data bags, sandboxes, cookbooks, policies, ACLs, `/validate/*`, search, data collector, compliance proxy | [`http_parity_test.rs`](crates/microwave-api/tests/http_parity_test.rs), [`data_collector_test.rs`](crates/microwave-api/tests/data_collector_test.rs), [`compliance_proxy_test.rs`](crates/microwave-api/tests/compliance_proxy_test.rs) |
| `X-Ops-Sign` signing — 1.0/1.1/1.3 canonicalization, RSA verification, key reassembly | [`docs/pedant-report.md`](docs/pedant-report.md) |
| Both metadata backends — SQLite and PostgreSQL behind one store contract | [`crates/microwave-store/tests/dual_backend.rs`](crates/microwave-store/tests/dual_backend.rs) |
| Operations CLI `microwave-ctl` — user/org administration, key rotation, backup/restore, reindex, diagnostics | [`TODO.md`](TODO.md) |
| Migration — snapshot import and `knife ec backup` import | [`docs/deployment.md`](docs/deployment.md#10-migration-from-a-legacy-server) |
| MCP endpoint — `experimental`, off by default, writes off with `writes = false` | [`docs/mcp.md`](docs/mcp.md) |

`/validate/*` and `_identifiers` are internal upstream APIs; the API surface, the authorization
model, and the dual-backend suites are described in the [parity page](https://rmoriz.github.io/microwave-development/docs/http-parity/).

## Known gaps

- No `chef-server.rb` importer. The legacy Ruby configuration is translated by hand; the mapping table is in [`docs/deployment.md`](docs/deployment.md#migrating-configuration-from-etcopscodechef-serverrb).
- Multi-node HA and automatic failover are not validated. `microwave-ctl ha-status` reports a single node, and `docker-compose.ha.yml` is an evaluation topology rather than a replica set.
- Embedded search is a relational scan, not an inverted index. It is correct for small and medium organizations and needs no extra service, but it scales linearly; move to OpenSearch or Elasticsearch before a large catalog makes queries hurt.

The open inventory below is generated from `docs/claims.toml` and includes the three above:

<!-- generated:remaining-work:start -->
Open gaps: 17 (migration: 6, mail: 1, search: 3, ha: 2, auth: 2, tooling: 3). By area: [migration](docs/remaining-work.md#area-migration), [mail](docs/remaining-work.md#area-mail), [search](docs/remaining-work.md#area-search), [ha](docs/remaining-work.md#area-ha), [auth](docs/remaining-work.md#area-auth), [tooling](docs/remaining-work.md#area-tooling).
<!-- generated:remaining-work:end -->

## Migrate from Chef Infra Server

The supported route is a snapshot copy: `microwave-migrate from-api` (or `export`) writes a
versioned JSON snapshot, and `microwave-ctl migrate-from-legacy --input <snapshot>` imports it into
the configured SQLite or PostgreSQL database. A `knife ec backup` directory imports just as
directly with `import-knife-backup`.

Neither path is lossless: no cookbook blobs without `--bookshelf-dir`, no password hashes or
`admin` flags, and no client or user private keys. Run both to get closer — the crawl first for
policies and policy groups, the knife backup last for hashes and admin flags. The full runbook, the
limits, and the `chef-server.rb` translation table are in
[deployment § 10](https://rmoriz.github.io/microwave-development/docs/deployment/#10-migration-from-a-legacy-server).

## Pedant compatibility

A full `scripts/run-pedant.sh strict` run on 2026-10-30 passes the core set with
**652 passed / 0 failed / 8 pending of 660**; the 8 pending examples are pedant's own skips.
Extended suites: **3,284 passed / 0 failed / 7 pending out of 3,291**. All twelve extended tags
are green.

Records: [`docs/pedant-report.md`](docs/pedant-report.md) and
[`docs/pedant-extended-report.md`](docs/pedant-extended-report.md).

## Layout

| Path | Contents |
|---|---|
| `chef-server/` | Vendored upstream server `15.10.127` source snapshot (commit `c38d169f083d064b8c542d912d072d8b88cf4c8c`). |
| `chef/` | Vendored upstream client `19.4.47` source snapshot (commit `46354347bc125edc98c5f080ca2592d55ec0e4bc`). |
| `crates/` | Rust workspace crates. |
| `bin/` | Server and compatibility binaries. |
| `.github/workflows/` | Pull-request, PostgreSQL, and nightly pedant gates. |
| `docs/superpowers/specs/` | Architecture and design specifications. |
| `packaging/` | systemd unit, Debian package metadata, example configuration, Garage config/bootstrap for Compose. |
| `TODO.md` | Master implementation checklist. |

The original upstream `.git` histories are preserved locally at `.git/vendor/chef-server.git` and
`.git/vendor/chef.git`. They sit inside the repository's `.git` directory and are therefore not
tracked or included in clones; inspect them with
`git --git-dir=.git/vendor/chef-server.git --work-tree=chef-server log`.

## Naming and Trademarks

"Chef", "Chef Infra", "Chef Infra Server", and "Chef Infra Client" are trademarks of Progress Software Corporation. They appear in this repository only to identify the upstream software and the compatibility surface. This project's own crates, binaries, modules, services, and public types use the `microwave-*` prefix or domain-neutral names; `chef-server-ctl` is retained only as a compatibility alias for `microwave-ctl`.

## License

Microwave Server is licensed under the GNU Affero General Public License, version 3 only (AGPL-3.0-only). See [LICENSE](LICENSE) for the full text. Vendored upstream sources under `chef/` and `chef-server/` keep their original Apache-2.0 licensing.
