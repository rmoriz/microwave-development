# cinc-console e2e Fixture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the official `cinc-console` container to the docker-e2e stack, fed by the WebUI keypair, with a seeded demo login.

**Architecture:** The console is a separate hardened container that reads the materialized `webui_priv.pem` from a dedicated volume populated by a short-lived `console-key` handoff service. The seed creates the demo user; `microwave.toml` enables `auth.webui_key`.

**Tech Stack:** Docker Compose v2, Alpine shell for the handoff, knife for the seed fixtures, `ghcr.io/cinc-project/cinc-console`.

**Spec:** `docs/superpowers/specs/2026-11-04-cinc-console-e2e-design.md`

## Global Constraints

- Base branch is `feat/webui-key`; do not touch the main checkout.
- The console is a fixture, not a production deployment: loopback-only bind, fixed credentials.
- The console must never receive `pivotal.pem`; only `webui_priv.pem` reaches it, mode `0400`, owner `1000:1000`.
- Console hardening stays at upstream defaults: `user: "1000:1000"`, `read_only: true`, `cap_drop: [ALL]`, `security_opt: no-new-privileges`, `init: true`.
- `SESSION_COOKIE_SECURE=false` because the stack is plain HTTP.
- Console port is `127.0.0.1:${MW_CONSOLE_PORT:-18081}:3000`.
- Demo credentials: `MW_CONSOLE_USER=e2e-console`, `MW_CONSOLE_PASSWORD=e2e-console-password`, `MW_CONSOLE_EMAIL=e2e-console@example.com`.
- `auth.webui_key = true` in `examples/docker-e2e/microwave/microwave.toml`.
- No new files outside `examples/docker-e2e/` and its README.

## Review Focus

1. Console boots before the key exists, or with an unset/too-short `SESSION_SECRET`: the container must not crash-loop; pinned by Task 3's dependency and healthcheck steps.
2. `SESSION_COOKIE_SECURE` left at the production default: the session cookie must not carry `Secure` over plain HTTP; pinned by Task 3's login/cookie assertion.
3. Key file ownership or mode wrong for uid 1000: the console must be able to read it; pinned by Task 2's `stat` assertion.
4. Seed run twice: the user fixture must not fail on an existing user; pinned by Task 1's second-run step.
5. Console seeing more than the key: the mount must expose only the key file; pinned by Task 3's mount inspection.

---

### Task 1: Seed the demo console user

**Files:**
- Modify: `examples/docker-e2e/seed/seed.sh`

**Interfaces:**
- Consumes: `$SHARED/knife-server.rb` and `$SHARED/knife.rb` written earlier in the script; the existing `log`, `SERVER`, `ORG` variables.
- Produces: user `$CONSOLE_USER` (default `e2e-console`) with `$CONSOLE_PASSWORD`, org member, member of `admins`; `console_user`/`console_password` lines in `seed-summary.txt`; the same values exposed to the README in Task 4.

- [ ] **Step 1: Add the variables next to the existing fixture variables**

```sh
# Demo console login. Fixture credentials, not production.
CONSOLE_USER="${MW_CONSOLE_USER:-e2e-console}"
CONSOLE_PASSWORD="${MW_CONSOLE_PASSWORD:-e2e-console-password}"
CONSOLE_EMAIL="${MW_CONSOLE_EMAIL:-e2e-console@example.com}"
```

- [ ] **Step 2: Add the reset-and-create section after the org-level `knife.rb` block (before the node reset)**

```sh
# ---- 3. Console demo user ---------------------------------------------------
# Recreate on every run so the documented password stays true. Absence on the
# first run is not an error.
log "resetting console user $CONSOLE_USER"
knife user delete "$CONSOLE_USER" -y --config "$SHARED/knife-server.rb" >/dev/null 2>&1 || true
knife user create "$CONSOLE_USER" \
    --email "$CONSOLE_EMAIL" \
    --password "$CONSOLE_PASSWORD" \
    --config "$SHARED/knife-server.rb"
knife org user add "$ORG" "$CONSOLE_USER" --config "$SHARED/knife.rb"
knife group add "$CONSOLE_USER" admins --config "$SHARED/knife.rb"
```

- [ ] **Step 3: Renumber the following section comments**

The old `3.`–`7.` become `4.`–`8.` and the header comment list gains the new step. Keep every other line unchanged.

- [ ] **Step 4: Extend the summary**

Add to the `seed-summary.txt` heredoc:

```sh
console_user: $CONSOLE_USER
console_password: $CONSOLE_PASSWORD
```

- [ ] **Step 5: Start the server and run the seed twice**

```sh
cd examples/docker-e2e
docker compose up -d microwave
docker compose run --rm seed
docker compose run --rm seed
```

Expected: both runs end with `seed: done`; the second run logs `resetting console user e2e-console` and does not fail on the existing user.

- [ ] **Step 6: Verify the user, org membership and group**

```sh
docker compose run --rm --no-deps workstation knife user show e2e-console
docker compose run --rm --no-deps workstation knife group show admins
```

Expected: the user exists with `e2e-console@example.com`; `admins` lists `e2e-console`. If `knife group show` lacks a JSON field for users, verify with
`docker compose run --rm --no-deps workstation knife group show admins -F json` and confirm the name appears in the output.

- [ ] **Step 7: Commit**

```sh
git add examples/docker-e2e/seed/seed.sh
git commit -m "test(e2e): seed a demo console user"
```

---

### Task 2: Hand the WebUI key to the console volume

**Files:**
- Modify: `examples/docker-e2e/docker-compose.yml`
- Modify: `examples/docker-e2e/microwave/microwave.toml`

**Interfaces:**
- Consumes: the server's materialized key at `/mw-data/webui_priv.pem`.
- Produces: volume `console-key` with `/key/webui_priv.pem` owned `1000:1000`, mode `0400`; service `console-key` that other services use as a completion dependency; `auth.webui_key = true` in the e2e config.

- [ ] **Step 1: Enable the WebUI key in the e2e config**

Append to `examples/docker-e2e/microwave/microwave.toml`:

```toml
[auth]
# The console signs web impersonation with its own keypair instead of
# pivotal.pem. Created on first start in the data directory.
webui_key = true
```

- [ ] **Step 2: Add the `console-key` service after `seed`**

```yaml
  # Copies the materialized WebUI key out of the data volume for the console
  # user (uid 1000). The console cannot read the 0600 root-owned original.
  console-key:
    image: ${ALPINE_IMAGE:-alpine:3.22}
    depends_on:
      microwave:
        condition: service_healthy
    volumes:
      - microwave-data:/mw-data:ro
      - console-key:/key
    command:
      [
        "sh",
        "-c",
        "set -e; test -s /mw-data/webui_priv.pem; cp /mw-data/webui_priv.pem /key/webui_priv.pem; chown 1000:1000 /key/webui_priv.pem; chmod 0400 /key/webui_priv.pem",
      ]
    restart: "no"
```

- [ ] **Step 3: Declare the volume**

Add `console-key:` to the `volumes:` block.

- [ ] **Step 4: Rebuild the server and run the handoff**

```sh
cd examples/docker-e2e
docker compose up -d --build microwave
docker compose up --exit-code-from console-key console-key
```

Expected: exit code 0.

- [ ] **Step 5: Verify the handed-off file**

The Compose project is named `microwave-e2e`, so the volume is
`microwave-e2e_console-key`:

```sh
docker run --rm -v microwave-e2e_console-key:/key:ro alpine:3.22 stat -c '%u:%g %a' /key/webui_priv.pem
```

Expected: `1000:1000 400`.

- [ ] **Step 6: Verify the server trusts the key**

```sh
docker compose exec microwave sh -c 'test -s /var/lib/microwave/webui_pub.pem && echo pub-present'
docker compose logs microwave | grep -c 'webui keypair is enabled'
```

Expected: `pub-present` and a count of at least 1.

- [ ] **Step 7: Commit**

```sh
git add examples/docker-e2e/docker-compose.yml examples/docker-e2e/microwave/microwave.toml
git commit -m "test(e2e): hand the webui key to the console"
```

---

### Task 3: Run the console

**Files:**
- Modify: `examples/docker-e2e/docker-compose.yml`

**Interfaces:**
- Consumes: `console-key` (Task 2), the seeded user (Task 1).
- Produces: service `cinc-console` on `127.0.0.1:${MW_CONSOLE_PORT:-18081}` with `CINC_WEBUI_KEY_FILE=/key/webui_priv.pem`.

- [ ] **Step 1: Add the `cinc-console` service after `console-key`**

```yaml
  # Browser UI for the stack. Fixture credentials from the seed; the key is
  # mounted read-only from the handoff volume.
  cinc-console:
    image: ${CINC_CONSOLE_IMAGE:-ghcr.io/cinc-project/cinc-console:0.9.0}
    depends_on:
      microwave:
        condition: service_healthy
      console-key:
        condition: service_completed_successfully
      seed:
        condition: service_completed_successfully
    user: "1000:1000"
    read_only: true
    cap_drop: [ALL]
    security_opt:
      - no-new-privileges:true
    init: true
    tmpfs:
      - /tmp:size=64m,mode=1777
      - /app/.next/cache:size=128m,mode=1777
    environment:
      NODE_ENV: production
      HOSTNAME: 0.0.0.0
      PORT: "3000"
      CINC_SERVER_URL: http://microwave:8000
      CINC_WEBUI_KEY_FILE: /key/webui_priv.pem
      # Fixture, fixed on purpose so repeated runs keep the session secret.
      SESSION_SECRET: ${MW_CONSOLE_SESSION_SECRET:-e2e-console-session-secret-32-chars-min}
      # The stack is plain HTTP.
      SESSION_COOKIE_SECURE: "false"
      # /authenticate_user is signed by the superuser.
      CINC_AUTH_ACTOR: pivotal
    volumes:
      - console-key:/key:ro
    ports:
      - "127.0.0.1:${MW_CONSOLE_PORT:-18081}:3000"
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:3000/api/healthz >/dev/null"]
      interval: 10s
      timeout: 3s
      retries: 12
      start_period: 20s
```

- [ ] **Step 2: Bring the stack up**

```sh
cd examples/docker-e2e
docker compose up -d
docker compose ps cinc-console
```

Expected after the start period: `healthy`, no restart loop.

- [ ] **Step 3: Verify the console cannot read anything but the key**

```sh
docker compose exec cinc-console ls /key
docker compose exec cinc-console sh -c 'test ! -e /mw-data/pivotal.pem && echo no-pivotal'
```

Expected: only `webui_priv.pem`; `no-pivotal`.

- [ ] **Step 4: Verify health and the root page**

```sh
curl -fsS http://127.0.0.1:18081/api/healthz
curl -fsS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:18081/
```

Expected: `{"status":"ok"}` and `200`.

- [ ] **Step 5: Verify the login and the cookie flags**

```sh
curl -fsS -D /tmp/console-headers -c /tmp/console-cookie \
  -H 'content-type: application/json' \
  -H 'Origin: http://127.0.0.1:18081' \
  -d '{"username":"e2e-console","password":"e2e-console-password"}' \
  http://127.0.0.1:18081/api/auth/login
grep -i '^set-cookie' /tmp/console-headers
curl -fsS -b /tmp/console-cookie http://127.0.0.1:18081/organizations/e2e | grep -qi 'node'
```

Expected: `{"ok":true}`; the `Set-Cookie` line has `HttpOnly` and no `Secure`; the org page renders (200 and a node/object reference).

- [ ] **Step 6: Verify a wrong password is rejected**

```sh
curl -s -o /dev/null -w '%{http_code}\n' -H 'content-type: application/json' \
  -H 'Origin: http://127.0.0.1:18081' \
  -d '{"username":"e2e-console","password":"wrong"}' \
  http://127.0.0.1:18081/api/auth/login
```

Expected: `401`.

- [ ] **Step 7: Commit**

```sh
git add examples/docker-e2e/docker-compose.yml
git commit -m "test(e2e): run cinc-console against the stack"
```

---

### Task 4: Document the console

**Files:**
- Modify: `examples/docker-e2e/README.md`

**Interfaces:**
- Consumes: the service names, port and credentials from Tasks 1–3.

- [ ] **Step 1: Add the service to the intro list**

Extend the numbered list and the Compose comment block with `cinc-console` — "cinc-console — the official web UI, logged in with the seeded demo user".

- [ ] **Step 2: Add a "Console" section after "Workstation"**

State: URL `http://127.0.0.1:18081`, user `e2e-console`, password `e2e-console-password`, that it signs with the WebUI key (not `pivotal.pem`), that both are fixtures, and that `MW_CONSOLE_PORT`, `MW_CONSOLE_USER`, `MW_CONSOLE_PASSWORD`, `MW_CONSOLE_SESSION_SECRET`, `CINC_CONSOLE_IMAGE` exist.

- [ ] **Step 3: Document the rotation consequence**

Add to the trade-offs section: a `rotate-credentials webui` invalidates the running console until `docker compose up` re-runs `console-key`; sessions keep working but server calls fail with 401 in the meantime.

- [ ] **Step 4: Add the new variables to the configuration table**

Rows for `MW_CONSOLE_PORT` (default `18081`), `MW_CONSOLE_USER`, `MW_CONSOLE_PASSWORD`, `MW_CONSOLE_SESSION_SECRET`, `CINC_CONSOLE_IMAGE`.

- [ ] **Step 5: Verify links and commands**

```sh
cd examples/docker-e2e
grep -n 'cinc-console\|18081\|e2e-console' README.md
docker compose config >/dev/null
```

Expected: the new mentions are present; the compose file still validates.

- [ ] **Step 6: Commit**

```sh
git add examples/docker-e2e/README.md
git commit -m "docs(e2e): document the console fixture"
```
