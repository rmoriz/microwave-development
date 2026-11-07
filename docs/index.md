# Microwave Server documentation

Microwave Server is a Rust reimplementation of the Chef Infra Server API, built
as a drop-in replacement for existing `cinc-client`[^cinc] fleets and operations
tooling. This site collects the guides an operator needs; the source for every
page is a markdown file in the repository, rendered here unchanged.

Microwave Server is an independent project, not affiliated with or endorsed by
Progress.

## Start here

- **[Deploy it](./deployment.md)** — the single binary, Docker and systemd
  packaging, the SQLite and PostgreSQL backends, backup and restore, and the
  migration route from a legacy Chef Infra Server. TLS, SMTP, external
  backends, and the whole runbook are documented in
  [`docs/deployment.md`](./deployment.md).
- **[Run multi-tenant mode (MTM)](./multi-tenant-mode.md)** — opt-in
  per-organization buckets and the strict form: the required settings, the
  live reload, and the move of an existing organization.
- **[Install it with Chef](./cookbook.md)** — the cookbook for Debian 13, from a
  single embedded node to a server with every backend on its own host.
- **[Point MCP clients at it](./mcp.md)** — the per-organization MCP endpoint
  over the Streamable HTTP transport. MCP is `experimental`: off by default, and
  writes are on by default but switched off with `writes = false`.
- **[Bound request pressure](./request-limits.md)** — the process-local
  admission, source, organization, and principal limits: what they protect,
  their defaults, how tokens and in-flight slots are counted, and how to tune
  them.
- **[Monitor it with OpenTelemetry](./opentelemetry.md)** — opt-in OTLP/HTTP
  metric export, off by default: the collector setup, the exact series with
  their types and attributes, whole-server and per-org queries, and the
  cardinality caps.
- **[Install the Debian package](./debian-package.md)** — the local `.deb`
  packaging layer, what it installs, and what it deliberately leaves out.
- **[HTTP API parity and tests](./http-parity.md)** — the signed API surface,
  the authorization model, and the dual-backend suites that pin them down.
- **[Run the load test](./load-test.md)** — the converge harness, how to read
  its report and fingerprint, and how to compare two runs.
- **[Try the whole stack with Docker](./docker-e2e.md)** — a Compose stack with
  a seeded organization, four distro nodes, a console and a workstation, for
  watching a real converge end to end.

## How do you know it works?

- **[Pedant results](./pedant-report.md)** — the `oc-chef-pedant` compatibility
  run against the shipped server, tag by tag.
- **[Extended pedant results](./pedant-extended-report.md)** — the wider suite.
- **[Remaining work](./remaining-work.md)** — the open gaps, generated from the
  repository's claims inventory.

## Source

The code, the issue tracker and these pages live at
[github.com/rmoriz/microwave-development](https://github.com/rmoriz/microwave-development).
The marketing page is [Homepage](https://rmoriz.github.io/microwave-development/).

[^cinc]: [Cinc Client](https://cinc.sh/) — the open-source Chef client distribution.
