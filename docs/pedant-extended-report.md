# Extended oc-chef-pedant Compatibility Report

**Date:** 2026-10-30
**Server under test:** `microwave-server 0.1.0`, binary built from `9966994`
(`fix(e2e): upload the postgresql wrapper and fix the workstation env`), run on `main` at
`5c7ffff`; no server source changed between the build and the run
**Pedant:** vendored `chef-server/oc-chef-pedant` (gemspec version `2.2.0`)
**Harness:** `scripts/run-pedant.sh` + `scripts/pedant-bootstrap.rb` +
`scripts/pedant-solr-stub.rb`

This report covers the eleven extended suites and the two contract suites that sit outside the
core set. The core record is [`docs/pedant-report.md`](pedant-report.md).

## Result

All twelve extended tags pass: **3,284 passed, 0 failed, 7 pending out of 3,291 examples**. The
contract suites `environments` and `roles` run 610 examples (587 passed, 23 pending) and 78
examples (78 passed), both with zero failures. Every failure is gone; the pending examples are
pedant's own skips.

## Environment

| Component | Version / value |
| --- | --- |
| OS | macOS 26.6.2 (25G83), arm64 |
| Ruby | `ruby 3.4.12 (2026-10-23 revision 366f5fdc00) +PRISM [arm64-darwin25]` |
| Bundler | `2.6.9` |
| Rust | `rustc 1.99.0 (b940084d7 2026-09-28)` |
| Cargo | `cargo 1.99.0 (5f94df478 2026-08-27)` |
| Pedant API version header | `X-Ops-Server-API-Version: 0` |
| Pedant `X-Chef-Version` | `18.10.17` (hard-coded in `lib/pedant/request.rb`) |

The server binary was built once with `cargo build --locked --bin microwave-server --bin
microwave-ctl` and reused with `MW_SKIP_BUILD=1`. Every tag ran against its own fresh server, data
directory, and seeded actors.

## Command

The extended tags ran as part of one full `strict` run (core, then extended, then extra):

```sh
MW_SKIP_BUILD=1 PEDANT_ARTIFACTS=/tmp/mw-pedant-current \
  scripts/run-pedant.sh strict
```

`strict` is `core + extended + extra`, so the same invocation also produced the core record. The
tags can be run on their own:

```sh
scripts/run-pedant.sh extended   # the eleven suites in this report
scripts/run-pedant.sh extra      # environments roles
PEDANT_TAGS=extended scripts/run-pedant.sh
```

Friendly aliases map to Pedant's RSpec metadata: `acls` → `acl`, `oauth` → `oc_id`,
`versioned_behaviors` → `api_v1`. Per tag the script starts `target/debug/microwave-server serve`
on a loopback ephemeral port with a temporary SQLite data directory, waits for `/_status`, seeds
actors, writes a pedant config, starts the Solr commit stub, then runs:

```sh
cd chef-server/oc-chef-pedant
SUPERUSER_KEY=<data-dir>/pivotal.pem \
  bundle exec ./bin/oc-chef-pedant \
    -c <artifacts>/<tag>/pedant_config.rb \
    -L <artifacts>/<tag>/http-traffic.log \
    --focus <tag>
```

Artifacts (logs, HTTP traffic, generated keys/config) are in `/tmp/mw-pedant-current`.

## Per-tag counts

RSpec summary lines, after stripping ANSI colour codes. "Passed" is
`examples - failures - pending`.

| Tag | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| `keys` | 312 | 312 | 0 | 0 |
| `acls` | 1,640 | 1,640 | 0 | 0 |
| `data_bags` | 89 | 89 | 0 | 0 |
| `cookbooks` | 531 | 531 | 0 | 0 |
| `sandboxes` | 14 | 14 | 0 | 0 |
| `search` | 501 | 500 | 0 | 1 |
| `policies` | 93 | 93 | 0 | 0 |
| `oauth` | 13 | 8 | 0 | 5 |
| `license` | 9 | 9 | 0 | 0 |
| `depsolver` | 20 | 20 | 0 | 0 |
| `versioned_behaviors` | 65 | 64 | 0 | 1 |
| `compliance_proxy` | 4 | 4 | 0 | 0 |
| **Total** | **3,291** | **3,284** | **0** | **7** |

Raw summary lines:

```text
keys                 312 examples, 0 failures
acls                 1640 examples, 0 failures
data_bags            89 examples, 0 failures
cookbooks            531 examples, 0 failures
sandboxes            14 examples, 0 failures
search               501 examples, 0 failures, 1 pending
policies             93 examples, 0 failures
oauth                13 examples, 0 failures, 5 pending
license              9 examples, 0 failures
depsolver            20 examples, 0 failures
versioned_behaviors  65 examples, 0 failures, 1 pending
compliance_proxy     4 examples, 0 failures
```

Notes:

- The `cookbooks` tag also pulls `environments/cookbooks_spec.rb` and
  `environments/single_cookbook_spec.rb` because those files carry the `:cookbooks` tag.
- The `versioned_behaviors` alias focuses `:api_v1`, which also matches `principal_spec.rb`.
- Pending examples are Pedant's own skips: 5 in `oauth` (unconfigured OAuth application / email),
  1 in `search` (index deletion that needs a real Solr), and 1 in `versioned_behaviors`.

## Contract suites

`environments` and `roles` sit outside `core` and `extended` and run in the same `strict` set:

| Tag | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| `environments` | 610 | 587 | 0 | 23 |
| `roles` | 78 | 78 | 0 | 0 |
| **Total** | **688** | **665** | **0** | **23** |

The `environments` pending examples are pedant's own skips. Both suites run against the same
seeded actors as the rest.

## Harness changes that make these suites meaningful

1. **Readiness timeout.** A fresh SQLite database can take more than ten seconds to migrate on a
   cold page cache. The readiness loop allows 60 seconds.
2. **Tag sets and aliases.** `core`, `extended`, `all`, `extra`, and `strict` expansions plus the
   aliases above.
3. **No-op Solr commit stub (`scripts/pedant-solr-stub.rb`).** Pedant's `word_break_spec.rb` calls
   `force_solr_commit` unconditionally and `search_util` posts it to the configured `search_server`.
   The harness starts a tiny 200-OK HTTP stub, writes its URL as `search_server`, and sets
   `direct_solr_query_sleep_time 0`. The real search API is still what is tested; the stub only
   absorbs the external Solr commit call.
4. **Auxiliary org for the ACL suite.** `PEDANT_AUX_ORG=1` makes the bootstrap switch
   `org.create_me` on in a `before(:suite)` hook, so the ACL suite can create and delete the second
   organization it uses for non-local group coverage.
5. **No extra seed data.** See [Seeding](#seeding).

## Seeding

The extended suites need no seed data beyond the standard bootstrap:

- `cookbooks`, `depsolver`, and `sandboxes` upload their own cookbooks, sandboxes, and checksums
  through the API.
- `policies` creates its own policy names, revisions, and groups.
- `data_bags` creates and deletes its own bags and items, including the "with no data bags" setup.
- `keys` creates its own clients, users, orgs, and keys.
- `acl` creates its own objects (roles, environments, nodes, cookbooks, data bags, policies,
  containers).
- `search` creates its own nodes, roles, environments, data bags, and clients, then searches the
  embedded index.
- `license`, `oauth`, `versioned_behaviors`, and `depsolver` use the seeded actors or the objects
  they create themselves.

Pre-seeding a cookbook, data bag, or policy would break empty-state examples (`GET /cookbooks`
without existing cookbooks, "with no data bags", "when no policies exist", and the `/search`
built-in-index list). The extended tags add no direct database writes.

## Known Pedant quirks and still-true harness limitations

1. **Search `force_solr_commit` assumes a Solr server.** The stub absorbs it. The
   `search_index_deletion` examples that genuinely need a real Solr stay pending and are only
   meaningful for an external-index deployment.
2. **`license_spec.rb` hard-codes `MAX_NODE_COUNT = 25`** and an `https://` `upgrade_url` regex,
   with a TODO to move it to config. A different Microwave node default has to be reconciled
   deliberately against that spec.
3. **`--focus api_v1` also selects `principal_spec.rb`.** The `versioned_behaviors` count therefore
   mixes `server_api_v1_spec.rb` with principal access-control examples.
4. **Key specs send `"admin": "true"` as a string.** This legacy Pedant quirk is worth supporting for
   old clients and is not a key-endpoint defect.
5. **Exact `Allow` headers include `HEAD`.** Axum adds `HEAD` automatically, so `GET,HEAD,POST`
   differs from Pedant's expected `GET, POST`. This is a framework-level shape difference.
6. **Cookbook validation failures cascade.** `create_spec.rb` and `update_spec.rb` reuse fixed names
   such as `cookbook_name`, so one missing validation produces a later `409` and multiplies the
   count.
7. **ACL and key runtime.** `acls` is 1,640 examples and took about 7 minutes in this run; `keys`
   took about 6.5 minutes because it creates a fresh org and many keys. They are the long pole.
8. **Harness edit safety.** `run-pedant.sh` reads itself while running; do not edit it during a run.
9. **Each tag needs its own server.** Pedant deletes requestor clients during cleanup even when
   they were pre-existing, so a shared instance produces spurious 401s after the first tag.
10. **`Gemfile.lock` is restored after each run.** Bundler adds the local `arm64-darwin-25` platform
    and drops the `rack` floor because the omnibus `safe_versions.rb` is missing from this checkout.
