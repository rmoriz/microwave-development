# Load test stack

One Compose stack for the converge load test: PostgreSQL, a seeded fixture, the
server built from the repository `Dockerfile` and the `microwave-load` driver
with a real `chef-client` canary, started from a checkout with Docker on a Linux
host and torn down on every exit. It always uses PostgreSQL, so a run is only
comparable to another run with the same fingerprint; the report and its
fingerprint land in `docker-load/out/`.

```sh
docker-load/run.sh          # reference profile
docker-load/run.sh smoke    # shrunken fixture, for a first look
```
