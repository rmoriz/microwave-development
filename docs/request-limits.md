# Request limits

Microwave Server bounds the request pressure one client can put on a running
process. Without that bound a single misconfigured `chef-client`, a busy
`knife` loop, or a scripting mistake can occupy every database connection and
every worker, and every other client — inside the same organization or in a
different one — waits behind it.

Every limit lives in process memory. There is no shared counter, no Redis, no
database round trip: a bucket check is a handful of instructions under a shard
lock, and it runs before any body is read.

The gates are on by default with conservative values. A limit violation
answers `503` with `Retry-After` in whole seconds; `chef-client` retries `503`
on its own, which is why `503` is used instead of `429`.

## What the limits can do

- **Protect the process.** A global in-flight cap keeps the admitted work below
  the database pool and the worker count, whatever the caller's identity.
- **Isolate organizations.** Every authenticated organization has its own rate
  and in-flight share. One noisy organization cannot eat another
  organization's budget.
- **Isolate principals.** Inside an organization, every principal has a
  stricter share than the organization. A single node key cannot starve its
  colleagues.
- **Filter unauthenticated floods early.** A token bucket per client address
  runs before signature verification, so junk never reaches the database.
- **Cover the side surfaces.** Bookshelf uploads and downloads are charged to
  the organization in the path before the body is buffered; MCP calls are
  charged to the token's organization and principal after authentication.
- **Exempt the signals.** Health, statistics, version, license, the landing
  page and its static assets stay reachable when every slot is taken.
- **Show what is happening.** Rejections are counted by scope and organization,
  logged with their request id, and visible as gauges and counters on `/_stats`
  and over OTLP.

## Defaults

The defaults fit a small single-node server. Raise them as the hardware, the
database pool, and the fleet grow — but never above what the next layer can
absorb.

| Setting | Default | Effect |
|---|---|---|
| `limits.enabled` | `true` | Master switch for every gate. |
| `limits.queue_timeout_ms` | `1500` | Longest wait for a free in-flight slot before `503`. `0` fails immediately. |
| `limits.max_entries` | `10000` | Upper bound per bucket map, divided over 16 shards. At least `100`. |
| `limits.global.in_flight` | `32` | Concurrent requests admitted per process. |
| `limits.source.rate_per_second` | `500` | Requests per second per client address, before authentication. |
| `limits.source.burst` | `1000` | Token capacity per client address. |
| `limits.org.rate_per_second` | `200` | Sustained requests per second for one organization. |
| `limits.org.burst` | `400` | Token capacity for one organization. |
| `limits.org.in_flight` | `16` | Concurrent requests for one organization. |
| `limits.principal.rate_per_second` | `20` | Sustained requests per second for one principal. |
| `limits.principal.burst` | `40` | Token capacity for one principal. |
| `limits.principal.in_flight` | `4` | Concurrent requests for one principal. |
| `limits.organizations.<name>` | none | Per-organization override of `rate_per_second`, `burst`, and `in_flight`. |

Each write is available as the matching `MW_LIMITS__*` environment variable,
for example `MW_LIMITS__ORG__RATE_PER_SECOND`; the
[configuration index](deployment.md#request-limits) lists every spelling.

## How counting works

### Order of the gates

A request passes the gates in this order, and a rejection at any gate returns
immediately:

1. **Admission.** Exempt paths pass. Otherwise one global in-flight slot is
   taken, waiting up to `limits.queue_timeout_ms`.
2. **Source bucket.** One token from the bucket of the resolved client address.
3. **Authentication.** Signature or bearer verification, and the organization
   lookup. Unauthenticated requests never reach the next two gates.
4. **Organization bucket.** One token and one org in-flight slot.
5. **Principal bucket.** One token and one principal in-flight slot.

All permits are held until the response is ready. Steps 1 and 2 run before the
body is read, so an upload rejected there never buffers. The organization and
principal permit are taken in the same middleware that verifies the signature,
and an MCP call takes them after its bearer token authenticated.

Paths without an organization (`/users`, `/organizations`, `/authenticate_user`,
`/id/*`, `/system_recovery`, OAuth) end after step 2: they are bounded globally
and by source only. An unknown organization answers `404` before any
organization token is spent.

### Tokens and refill

Each bucket holds a number of tokens up to its `burst`. Tokens refill
continuously at `rate_per_second`; a request takes one token and proceeds only
while at least one is available. A long idle period refills only up to `burst`,
never beyond it.

A rejection reports the whole seconds until the next token:
`ceil((1 - tokens) / rate_per_second)`, at least `1`. This is the `Retry-After`
value. Because `burst` tokens are available at once, a burst above
`rate_per_second` absorbs a short spike without a rejection.

### In-flight slots

The global cap, every organization and every principal also own a semaphore.
A caller that finds it empty waits up to `limits.queue_timeout_ms`, then gets
`503` with `Retry-After: 1`. The wait is what lets a short burst of parallel
`chef-client` runs drain instead of failing outright. With `0` there is no
wait at all.

The source bucket has no separate in-flight cap; the global admission already
bounds concurrency there.

### Bucket lifetime, eviction, and memory

Bucket entries are created lazily per key and live until they are evicted. Each
map is split over 16 shards. When a shard exceeds `ceil(max_entries / 16)`
entries, it drops roughly the oldest tenth of its entries, oldest first.

An entry with a request in progress is never evicted, so its tokens and
in-flight state survive. A map whose entries are all busy grows past
`max_entries` instead of handing out a fresh, full bucket. `max_entries` is
therefore a target for the idle case, not a hard allocation cap. A restart
clears every bucket.

### Exempt paths

These paths skip both pre-auth gates (and everything behind them):

- `/_status`, `/_stats`, `/version`, `/server_api_version`, `/license`
- the static assets and the favicon
- `/` while it serves the landing page

A single trailing slash is tolerated, so `/_status/` is exempt as well. A
probe or scrape URL written with a trailing slash keeps working while the
server is saturated.

### Which address is the client

The immediate peer is the client unless it is listed in
`server.trusted_proxies`. For a trusted peer the `X-Forwarded-For` header is
read from the right, and the rightmost address that is itself not trusted is
used. A prefix a client prepends to the header therefore cannot bypass the
bucket.

IPv4 addresses are bucketed exactly. IPv6 addresses are bucketed by their `/64`
prefix, so a client that rotates through its subnet keeps one quota, and an
IPv4-mapped IPv6 address shares the bucket of its IPv4 form. An entry may carry
a port (`203.0.113.7:443`).

A request with no peer address at all — an in-process call, for example —
skips the source bucket and is still bounded globally.

### Organization and principal keys

The organization bucket is keyed on the internal organization ID, so renaming
an organization keeps its bucket. The principal bucket is keyed on the pair of
organization ID and principal ID.

`limits.organizations.<name>` is matched on the organization name and overrides
`rate_per_second`, `burst`, and `in_flight` for that organization. Only the keys
you set win; the others keep their default. An override changes a live bucket on
the next request, including the in-flight cap. Because the match is by name, an
override stops applying after the organization is renamed until you rename the
key too.

Bookshelf paths carry the organization ID and no principal. The gate resolves
the name first so that overrides and the `org` metric label follow the
organization; if the row is gone, the ID is used.

### Responses and headers

A rejection is

```http
HTTP/1.1 503 Service Unavailable
Retry-After: 2
Content-Type: application/json

{"error":["Server is overloaded. Retry later."]}
```

`Retry-After` is always at least `1`. Cinc and `chef-client` retry a `503` with
backoff; they do not retry `429`.

### Counters, gauges, and logs

Every decision is counted as `microwave_server_limits_total` with the labels
`scope` (`global`, `source`, `org`, `principal`), `org`, and `outcome`
(`allowed`, `rejected`). `microwave_server_limits_in_flight` reports the current
in-flight count per scope. Both appear on `/_stats` — JSON under `limits`, and
Prometheus text — and are exported over OTLP when metric export is enabled; see
[OpenTelemetry metrics](opentelemetry.md).

A rejection also writes a `request rejected by limit` warning with the request
id, method, path, scope, organization, principal, and `Retry-After`, so it joins
the access-log line for the same request.

## Options in detail

### `limits.enabled`

Boolean, default `true`. `false` turns every gate into a no-op immediately:
no slot is taken, no bucket is consulted, and `/_stats` still reports the
`limits` object with the counters it already has. Paths, signatures, and error
shapes are unchanged. Useful to take the limiter out of the picture while
diagnosing an unrelated problem.

### `limits.queue_timeout_ms`

Unsigned integer, default `1500`, `0` allowed. Longest time a request waits for
a free in-flight slot. Lower it when latency matters more than absorbing
bursts; raise it when clients are long-running and a short queue is normal.
The value applies to all three in-flight scopes.

### `limits.max_entries`

Unsigned integer, at least `100`, default `10000`. Target size of each bucket
map, split over 16 shards. Size it to the number of distinct source addresses,
organizations, and principals you expect between evictions, with headroom.
`check-config` rejects anything below `100`.

### `limits.global.in_flight`

Unsigned integer, at least `1`, default `32`. Concurrent requests admitted by
one process. This is the last line before the database pool and the worker
set. Keep it below `database.pool_size` and below the worker count, so the
pool serves admitted requests instead of queueing them. On a large host with a
large pool, raise it in steps and watch the pool gauges on `/_stats`.

### `limits.source.rate_per_second` and `limits.source.burst`

Unsigned integers, at least `1`, defaults `500` and `1000`. Pre-auth allowance
per client address. This bucket exists to keep unauthenticated junk away from
signature checking and the database, so it can be far more generous than the
organization bucket — a NAT gateway or a CI runner shares one address for many
legitimate clients. Raise the burst for large parallel fleets behind one
address; raise the rate when a legitimately busy address is rejected.

### `limits.org.rate_per_second`, `limits.org.burst`, `limits.org.in_flight`

Unsigned integers, at least `1`, defaults `200`, `400`, and `16`. The
organization share across all its principals. It should sit above the sum a
healthy organization needs and below what the process can carry. With many
organizations, keep `org.in_flight × organizations` near the global cap; the
global gate is shared, so one organization cannot hold more than its own cap
anyway.

### `limits.principal.rate_per_second`, `limits.principal.burst`, `limits.principal.in_flight`

Unsigned integers, at least `1`, defaults `20`, `40`, and `4`. The per-principal
share inside an organization. Keep it well below the organization values: the
organization values exist to serve several principals at once. A node that
runs many recipes in parallel needs enough `principal.in_flight` for its
converge; a shared user identity needs enough for its tooling.

### `limits.organizations.<name>`

Per-organization override, keyed by organization name; the name must follow the
organization naming rules. Each of `rate_per_second`, `burst`, and `in_flight`
is optional and must be at least `1`. Use it for the batch org that legitimately
needs more, or the tenant that should get less. Overrides and plain requests
share one bucket, so an override cannot be used to get a second allowance.

```toml
[limits.organizations.batch-indexer]
rate_per_second = 500
burst = 1000
in_flight = 32
```

## Tuning and troubleshooting

- **Find the scope first.** `/_stats` and the rejection log name the scope that
  bound the request. A `global` rejection means the process cap; raise
  `limits.global.in_flight` or take load off the host. An `org` or `principal`
  rejection means one tenant or identity is too loud.
- **Do not outrun the next layer.** A global cap above the pool size or the
  worker count moves the queue into the database, where the wait has no
  `Retry-After` and no per-org share.
- **`Retry-After` is honest.** It is derived from the refill rate, not fixed, so
  a client that honours it usually succeeds on the next attempt.
- **A `source` rejection on a shared address is a budget question.** A NAT or
  proxy address serves many clients; raise the source burst or rate rather than
  blaming the client.
- **An override that stopped working** usually follows an organization rename.
  Rename the `limits.organizations.<name>` key with the organization.
- **`check-config` fails at startup** on zero rates, zero burst, zero
  in-flight values, `max_entries` below `100`, or an invalid override name.
- **Several replicas multiply every limit.** Each process enforces its own
  buckets, so the effective fleet total is the per-process value times the
  replica count. Size per instance against the share of traffic it receives.

## See also

- [Deployment](deployment.md#request-limits) — the key index and the deployment
  runbook.
- [OpenTelemetry metrics](opentelemetry.md) — the limit counters and gauges,
  their attributes, and the cardinality caps.
- [MCP endpoint](mcp.md#limits) — request limits on the MCP surface.
- [Load test](load-test.md#limit-503s) — the stack's widened buckets and the
  `limits 503s` column.
