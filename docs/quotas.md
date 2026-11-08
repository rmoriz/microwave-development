# Object quotas

An install without quotas lets one tenant upload cookbooks until the bucket
fills and register nodes until the database and the license distance grow. An
object quota puts a ceiling on that. It is opt-in per organization and
unlimited until you add an entry.

A quota is a hard, per-organization ceiling on one object kind. A creation that
would exceed it fails with `403` before anything is written; nothing is stored
partially.

## Kinds

Five kinds can be capped:

- nodes,
- cookbook versions (a cookbook artifact version shares this ceiling),
- custom environments,
- roles,
- policy revisions.

Users, clients, groups, data bags, containers and blobs are not capped. Bucket
quotas and lifecycle rules stay the operator's job for the blobs; see
[Multi-tenant mode](multi-tenant-mode.md).

## Configuration

Every field is optional and absent means unlimited for that kind. Adding an
entry is the whole opt-in: `enabled` defaults to `true`.

```toml
[quotas]
# Master switch; false ignores every entry below.
enabled = true

# Every field is optional, absent means unlimited for that kind. Zero is a
# valid ceiling: the organization may hold no object of that kind at all.
[quotas.organizations."acme"]
max_nodes = 500
max_cookbook_versions = 5000
max_environments = 20
max_roles = 100
max_policy_revisions = 200
```

The map is keyed by organization name and follows the organization naming
rules. `MW_QUOTAS__ENABLED` sets the switch and `MW_QUOTAS__ORGANIZATIONS`
takes one JSON object, like `MW_OBJECT_STORE__ORGANIZATIONS`.

The auto-created `_default` environment is exempt, so
`max_environments = 0` still creates an organization. Zero is a valid ceiling:
the organization may hold no object of that kind at all.

## What a rejection looks like

A creation over the ceiling answers `403` with `{"error":["node limit of 5 reached"]}`; the same `PUT` on an existing object still updates it. Every
other kind spells its own name in place of `node`.

Deletes always work, so a cleanup frees a slot and the next creation succeeds.
A re-upload of an existing node, cookbook version or policy revision creates no
row and never trips a ceiling; it keeps its familiar `409` or `200`.

Lowering a limit never deletes anything. Objects above the new ceiling stay in
place and only additions are blocked until the count drops below it.

## Metrics and logs

A rejection increments `microwave_server_limits_total{scope="quota:<kind>",outcome="rejected"}`,
so `/_stats`, the OTLP export and the cardinality caps work unchanged. The
`WARN` log line carries the organization id, the kind and the configured
ceiling.

## Independence

Quotas are independent of `limits.enabled` and of `object_store.require_org_bucket`.
The rate limits bound request pressure; the object quotas bound stored rows. MTM
installs get the most out of both together, but neither requires the other.

## Provisioning and reload

An entry for a new organization is registered with `org-create`, so pre-provisioning works: the command reloads a running server, and the ceilings already
apply to the very first request of the fresh organization. If no server runs,
the entry stays inert until the next start; `org-create` only warns then.

`microwave-ctl check-config` fails when an entry names an organization that does
not exist yet, also with `quotas.enabled = false`. A reload only warns about the
same name and keeps the rest of the policy, so you can ship the entry before the
organization. Everything else about a reload applies: a changed ceiling takes
effect without a restart, and a rejected reload keeps the previous policy.

A `microwave-migrate` import runs without quotas, so a restore is never truncated.

## Diagnostics

`microwave-ctl check-config` fails on every unknown organization name and
summarizes the number of entries. `microwave-ctl diagnostics` prints
`quotas: <n> organization(s)`, or `quotas: disabled` when the switch is off, and
reports an unknown name as a warning without failing.

## Limitations

- No byte or blob ceiling. Use bucket quotas and lifecycle rules for storage.
- No soft warning, grace override or billing integration.
- Only creations are capped. There is no automatic cleanup when a limit drops.

## See also

- [Deployment](deployment.md#object-quotas) — every key and its environment form.
- [Request limits](request-limits.md) — the separate, process-local admission limits.
- [Multi-tenant mode](multi-tenant-mode.md) — per-organization buckets.
