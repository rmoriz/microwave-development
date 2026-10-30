# oc-chef-pedant Compatibility Report

**Date:** 2026-10-30
**Server under test:** `microwave-server 0.1.0`, binary built from `9966994`
(`fix(e2e): upload the postgresql wrapper and fix the workstation env`), run on `main` at
`5c7ffff`; no server source changed between the build and the run
**Pedant:** vendored `chef-server/oc-chef-pedant` (gemspec version `2.2.0`)
**Harness:** `scripts/run-pedant.sh` + `scripts/pedant-bootstrap.rb`

## Result

Every core tag passes: **652 passed / 0 failed / 8 pending out of 660 examples**. The pending
examples are pedant's own skips in `clients` (3) and `nodes` (5), not server gaps.

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

The binary was built once with `cargo build --locked --bin microwave-server --bin microwave-ctl`
and reused with `MW_SKIP_BUILD=1`, so every tag ran against the same server build.

## Command line

```sh
MW_SKIP_BUILD=1 PEDANT_ARTIFACTS=/tmp/mw-pedant-current \
  scripts/run-pedant.sh strict
```

`strict` expands to `core`, `extended`, and `extra`; the core part is
`headers users organizations clients nodes`. Each tag gets its own fresh server, data directory,
and seeded actors: pedant deletes the requestor clients in its after-suite hook even when they were
pre-existing, so a shared instance produces bogus 401s after the first tag. Per tag the script
starts `target/debug/microwave-server serve` on a loopback ephemeral port with a temporary SQLite
data directory, waits for `/_status`, seeds actors, writes a pedant config, then runs:

```sh
cd chef-server/oc-chef-pedant
SUPERUSER_KEY=<data-dir>/pivotal.pem \
  bundle exec ./bin/oc-chef-pedant \
    -c <artifacts>/<tag>/pedant_config.rb \
    -L <artifacts>/<tag>/http-traffic.log \
    --focus <tag>
```

Exit status of the run was **0**. Logs, generated keys, config, and HTTP traffic captures are in
`/tmp/mw-pedant-current`.

## Counts per tag

RSpec's own summary lines, after stripping ANSI colour codes:

| Tag | Examples | Passed | Failed | Pending |
| --- | ---: | ---: | ---: | ---: |
| `headers` | 7 | 7 | 0 | 0 |
| `users` | 165 | 165 | 0 | 0 |
| `organizations` | 15 | 15 | 0 | 0 |
| `clients` | 142 | 139 | 0 | 3 |
| `nodes` | 331 | 326 | 0 | 5 |
| **Total** | **660** | **652** | **0** | **8** |

Raw summary lines:

```text
headers        7 examples, 0 failures
users          165 examples, 0 failures
organizations  15 examples, 0 failures
clients        142 examples, 0 failures, 3 pending
nodes          331 examples, 0 failures, 5 pending
```

## Harness notes and still-true limitations

- `scripts/pedant-bootstrap.rb` creates the org, validator, clients, and users through the public
  signed API, then seeds the default groups through the `/organizations/:org/groups` API. It writes
  a generated `pedant_config.rb` with `create_me: false` for all requestors.
- Pedant's requestor setup drops the `admin`/`associate` flags for pre-existing users, so the org
  admin user is left to pedant's own `create_me`. The other users and clients are pre-created so
  the group membership is in place before pedant starts.
- The org-creation response carries the validator name and private key, which the bootstrap reuses
  as the validator key. When that key is absent the bootstrap creates the validator itself.
- Each tag runs against a separate server and data directory, as described above.
- `headers` needs a vendored harness patch: the nginx-default-error example called
  `request_url.sub!("https", "http")`, and `sub!` returns `nil` for an already plain-HTTP URL, so
  the example signed a nil URL. It now passes `request_url` unchanged. The patch is listed under
  "Vendored harness-only patches" at the top of [`scripts/run-pedant.sh`](../scripts/run-pedant.sh)
  and must be re-applied on every re-vendor.
- Pending examples are pedant's, not harness failures: 3 in `clients`, 5 in `nodes`.
- The harness restores the vendored `Gemfile.lock` after each run. Bundler adds the local
  `arm64-darwin-25` platform and drops the `rack` floor because the omnibus `safe_versions.rb` file
  is not present in this checkout; committing that host-specific rewrite is undesirable.

The extended, extra, and harness-specific notes are in
[`docs/pedant-extended-report.md`](pedant-extended-report.md).
