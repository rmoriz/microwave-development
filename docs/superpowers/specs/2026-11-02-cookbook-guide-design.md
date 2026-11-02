# Chef cookbook guide on the docs site

## Intent

The cookbook is installable, but the only page describing it is
`cookbooks/microwave/README.md` on GitHub. Operators who want to run it have to
leave the documentation site, and the deployment guide only carries a five-line
pointer. A published guide should show the two ends of the topology scale
plainly: the smallest all-in-one node, and a server node with every backend
external. The cookbook README stays the reference for attributes, resources and
test commands.

## Goals

- New page `docs/cookbook.md`, published on the Docusaurus site.
- Sections: prerequisites, the minimal embedded node, everything external, the
  service switches, the two TLS limits, tests, and a link to the README.
- The embedded example is exactly what the README calls worth setting — a bind
  and, optionally, the demo organization — plus `microwave::default`. Defaults
  are listed, not restated as a runbook: PostgreSQL and OpenSearch on loopback,
  local object store, Garage only with `object_store_backend = 'garage'`.
- The external example is a server node using `microwave::server` only, with
  `database_host`, `search_host`, password file, one concatenated CA file,
  `object_store_*` endpoint and key files, and the three backend nodes each
  opening `client_network`. The notes state the CA, SAN and reachability
  constraints the README carries.
- `docs/index.md` lists the guide, `docs/deployment.md` links it from section 5,
  `sidebars.ts` puts it under "Operator guides", the navbar and the footer get a
  "Cookbook" entry.
- Claims in `docs/claims.toml` cover the page and verify against the cookbook
  README, so the coverage rule in `scripts/check-claims.py` stays green and the
  page cannot silently drift.

## Non-goals

The full attribute and property tables (the README carries them), multi-node
Garage clusters, HA, client-facing TLS, platform portability beyond the
cookbook's Debian 13, Supermarket publishing, and any change to the cookbook
itself.

## Anti-goals

- A page that promises a topology the cookbook does not render.
- Two divergent copies of the attribute table.
- Any fact not traceable to `cookbooks/microwave/README.md` or the resources.

## Constraints

- Debian 13 only, Chef Infra Client 19.4.46 to below 20 — both from
  `metadata.rb`.
- `include` in `website/docs/docusaurus.config.ts` plus a sidebar entry; links
  outside `docs/` use the `../` form the `github-relative-links` plugin
  rewrites.
- `onBrokenLinks: 'throw'` makes the build the link test.

## Approaches

A chosen: one dedicated guide page, two topologies, README as reference. B
(only extending `deployment.md`) was dropped because the page would mix
deployment and cookbook. C (mirroring the README) was dropped for the duplicate
source.

## Verification

`cd website/docs && npm run build` and `python3 scripts/check-claims.py`. The
homepage is untouched.
