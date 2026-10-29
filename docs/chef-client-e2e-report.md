# chef-client end-to-end converge report

Date: 2026-10-24
Branch: `subagent/e2e`
Server: `microwave-server` at `b707c80` (`merge: extended pedant coverage`)
Harness: `scripts/run-chef-client-e2e.sh`
Bootstrap: `scripts/chef-client-e2e-bootstrap.rb`

> **Post-merge status (2026-10-24):** the two gaps below are fixed. A strict run with `MW_E2E_COMPAT=0 MW_E2E_SYNTH_V2=0` now **PASSES**: `environment_read_status=200`, `environment_acl_granted=false`, `v2_solve_status=200`, `v2_all_files=true`, `v2_synthesized=false`, and validator registration succeeds. The bootstrap now sends the upstream-required `name: <cookbook>-<version>`. The historical pre-fix report below is retained for context.

> **Default flipped (2026-10-29):** the harness now runs stock by default
> (`MW_E2E_COMPAT=0`, stock behaviour as shipped by chef-server) and
> `MW_E2E_COMPAT=1` re-enables both provisioning steps. The table below records
> the 2026-10-24 run under the then-default compatibility mode.

## Verdict

A real `chef-client` converge succeeds end-to-end against `microwave-server`
**only after two harness provisioning steps that compensate for missing server
behavior**. No server code was changed, no client code was patched, and no
response was stubbed.

| Mode | Command | Result |
| --- | --- | --- |
| Default / compatibility | `scripts/run-chef-client-e2e.sh` | **PASS** — cookbook downloaded, recipe converged, marker written, node and client saved |
| Stock server | `MW_E2E_COMPAT=0 scripts/run-chef-client-e2e.sh` | **FAIL** — `403 missing permission` reading the default environment |
| ACL granted, stock v2 client | `MW_E2E_SYNTH_V2=0 scripts/run-chef-client-e2e.sh` | **FAIL** — `NoMethodError: undefined method 'inject' for nil` parsing the API v2 cookbook manifest |

The default run grants the `clients` group read access to the default
environment and synthesizes an API v2 `all_files` manifest from the server's
own v0 response. Both actions are logged as server gaps and are described
below. Everything else is the real client talking to the real server.

## Environment

| Component | Version |
| --- | --- |
| Host | macOS 26.6.2, arm64 |
| Ruby | `ruby 4.0.9 (2026-10-23 revision 3e93b23afc) +PRISM [arm64-darwin25]` (asdf) |
| Chef Infra Client | `18.7.10` (asdf shim at `/Users/rmoriz/.asdf/shims/chef-client`) |
| Ohai | `18.2.6` |
| mixlib-authentication | `3.0.10` |
| Rust | `rustc 1.99.0 (b940084d7 2026-09-28)` |
| Cargo | `cargo 1.99.0 (5f94df478 2026-08-27)` |
| Vendored chef checkout | `19.4.47` (not used) |

### Why the installed client was used instead of the vendored checkout

The vendored checkout is `chef/` at `19.4.47`. Its `Gemfile` points at git
sources for `ohai`, `cheffish`, and `rest-client`, plus the local `chef`,
`chef-bin`, `chef-config`, and `chef-utils` paths. A bundle is not installed:

```text
$ cd chef
$ BUNDLE_PATH=/private/.../mw-e2e-bundle bundle check
Bundler can't satisfy your Gemfile's dependencies.
Install missing gems with `bundle install`.
$ echo $?
1
```

Installing that bundle would fetch multiple git repositories and build native
extensions, which is substantially heavier than the already-installed
`chef-client` 18.7.10. The harness therefore prefers an installed
`chef-client` by default and supports the vendored checkout as a fallback:

- `MW_E2E_CHEF_CLIENT=/path/to/chef-client` selects an explicit binary.
- `MW_E2E_PREFER_VENDORED=1` prefers `chef/` **if** `bundle check` already
  succeeds with `MW_E2E_BUNDLE_PATH`.
- Otherwise the harness looks for `chef-client` on `PATH`, then falls back to
  the vendored bundle, and fails with exit code `2` if neither is available.

The exact installed-client invocation used by the harness is:

```sh
CHEF_LICENSE=accept-no-persist /Users/rmoriz/.asdf/shims/chef-client \
  -c "$ARTIFACTS/client.rb" \
  -j "$ARTIFACTS/node.json" \
  --no-color
```

## Exact commands

Worktree setup:

```sh
git -C /Users/rmoriz/Sync/Projekte/microwave-server worktree add \
  /private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e \
  -b subagent/e2e main
```

Build:

```sh
cd /private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e
cargo build --locked --bin microwave-server --bin microwave-ctl
```

Successful default run:

```sh
MW_SKIP_BUILD=1 \
MW_E2E_ARTIFACTS=/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e-run4 \
scripts/run-chef-client-e2e.sh
```

Strict stock-server run:

```sh
MW_SKIP_BUILD=1 MW_E2E_COMPAT=0 \
MW_E2E_ARTIFACTS=/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e-run-strict2 \
scripts/run-chef-client-e2e.sh
```

ACL granted, no v2 synthesis (reproduces the stock v2 client failure):

```sh
MW_SKIP_BUILD=1 MW_E2E_SYNTH_V2=0 MW_E2E_REGISTER=0 \
MW_E2E_ARTIFACTS=/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e-run-acl-only \
scripts/run-chef-client-e2e.sh
```

## What the harness does

1. Builds `microwave-server` and `microwave-ctl`.
2. Picks a free loopback port and starts the server with a fresh temp data
   directory and a fresh SQLite database.
3. Waits for `GET /_status`, then checks that `pivotal.pem` was created.
4. Creates an organization with `POST /organizations`. The response includes
   the generated `<org>-validator` client name and private key.
5. Creates a normal client with `POST /organizations/:org/clients`
   (`create_key: true`) and saves its private key.
6. Probes the default environment as that client with API version 2.
7. Creates a minimal cookbook on disk (`metadata.rb` and
   `recipes/default.rb`). The recipe writes `$ARTIFACTS/marker.txt`.
8. Uploads every file through the sandbox API:
   `POST /sandboxes`, presigned `PUT` to `/bookshelf/...`, then
   `PUT /sandboxes/:id` with `is_completed: true`.
9. Uploads a v0 cookbook manifest with
   `PUT /organizations/:org/cookbooks/:name/:version`.
10. Probes `POST /organizations/:org/environments/_default/cookbook_versions`
    as the client with API version 2.
11. If the response is not v2-shaped, synthesizes an `all_files` manifest from
    the server's own v0 response (including the server-generated GET URLs) and
    uploads it as a patch version.
12. Writes `client.rb`, `client-validator.rb`, and `node.json`. The run list is
    supplied through `-j`, **not** `--override-runlist`, because chef-client
    skips the final node save for a temporary override policy.
13. Runs the real `chef-client`, then verifies the marker file.
14. Verifies the node and client through the API, including the run list and
    `automatic.cookbooks.mw_e2e.version`.
15. Runs a second converge with a nonexistent `client_key`, so chef-client
    registers a new client with the validator key, then verifies that node and
    client as well.

The harness exits non-zero on any failure and keeps the server log, bootstrap
log, chef-client logs, probe responses, manifests, generated keys, and SQLite
data under `MW_E2E_ARTIFACTS`.

## Successful default run

Artifacts:
`/private/var/folders/yv/wmw_npmn7y5c2lwxjywt_f800000gn/T/opencode/mw-e2e-run4`

```text
==> ruby:        ruby 4.0.9 (2026-10-23 revision 3e93b23afc) +PRISM [arm64-darwin25]
==> chef-client: Chef Infra Client: 18.7.10 (installed)
==> starting microwave-server on http://127.0.0.1:62103
==> bootstrapping organization, clients, and cookbook
bootstrap: server gap: default environment ACL did not allow clients; granted read to the clients group
bootstrap: server gap: API v2 cookbook_versions response had no all_files; uploaded a synthesized v2 manifest mw_e2e-0.1.1
...
==> running chef-client
...
[INFO] Loading cookbooks [mw_e2e@0.1.1]
[INFO] Synchronizing cookbooks:
  - mw_e2e (0.1.1)
[INFO] Converging 1 resources
Recipe: mw_e2e::default
  * file[.../marker.txt] action create
[INFO] Chef Infra Client Run complete in 0.5 seconds
...
==> marker verified: .../marker.txt
{
  "node": "mw-e2e-node",
  "node_status": 200,
  "client_status": 200,
  "run_list": [
    "recipe[mw_e2e]"
  ],
  "cookbook_version": "0.1.1",
  "failures": []
}
==> validator registration verified: mw-e2e-node-validator

chef-client e2e: PASS
```

Marker content:

```text
converged by chef-client e2e
```

The validator-registration converge log contains:

```text
Creating a new client identity for mw-e2e-node-validator using the validator key.
INFO: Client key .../keys/registered-client.pem is not present - registering
INFO: Run List is [recipe[mw_e2e]]
...
Infra Phase complete, 0/1 resources updated in 20 seconds
```

Both nodes were read back from the server with `GET
/organizations/mw_e2e/nodes/...`; both returned `200` and the expected run
list and cookbook version.

## Failure evidence

### 1. Stock server: environment read is 403

`MW_E2E_COMPAT=0` exits `5`. The first failing request is the client's
environment read:

```http
GET /organizations/mw_e2e/environments/_default HTTP/1.1
X-Chef-Version: 18.7.10
X-Ops-Server-API-Version: 2
X-Ops-Userid: mw-e2e-node
```

```http
HTTP/1.1 403 Forbidden
Content-Type: application/json

{"error":["missing permission"]}
```

Chef-client reports:

```text
Error Resolving Cookbooks for Run List:
Authorization Error
-------------------
This client is not authorized to read some of the information required to
access its cookbooks (HTTP 403).

To access its cookbooks, a client needs to be able to read its environment and
all of the cookbooks in its expanded run list.

Server Response:
----------------
missing permission
```

The same request returns `200` after the harness grants the `clients` group
read access to the environment:

```json
{
  "chef_type": "environment",
  "cookbook_versions": {},
  "default_attributes": {},
  "description": "The default Chef environment",
  "json_class": "Chef::Environment",
  "name": "_default",
  "override_attributes": {}
}
```

### 2. API v2 cookbook manifest is returned in the v0 shape

With the environment ACL granted but v2 synthesis disabled
(`MW_E2E_SYNTH_V2=0`), chef-client 18.7.10 still fails. The server returns
`200` for the solve, but the response has the v0 segment arrays, not the v2
`all_files` array:

```http
POST /organizations/mw_e2e/environments/_default/cookbook_versions
X-Ops-Server-API-Version: 2

{"run_list":["recipe[mw_e2e]"]}
```

```json
{
  "mw_e2e": {
    "attributes": [],
    "cookbook_name": "mw_e2e",
    "definitions": [],
    "files": [],
    "libraries": [],
    "metadata": {
      "dependencies": {},
      "name": "mw_e2e",
      "version": "0.1.0"
    },
    "name": "mw_e2e",
    "providers": [],
    "recipes": [
      {
        "checksum": "9dcce82a71703249102a503252174e9b",
        "name": "default.rb",
        "path": "recipes/default.rb",
        "specificity": "default",
        "url": "http://127.0.0.1:.../bookshelf/organizations/.../checksums/...?token=..."
      }
    ],
    "resources": [],
    "root_files": [
      {
        "checksum": "167b076d7e4cae4b2cde3d97a9070ddd",
        "name": "metadata.rb",
        "path": "metadata.rb",
        "specificity": "default",
        "url": "http://127.0.0.1:.../bookshelf/organizations/.../checksums/...?token=..."
      }
    ],
    "templates": [],
    "version": "0.1.0"
  }
}
```

There is no `all_files` key. Chef 18/19 requests API v2, selects
`Chef::Cookbook::ManifestV2`, and then fails:

```text
NoMethodError: undefined method 'inject' for nil
.../chef-18.7.10/lib/chef/cookbook_manifest.rb:302:in 'Chef::CookbookManifest#extract_checksums_from_manifest'
.../chef-18.7.10/lib/chef/cookbook_manifest.rb:161:in 'Chef::CookbookManifest#update_from'
.../chef-18.7.10/lib/chef/cookbook_version.rb:177:in 'Chef::CookbookVersion#manifest='
.../chef-18.7.10/lib/chef/cookbook_version.rb:453:in 'Chef::CookbookVersion.from_hash'
.../chef-18.7.10/lib/chef/policy_builder/expand_node_object.rb:190:in 'block in Chef::PolicyBuilder::ExpandNodeObject#sync_cookbooks'
```

### 3. What the compatibility path does

The harness reads the v0 manifest from the server, which already contains
server-generated presigned GET URLs, converts every segment entry into an
`all_files` entry, and uploads that as `mw_e2e-0.1.1`. The subsequent solve
returns the shape the client expects:

```json
{
  "mw_e2e": {
    "all_files": [
      {
        "checksum": "9dcce82a71703249102a503252174e9b",
        "name": "recipes/default.rb",
        "path": "recipes/default.rb",
        "specificity": "default",
        "url": "http://127.0.0.1:.../bookshelf/.../checksums/...?token=..."
      },
      {
        "checksum": "167b076d7e4cae4b2cde3d97a9070ddd",
        "name": "metadata.rb",
        "path": "metadata.rb",
        "specificity": "default",
        "url": "http://127.0.0.1:.../bookshelf/.../checksums/...?token=..."
      }
    ],
    "cookbook_name": "mw_e2e",
    "metadata": {
      "dependencies": {},
      "name": "mw_e2e",
      "version": "0.1.1"
    },
    "name": "mw_e2e-0.1.1",
    "version": "0.1.1"
  }
}
```

The real client then downloads both files through `/bookshelf`, converges the
recipe, writes the marker, and saves the node.

## Missing server behaviors, prioritized

### P0 — blocks every stock chef-client converge

1. **Default environment ACL does not grant `clients` read access.**
   `GET /organizations/:org/environments/_default` as a normal client returns
   `403 missing permission`. The depsolver then returns `403` too because it
   also checks `environment.authz_id` read. The default org policy in
   `crates/microwave-authz/src/org.rs` grants the `clients` group read on the
   `environments` **container**, but environment objects created by
   `create_environment` only grant the creator. Cookbooks already inherit the
   container ACL (`create_cookbook_authz`), so environments should do the same
   or the default policy should add the `clients` group to the `_default`
   environment ACL. Roles likely have the same problem for `role[...]` run
   list entries.

2. **API v2 cookbook manifests are not implemented.**
   `GET /organizations/:org/cookbooks/:name/:version` and
   `POST /organizations/:org/environments/:env/cookbook_versions` return the
   v0 segment arrays regardless of `X-Ops-Server-API-Version`. Chef Infra
   Client 18/19 requests API v2 and expects `all_files`. The client crashes
   with `NoMethodError: undefined method 'inject' for nil` before it can sync
   cookbooks. This is the second hard blocker for stock clients.

### P1 — required for a correct v2 implementation

3. **V2 `all_files` entries need presigned download URLs.**
   `add_download_urls` in `crates/microwave-api/src/cookbooks.rs` only walks
   the v0 segment arrays. If a v2 manifest is stored without URLs, the client
   has no way to download the files. The harness had to copy the v0 response's
   URLs into its synthesized `all_files` entries. The server should add URLs to
   `all_files` on read/solve, or emit them when storing a v2 manifest.

4. **Cookbook checksum validation ignores `all_files`.**
   `manifest_checksums` only scans the nine v0 segments. A v2 manifest can
   reference a checksum that was never uploaded and `PUT
   /cookbooks/:name/:version` will still return `201`. The sandbox commit is
   currently the only thing that proves the blobs exist. Add `all_files` to
   the checksum extraction so v2 uploads fail with the same
   `manifest references missing checksums` error as v0.

### P2 — follow-up coverage

5. **Role ACLs likely repeat the environment issue.** The depsolver checks
   `role.authz_id` read for `role[...]` entries. The harness only used a
   recipe, so this was not exercised. Add a role-based e2e case after fixing
   the environment ACL.

6. **Node update ACL semantics.** The `clients` group has read/create on the
   `nodes` container, not update. A client can update its own node because the
   node creator gets all permissions, but a different client cannot update a
   node created by another client. This did not block the single-client
   converge, but it is worth checking against upstream behavior.

7. **Not exercised by this harness:** search, policyfiles/policy groups, data
   bags, data collector, and the v0/v2 cookbook `GET` endpoints under every
   API version. The v0 path and the basic cookbook/sandbox/node paths are
   verified.

## Files

- `scripts/run-chef-client-e2e.sh` — harness entry point and server lifecycle.
- `scripts/chef-client-e2e-bootstrap.rb` — org/validator/client setup, sandbox
  upload, cookbook manifest upload, v2 capability probe/synthesis, config
  generation, and node/client verification.
- `docs/chef-client-e2e-report.md` — this report.

No server code was modified.

## Next steps

1. Fix the default environment/role ACL inheritance.
2. Implement API v2 cookbook manifest output (`all_files`) for both
   `GET /cookbooks/:name/:version` and the depsolver response.
3. Add presigned URL injection and checksum validation for `all_files`.
4. Rerun `MW_E2E_COMPAT=0 scripts/run-chef-client-e2e.sh`; it should pass
   without either compatibility step. The harness probes capabilities on every
   run, so once the server is fixed it stops applying the workarounds
   automatically.
5. Optionally install the vendored `chef` 19.4.47 bundle and run with
   `MW_E2E_PREFER_VENDORED=1` for a second client version.
