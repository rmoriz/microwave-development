# Running tenants on their own buckets

By default every organization writes into one shared bucket. A larger install
often wants more separation: one bucket per organization, so a quota, a
lifecycle rule or a leaked URL only ever touches one tenant.

Microwave Server supports that in two strengths:

- **Per-organization buckets** — an organization opts in with one map entry.
  Every other organization keeps using the shared bucket.
- **Strict tenant mode** — every organization must have its own bucket. The
  shared bucket is gone, so there is no silent fallback to the wrong target.

Both work against any plain S3 endpoint (AWS S3, Garage, MinIO). Bucket
creation, quota and lifecycle stay with your own tooling; the server only
routes, signs and probes.

## What you need

Before you start, have the following ready:

- the `s3` or `garage` backend — the map and strict mode are rejected with
  `local`,
- one bucket per organization, named with lowercase letters, digits, `-` and
  `.`, 3–63 characters, no leading or trailing `-` or `.`,
- a key pair per organization with `HeadBucket`, `PutObject`, `GetObject` and
  `DeleteObject` on that bucket,
- a health bucket for `/_status` in strict mode,
- an endpoint both the server and the clients can resolve.

The bucket name is the only per-organization value you configure; the key pair
never enters the config file. It lives in the secret store as
`object_store.<org>.access_key` and `object_store.<org>.secret_key`.

The smallest strict configuration looks like this:

```toml
[object_store]
backend = "garage"
endpoint = "https://cookbooks.example.com"
region = "garage"
path_style = true
require_org_bucket = true
health_bucket = "mw-health"
# no shared `bucket` in strict mode
```

`region`, `path_style` and the shared credentials fall back to the global
values; every organization still needs its own bucket entry and both secrets.
The field reference is in
[Deploying Microwave Server](deployment.md#object-store); this page stays on
the workflow.

## Turning strict mode on

1. Create the health bucket and one bucket per organization with your own
   tooling.
2. Store the key pairs:

   ```sh
   microwave-ctl set-secret object_store acme.access_key GK-acme
   microwave-ctl set-secret object_store acme.secret_key ...
   ```

3. Add the entries and the strict block above to the configuration. Leave the
   shared `bucket` out.
4. Run `microwave-ctl check-config`. It names every missing entry, missing
   secret, invalid bucket name and duplicate target before anything restarts.
5. Start the server. On the way up it sends one `HEAD` per bucket; a bucket it
   cannot reach stops the start.

With `verify_buckets = false` the probe is skipped, so a typo surfaces on the
first request instead.

An empty organization table is fine, so a fresh strict install starts even
before the first tenant exists. The first `org-create` then needs its entry and
secrets immediately; the command prints the new organization id and the missing
steps.

## Giving a single organization its own bucket

Without strict mode, prepare the bucket and credentials as above, add the map
entry, and reload:

```toml
[object_store.organizations."acme"]
bucket = "mw-acme"
```

An entry without its own key pair inherits the shared keys. Hand out a separate
pair where it matters: presigned URLs are signed with the tenant key, so a
leaked URL stays inside that one bucket.

If you later flip `require_org_bucket`, add an entry and both credentials for
every organization first. Until then they still use the shared bucket.

## Applying a change without a restart

Configuration, map entries and secrets all apply live, through
`microwave-ctl reload` or a `SIGHUP` to the server. The reload builds and
probes the whole object-store block before it swaps; a rejected reload keeps
the previous routing and says so in `microwave-ctl diagnostics`.

The key layout does not change: `organization-<org_id>/checksum-<md5>` inside
every bucket. Keys and checksums never reach the log.

## Moving an existing organization

The copy is not quiesced, so plan on two passes:

1. Create the bucket and credentials, `set-secret` them and add the entry.
2. Copy `organization-<org_id>/` from the shared bucket to the new one.
3. Reload. The organization routes to its own bucket with strict mode still
   off.
4. Copy again to catch writes that landed on the shared bucket in between.
5. Flip `require_org_bucket` only when the second copy is clean, then reload.

A rollback is the same copy in reverse before you flip the flag; without it
existing downloads break.

## When something does not line up

- An organization without a bucket answers `500 NoOrgBucket` and fails
  without writing into the shared bucket. Add the entry and credentials, then
  reload.
- `check-config` and `diagnostics` list missing entries, missing secrets,
  invalid names and duplicate `(endpoint, bucket)` pairs.
- An entry for a deleted organization is only a warning on the server and stays
  inactive; `check-config` and `diagnostics` report it and `reload` refuses to
  signal until it is removed.
- A bucket that answers the startup `HEAD` with `403` is usually a key without
  `HeadBucket`. Widen the key or set `verify_buckets = false` while you fix it.

## Deleting and re-creating an organization

Deleting an organization leaves its bucket and objects in place. The
organization **id** is part of the key prefix, so re-creating the same name
gets a new id and a new prefix: the old blobs stay where they are and the new
organization does not see them. `org-create` prints the id so your tooling can
move or clean them up.
