# Chef cookbook guide Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish a cookbook guide on the docs site that shows the minimal all-in-one node and the fully external topology.

**Architecture:** One new markdown page in `docs/`, published by adding it to the Docusaurus `include` list and sidebar. A second task adds the entry points (index, deployment section, navbar, footer) and the claims that keep the page traceable.

**Tech Stack:** Docusaurus 3.10, markdown, `docs/claims.toml`.

**Spec:** `docs/superpowers/specs/2026-11-02-cookbook-guide-design.md`

## Global Constraints

- Debian 13 only; Chef Infra Client 19.4.46 up to below 20 (`metadata.rb`).
- English copy. MCP stays `experimental`; the cookbook renders no `[mcp]` block.
- The cookbook renders no `[server.tls]` block: the listener is plain HTTP.
- The rendered database URL ends in `sslmode=require`: encrypted, peer unverified.
- Facts come from `cookbooks/microwave/README.md`, its resources and `metadata.rb`.
- Claims for every new `docs/*.md` file; `scripts/check-claims.py` enforces it.

## Review Focus

- **External database without `client_network`:** `pg_hba` keeps only `hostssl` loopback rules, so the server cannot connect. The page must tie `client_network` to every backend example.
- **Server node without all three CAs:** the S3 and OpenSearch hops verify against the system trust store; one CA file must hold database, search and garage CAs. The page must say this next to `ca_cert_file`.
- **`object_store_endpoint` naming a host outside the certificate SAN:** presigned URLs break. The page must say it is the client-facing name and has to be in the SAN.
- **`version: 'latest'` without `GITHUB_TOKEN`:** anonymous GitHub API rate limits fail the converge. The page states the token and the `version` pin.
- **Reader with only one backend external:** the examples are per-node, so a mixed layout stays possible. The page must present them as independent node blocks, not one all-or-nothing script.

---

### Task 1: The guide page

**Files:**
- Create: `docs/cookbook.md`
- Modify: `website/docs/docusaurus.config.ts` (the `include` array)
- Modify: `website/docs/sidebars.ts` (the "Operator guides" items)

**Interfaces:**
- Produces: route `/cookbook/` and the anchors `#minimal-embedded-node`, `#everything-external`, `#the-service-switches`, `#tls-limits`, `#tests`. Task 2 links the route; the claims there grep `include_recipe 'microwave::default'` and `include_recipe 'microwave::server'` on this page.

- [ ] **Step 1: Write `docs/cookbook.md`**

Structure, headings exactly as given. Prose is the implementer's, except the sentences in quotes here and every code block.

Intro paragraph: what `cookbooks/microwave` installs (the release `.deb`, PostgreSQL 19, OpenSearch 3.9, Garage behind nginx on Debian 13), that it is resource-oriented and the recipes only wrap the four resources, and a link to [`cookbooks/microwave/README.md`](../cookbooks/microwave/README.md) as the reference for resources, properties and attributes. State the Chef constraint `>= 19.4.46, < 20` and that MCP stays off because no `[mcp]` block is rendered.

`## Prerequisites` — the run needs a Chef Infra Server or a Microwave instance for the run list; the server package comes from a GitHub release. `version: 'latest'` asks the API every converge, so a `GITHUB_TOKEN` in the environment lifts the anonymous rate limit and a pinned version is the escape hatch.

`## Minimal embedded node` — all four resources on one node, PostgreSQL and OpenSearch on loopback, local object store. The example:

```ruby
node.default['microwave']['bind'] = 'chef.example.com'
node.default['microwave']['create_organization'] = true
include_recipe 'microwave::default'
```

Then: these two are all that is worth setting — the bind because the default is the node name, which stock Debian resolves to `127.0.1.1`, and `create_organization` for the `pivotal` admin, the `demo` organization and its membership. `create_organization` waits for `/_status` and the server writes the superuser key to `<data_dir>/pivotal.pem`. Garage is composed only with `object_store_backend = 'garage'`; the local store is the default. The listener is plain HTTP, so client TLS belongs in a proxy in front.

`## Everything external` — one node per role, every backend opened to the client network. One sentence up front: the blocks are independent, so a mixed layout with a single backend elsewhere is fine too.

```ruby
# database node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::database'
```

```ruby
# search node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::search'
```

```ruby
# store node
node.default['microwave']['client_network'] = '10.0.0.0/24'
include_recipe 'microwave::garage'
```

```ruby
# server node
node.default['microwave']['bind'] = '10.0.0.10'
node.default['microwave']['database_host'] = 'db.example.com'
node.default['microwave']['search_host'] = 'search.example.com'
node.default['microwave']['database_password_file'] = '/etc/chef/db.password'
node.default['microwave']['ca_cert_file'] = '/etc/chef/ca.crt'
node.default['microwave']['object_store_backend'] = 'garage'
node.default['microwave']['object_store_endpoint'] = 'https://store.example.com:3900'
node.default['microwave']['object_store_access_key_file'] = '/etc/chef/garage.access_key'
node.default['microwave']['object_store_secret_key_file'] = '/etc/chef/garage.secret_key'
include_recipe 'microwave::server'
```

After the server block: without `client_network` a backend stays on loopback and no remote client connects. `microwave::server` raises without a database host and a search host, and without a password it can read from `database_password` or the password file, so the server node carries the password and CA itself. The CA file holds all three CAs concatenated — each node signs with its own CA, and the server verifies the OpenSearch and S3 hops against the system trust store. `object_store_endpoint` lands in presigned URLs, so it must be the address clients reach the store at and be covered by the store certificate's SAN.

`## The service switches` — `microwave::disable` is `stop` plus `disable` and a marker under `/etc/microwave/disabled/<unit>`; no mask, no package removal, no data deletion. The marker keeps a unit down over later `microwave::default` converges until `microwave::enable` or a resource's `:enable` removes it. Disable goes server first, nginx last; enable is the inverse. A single resource's `:enable` never starts nginx, so `microwave::enable` belongs on the same node.

`## TLS limits` — the database URL uses `sslmode=require`: encrypted, peer unverified; `verify-full` in a hand-written `database_url` closes that. The listener stays plain HTTP, and the installed CA covers backend connections, not clients.

`## Tests` — `bundle exec rspec` (ChefSpec), `bundle exec cookstyle`, and the three Kitchen suites `kitchen:all_in_one`, `kitchen:split` and `kitchen:disable`; each converges twice with `enforce_idempotency`. CI runs them from `.github/workflows/cookbook.yml` on `cookbooks/**` changes.

- [ ] **Step 2: Publish the page**

Add `'cookbook.md'` to the `include` array in `website/docs/docusaurus.config.ts`, after `'deployment.md'`. Add `{type: 'doc', id: 'cookbook', label: 'Chef cookbook'}` to the "Operator guides" items in `website/docs/sidebars.ts`, after `deployment`.

- [ ] **Step 3: Build and check the route**

Run: `cd website/docs && npm run build && test -f build/cookbook/index.html && grep -q "microwave::default" build/cookbook/index.html && grep -q "microwave::server" build/cookbook/index.html && grep -q "client_network" build/cookbook/index.html && grep -q "GITHUB_TOKEN" build/cookbook/index.html && grep -q "concatenated" build/cookbook/index.html && grep -q "presigned" build/cookbook/index.html && grep -q "database node" build/cookbook/index.html`
Expected: build passes (no broken links, no broken anchors), every grep succeeds.

- [ ] **Step 4: Commit**

```bash
git add docs/cookbook.md website/docs/docusaurus.config.ts website/docs/sidebars.ts
git commit -m "docs(cookbook): publish the cookbook guide"
```

---

### Task 2: Entry points and claims

**Files:**
- Modify: `docs/index.md` ("Start here" list)
- Modify: `docs/deployment.md:650-681` (section 5)
- Modify: `website/docs/docusaurus.config.ts` (navbar items, footer Guides links)
- Modify: `docs/claims.toml` (two claims)

**Interfaces:**
- Consumes: route `/cookbook/` and the page text from Task 1.

- [ ] **Step 1: Link the page from the index**

Add after the deployment bullet in "Start here":

```markdown
- **[Install it with Chef](./cookbook.md)** — the cookbook for Debian 13, from a
  single embedded node to a server with every backend on its own host.
```

- [ ] **Step 2: Point the deployment section at the page**

In `docs/deployment.md` section 5, after the paragraph ending "on top.", add: `The full guide is [Chef cookbook](./cookbook.md); this section keeps the summary.` Leave the rest of the section as it is.

- [ ] **Step 3: Add the navbar and footer entries**

In `website/docs/docusaurus.config.ts`, add `{type: 'doc', docId: 'cookbook', label: 'Cookbook', position: 'left'}` after the `mcp` navbar item, and `{label: 'Chef cookbook', to: '/cookbook'},` after the `Debian packages` footer link.

- [ ] **Step 4: Add the claims**

Append to `docs/claims.toml`:

```toml
[[claim]]
id = "cookbook-guide-minimal"
summary = "Cookbook guide: the embedded node needs only a bind and the demo organization."
area = "deployment"
status = "verified"
surfaces = [
  { path = "docs/cookbook.md", needle = "include_recipe 'microwave::default'" },
]
verify = { kind = "grep", path = "cookbooks/microwave/README.md", pattern = "Only the bind is worth setting" }

[[claim]]
id = "cookbook-guide-external"
summary = "Cookbook guide: the external topology is one node per role on a client network."
area = "deployment"
status = "verified"
surfaces = [
  { path = "docs/cookbook.md", needle = "include_recipe 'microwave::server'" },
  { path = "docs/cookbook.md", needle = "client_network" },
]
verify = { kind = "grep", path = "cookbooks/microwave/README.md", pattern = "A database node, a search node, a store node and a server node" }
```

- [ ] **Step 5: Check claims and build**

Run: `python3 scripts/check-claims.py`
Expected: no output, exit 0.

Run: `cd website/docs && npm run build && grep -q 'href="/microwave-development/docs/cookbook/"' build/deployment/index.html`
Expected: build passes, grep succeeds.

- [ ] **Step 6: Commit**

```bash
git add docs/index.md docs/deployment.md docs/claims.toml website/docs/docusaurus.config.ts
git commit -m "docs(cookbook): link the guide from index, deployment and nav"
```
