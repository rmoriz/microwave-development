# cinc-console in the docker-e2e stack

Status: draft for approval
Base: branch `feat/webui-key`, stacked on the optional WebUI keypair work

## Intent

Give the e2e stack a browser-facing console so the WebUI keypair feature is
exercised the way a real deployment uses it: `cinc-console` logs a user in
against microwave-server, signs every later request with the WebUI private key
and `X-Ops-Request-Source: web`, and microwave-server enforces that user's ACLs.

The console is a fixture. It is not hardened for production, but it keeps the
upstream image's runtime hardening so the stack still resembles a real
deployment.

## Goals

- `docker compose up` brings up a console at `127.0.0.1:${MW_CONSOLE_PORT:-18081}`
  that can log in and browse the `e2e` organization.
- `examples/docker-e2e/microwave/microwave.toml` sets `auth.webui_key = true`.
- The console gets the materialized `webui_priv.pem`, never `pivotal.pem`.
- A seeded, documented demo user (`e2e-console`) can log in; it is an org
  member and in `admins`.
- The README documents the new service, the credentials, the fixture status,
  and what a key rotation does to the running stack.

## Non-goals

- TLS, a reverse proxy, or a non-loopback bind.
- LDAP or any auth mode other than `local`.
- Browser automation; a human opens the URL and logs in.
- Production hardening beyond keeping the upstream defaults (non-root,
  read-only rootfs, dropped capabilities).
- A console-side key rotation flow.

## Anti-goals

- The console holding `pivotal.pem`, which would hide whether the WebUI key
  path works.
- Baking a private key into an image or committing one.
- Making the existing node/converge e2e flow depend on the console.

## Constraints

- The console image runs as uid 1000 on a read-only rootfs
  (`ghcr.io/cinc-project/cinc-console`, `deploy/compose` upstream). The
  materialized key is `0600` under root in the `microwave-data` volume.
- The console validates its configuration at boot and exits when it is missing:
  `CINC_SERVER_URL`, one of `CINC_WEBUI_KEY`/`CINC_WEBUI_KEY_FILE`, and one of
  `SESSION_SECRET`/`SESSION_SECRET_FILE`.
- The image is published on GHCR and pulled, not built.
- Plain HTTP: the browser gets `localhost`, so a `Secure` cookie works, but the
  stack is also reachable through a LAN address; set
  `SESSION_COOKIE_SECURE=false` and say so.
- `CINC_SERVER_URL` takes the server root without `/organizations/<org>`; the
  console builds org paths itself.
- Login posts to `/authenticate_user`, which the server accepts only with a
  signing actor. The console signs those calls as `CINC_AUTH_ACTOR=pivotal`.

## Approaches

### Key delivery

- **A — handoff init service (chosen).** A short-lived `console-key` service
  copies the key into a dedicated volume as `1000:1000`, `0400`. The console
  stays at upstream hardening. Cost: one extra service and a stale copy after a
  rotation until the next `up`.
- **B — console as root.** No extra service, but the console loses the
  hardening parity and sees the whole data directory.
- **C — inline `CINC_WEBUI_KEY` through an entrypoint.** Needs root for the
  same reason as B, plus a shell wrapper, so it buys nothing over B.

Chosen: A.

### Demo user

- **A — seed creates `e2e-console` (chosen).** Fixed password, deleted and
  recreated on every seed run like the node fixtures, org member and `admins`.
  Deterministic, and the login does not depend on reading a generated secret.
- **B — use `pivotal` and its generated password.** No seed step, but the human
  has to extract the password and `pivotal` is not an org member.

Chosen: A.

## Design

### `microwave/microwave.toml`

```toml
[auth]
webui_key = true
```

The default `auth.webui_key_path` keeps the key inside `data_dir`, so the
materialized file is reachable from the `microwave-data` volume.

### `console-key` service

- `alpine` with `curl`-free `sh`; depends on `microwave: service_healthy`.
- Mounts `microwave-data:/mw-data:ro` and a new `console-key` volume at `./key`.
- Copies `/mw-data/webui_priv.pem` to `/key/webui_priv.pem`, `chown 1000:1000`,
  `chmod 0400`, and fails loudly when the source is missing.
- Exits; `restart: "no"`.

### `cinc-console` service

- `image: ${CINC_CONSOLE_IMAGE:-ghcr.io/cinc-project/cinc-console:0.9.0}`
- `user: "1000:1000"`, `read_only: true`, `cap_drop: [ALL]`,
  `security_opt: [no-new-privileges:true]`, `init: true`, tmpfs `/tmp` and
  `/app/.next/cache`.
- `depends_on`: `microwave: service_healthy`, `console-key: service_completed_successfully`,
  `seed: service_completed_successfully`.
- Environment: `CINC_SERVER_URL=http://microwave:8000`,
  `CINC_WEBUI_KEY_FILE=/key/webui_priv.pem`, `SESSION_SECRET` default
  `e2e-console-session-secret-32-chars-min`, `SESSION_COOKIE_SECURE=false`,
  `CINC_AUTH_ACTOR=pivotal`, `NODE_ENV=production`, `PORT=3000`,
  `HOSTNAME=0.0.0.0`.
- `ports: ["127.0.0.1:${MW_CONSOLE_PORT:-18081}:3000"]`
- Healthcheck: `wget -qO- http://127.0.0.1:3000/api/healthz`.

### Seed

New step after the org exists and before the cookbook work:

- `MW_CONSOLE_USER` (default `e2e-console`), `MW_CONSOLE_PASSWORD` (default
  `e2e-console-password`), `MW_CONSOLE_EMAIL`.
- Delete the user (ignore absence), create it with the server-level config.
- `knife org user add "$ORG" "$user"` and `knife group add "$user" admins`,
  both with the org-level config.
- Append user/org to `seed-summary.txt`.

### Documentation

README: service list entry, usage with the published port and credentials, the
"fixtures, not production" note, key-rotation caveat, and the new environment
variables in the table. No production docs change.

## Left to the builder

- Exact `knife user create` arguments and whether `knife group add` needs the
  `-F json` follow-up on this server.
- Whether the healthcheck needs a start period beyond the image's own.
- Whether `console-key` needs a retry loop instead of relying on the health
  gate.
- Whether the seed's user reset needs to remove the user's keys first.
