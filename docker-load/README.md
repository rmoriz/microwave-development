# Load test stack

One Compose stack for the converge load test: PostgreSQL, a seeded fixture, the
server built from the repository `Dockerfile` and the `microwave-load` driver
with a real `chef-client` canary. It always uses PostgreSQL and measures one
host, so a run is only comparable to another run with the same fingerprint. A
Linux Docker host with enough RAM for the limits below is recommended; the
defaults are 2 CPUs/4 GB for the server and 4 CPUs/4 GB for the load side.

```sh
docker-load/run.sh          # reference profile
docker-load/run.sh smoke    # shrunken fixture, for a first look
```

The report and its fingerprint land in `docker-load/out/`. Containers, volumes
and the locally built images are removed on exit, also after a failure. Raise
`MW_POOL_SIZE` together with `MW_PG_MAX_CONNECTIONS` for a pool-size comparison
and keep everything else identical; `MW_SERVER_CPUS`, `MW_SERVER_MEM`,
`MW_LOAD_CPUS` and `MW_LOAD_MEM` set the resource limits.
