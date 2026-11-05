# Docker-e2e Pull-or-Build Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the e2e stack pull `ghcr.io/rmoriz/microwave-server:latest` by default, keep a source build behind an explicit Compose override, and publish a docs page that gets a newcomer from checkout to a running stack.

**Architecture:** The base Compose file defines `microwave` with `image` only. A `docker-compose.build.yml` overlay adds `build` and changes the `MW_IMAGE` default to the local tag, so merging both files selects the source path and the base default stays a real pull. `docs/docker-e2e.md` is the operator page; it is included in the Docusaurus build and its service, port and login list is asserted against the Compose file.

**Tech Stack:** Docker Compose, Python `unittest`, Docusaurus, Markdown.

**Spec:** `docs/superpowers/specs/2026-11-05-docker-e2e-pull-or-build-design.md`

## Global Constraints

- Base pull default is `ghcr.io/rmoriz/microwave-server:latest`.
- Build default is `microwave-server:e2e`; the overlay must not write the GHCR tag unless `MW_IMAGE` pins one.
- `docker compose up -d` must not compile the server.
- The project name and all volumes stay in the base file.
- Source path: `docker compose -f docker-compose.yml -f docker-compose.build.yml …`.
- No wrapper script.
- The docs page starts from `git clone https://github.com/rmoriz/microwave-development.git`
  and never assumes an existing checkout.
- The docs page names every service with its container name and whether it
  publishes a port; loopback-only stays loopback.
- Fixture logins are marked as fixtures, never as production credentials.

## Review Focus

- Base `docker compose config` must show the GHCR image and no `build` key.
- Overlay `docker compose config` must show the local image and the root build context.
- A pinned `MW_IMAGE` must win in both modes.
- `docker compose up -d` must not build `microwave` implicitly.
- The workstation must still build locally and the project name must stay `microwave-e2e`.
- The docs page must list exactly the services of the Compose file and the
  effective port and login defaults; the drift test fails when either side moves.
- The docs page must build in Docusaurus with a working sidebar entry.

---

### Task 1: Split pull and build Compose files

**Files:**
- Modify: `docker-e2e/docker-compose.yml`
- Create: `docker-e2e/docker-compose.build.yml`
- Create: `scripts/tests/test_docker_e2e_compose.py`

**Interfaces:**
- Consumes: the root `Dockerfile` and its `RUST_VERSION` arg.
- Produces: base mode image `ghcr.io/rmoriz/microwave-server:latest`; overlay mode image `microwave-server:e2e` with `build.context` at the repo root.

- [ ] **Step 1: Write the failing config test**

Create `scripts/tests/test_docker_e2e_compose.py`. It uses
`subprocess.run` with `cwd=ROOT / "docker-e2e"`, skips the class when
`docker compose version` fails, and asserts:

```python
def test_pull_is_the_default(self):
    service = self.base()["services"]["microwave"]
    self.assertEqual(service["image"], "ghcr.io/rmoriz/microwave-server:latest")
    self.assertNotIn("build", service)

def test_overlay_builds_locally(self):
    service = self.overlay()["services"]["microwave"]
    self.assertEqual(service["image"], "microwave-server:e2e")
    self.assertEqual(pathlib.Path(service["build"]["context"]), ROOT)
    self.assertTrue(service["build"]["dockerfile"].endswith("Dockerfile"))
    self.assertEqual(str(service["build"]["args"]["RUST_VERSION"]), "1")

def test_pinned_image_wins_in_both_modes(self):
    pinned = "ghcr.io/rmoriz/microwave-server:0.3.1"
    env = {**os.environ, "MW_IMAGE": pinned}
    self.assertEqual(self.base(env)["services"]["microwave"]["image"], pinned)
    self.assertEqual(self.overlay(env)["services"]["microwave"]["image"], pinned)

def test_project_and_workstation_stay(self):
    config = self.base()
    self.assertEqual(config["name"], "microwave-e2e")
    self.assertIn("build", config["services"]["workstation"])
```

Helpers run `docker compose config --format json`, the overlay helper adds
`-f docker-compose.yml -f docker-compose.build.yml` before `config`, and
`json.loads` the stdout. Pass `env=` through.

- [ ] **Step 2: Run the test to verify it fails**

Run: `python3 -m unittest scripts.tests.test_docker_e2e_compose -v`
Expected: FAIL because the base service still has `build` and the overlay file
does not exist.

- [ ] **Step 3: Make the base file pull-only**

Replace the `microwave.build` block with:

```yaml
    image: ${MW_IMAGE:-ghcr.io/rmoriz/microwave-server:latest}
```

Keep the existing `image` as the only image line for the service. Update the
top comment to describe the two modes and their commands.

- [ ] **Step 4: Add the overlay**

Create `docker-e2e/docker-compose.build.yml`:

```yaml
# Source-build overlay for the e2e stack:
#   docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
#
# The local tag keeps a source build from overwriting the pulled
# ghcr.io/...:latest image. Set MW_IMAGE to pin a different tag.
services:
  microwave:
    build:
      context: ..
      dockerfile: Dockerfile
      args:
        RUST_VERSION: ${RUST_VERSION:-1}
    image: ${MW_IMAGE:-microwave-server:e2e}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `python3 -m unittest scripts.tests.test_docker_e2e_compose -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add docker-e2e/docker-compose.yml docker-e2e/docker-compose.build.yml scripts/tests/test_docker_e2e_compose.py
git commit -m "feat(docker-e2e): pull the image by default and build on demand"
```

---

### Task 2: Document both modes

**Files:**
- Modify: `docker-e2e/README.md`

- [ ] **Step 1: Update the service description and usage**

Change the `microwave` description to say it is pulled from
`ghcr.io/rmoriz/microwave-server:latest` by default and built from the
repository root with the overlay.

Replace the usage block with:

```sh
cd docker-e2e
docker compose up -d
```

Add directly after it that this pulls the moving `latest`, that a version can
be pinned with
`MW_IMAGE=ghcr.io/rmoriz/microwave-server:0.3.1 docker compose up -d`, and
that a source build is:

```sh
docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build
```

Keep a separate sentence that only `workstation/**` changes need `--build`,
because that image is still local-only.

- [ ] **Step 2: Document `MW_IMAGE`**

Add a `MW_IMAGE` row to the configuration table: pull default
`ghcr.io/rmoriz/microwave-server:latest`, build default
`microwave-server:e2e`, and the pin form from Step 1.

Add one note under "Notes and trade-offs" that `latest` moves and the source
overlay exists for server changes.

- [ ] **Step 3: Verify**

Run: `python3 -m unittest scripts.tests.test_docker_e2e_compose -v`
Expected: PASS.

Run: `python3 scripts/check-claims.py`
Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add docker-e2e/README.md
git commit -m "docs(docker-e2e): explain pull and source-build modes"
```

---

### Task 3: Publish the quick-start docs page

**Files:**
- Create: `docs/docker-e2e.md`
- Modify: `website/docs/sidebars.ts`
- Modify: `website/docs/docusaurus.config.ts`
- Modify: `docs/claims.toml`
- Modify: `scripts/tests/test_docker_e2e_compose.py`

**Interfaces:**
- Consumes: the service names, port defaults and fixture logins from the base
  Compose file after Task 1.
- Produces: the Docusaurus route `/docker-e2e`; no code interface.

- [ ] **Step 1: Write the failing page test**

Add a `DocsPageTest` to `scripts/tests/test_docker_e2e_compose.py`. It reads
`docs/docker-e2e.md` and asserts:

```python
def test_page_names_every_service_and_no_other(self):
    compose = yaml.safe_load((ROOT / "docker-e2e" / "docker-compose.yml").read_text())
    for service in compose["services"]:
        self.assertIn(f"microwave-e2e-{service}-1", self.page)
    names = set(re.findall(r"microwave-e2e-([a-z-]+)-1", self.page))
    self.assertEqual(names, set(compose["services"]))

def test_page_quick_start_and_ports(self):
    self.assertIn("git clone https://github.com/rmoriz/microwave-development.git", self.page)
    self.assertIn("cd docker-e2e", self.page)
    self.assertIn("docker compose up -d", self.page)
    self.assertIn("18080", self.page)
    self.assertIn("127.0.0.1:18081", self.page)

def test_page_logins_and_build_path(self):
    self.assertIn("e2e-console", self.page)
    self.assertIn("e2e-console-password", self.page)
    self.assertIn("/var/lib/microwave/pivotal.password", self.page)
    self.assertIn("docker compose -f docker-compose.yml -f docker-compose.build.yml up -d --build", self.page)
    self.assertIn("docker compose down -v", self.page)
```

Import `yaml` and `re` at the top of the file. Run
`python3 -m unittest scripts.tests.test_docker_e2e_compose -v` and expect the
`DocsPageTest` cases to fail on the missing file.

- [ ] **Step 2: Write the page**

Create `docs/docker-e2e.md`. Sections in this order:

1. Intro: what the stack proves, that it runs entirely in containers, and that
   fixtures are not production credentials.
2. Prerequisites: Docker Engine with Compose v2, a couple of GB of disk, network
   during the first start. No Rust toolchain, because the default pulls.
3. Quick start with the clone URL, `cd docker-e2e` and `docker compose up -d`,
   plus the `docker compose logs -f seed` and node log follow-up. State that
   `seed` must finish and that the nodes start on their own afterwards.
4. Services table with three columns: container, purpose, published port. Every
   container is `microwave-e2e-<service>-1`; list all nine services, mark the
   published API port `18080` and the loopback-only console `127.0.0.1:18081`,
   and `none` for the rest.
5. Logins table: console `http://127.0.0.1:18081` with `e2e-console` /
   `e2e-console-password`; superuser `pivotal` with
   `docker compose exec microwave cat /var/lib/microwave/pivotal.password`; MCP
   bearer `e2e-mcp-token`, already `$MW_MCP_TOKEN` inside `workstation`. Say the
   nodes register themselves and need no login.
6. Access paths: `./shell` or `docker compose exec workstation bash` for the
   workstation, `docker compose exec node-debian bash` for a node, and the
   status probe `curl -fsS http://127.0.0.1:18080/_status`.
7. Pin a release with
   `MW_IMAGE=ghcr.io/rmoriz/microwave-server:0.3.1 docker compose up -d`, and
   note that the untagged default `latest` moves.
8. Source build with the overlay command from Task 1.
9. Tear down with `docker compose down -v`.
10. "Limits" list: the fixtures, the moving `latest`, the privileged node
    containers and the API port listening on all interfaces.
11. Links to [`docker-e2e/README.md`](../docker-e2e/README.md) and
    [`deployment.md`](deployment.md).

Use the repository's prose style: short factual sentences, no marketing.

- [ ] **Step 3: Wire it into the site**

Add `{type: 'doc', id: 'docker-e2e', label: 'Docker e2e stack'}` to the
`Operator guides` category in `website/docs/sidebars.ts`, right after
`load-test`.

Add `'docker-e2e.md'` to the `include` list in
`website/docs/docusaurus.config.ts`, right after `'load-test.md'`.

- [ ] **Step 4: Give the page claim coverage**

Add three `[[claim]]` entries to `docs/claims.toml` in the `tooling` area: the
pull default with the pin form, the loopback console and its fixture login, and
the source-build overlay with the teardown. Each `verify` is `kind = "grep"`
against `docker-e2e/docker-compose.yml` or `docker-e2e/README.md` for the fact
the page states, and each `surface` needle is a sentence that exists verbatim in
`docs/docker-e2e.md`.

- [ ] **Step 5: Verify**

```bash
python3 -m unittest scripts.tests.test_docker_e2e_compose -v
python3 scripts/check-claims.py
cd website/docs && npm run build && npm run typecheck
```

Expected: tests and claims pass; the build renders `/docker-e2e/` and the
typecheck is clean.

- [ ] **Step 6: Commit**

```bash
git add docs/docker-e2e.md docs/claims.toml website/docs/sidebars.ts \
  website/docs/docusaurus.config.ts scripts/tests/test_docker_e2e_compose.py
git commit -m "docs(e2e): publish a docker-e2e quick start"
```

---

### Task 4: Final verification

- [ ] **Step 1: Run the suite**

```bash
python3 -m unittest discover -s scripts/tests -v
python3 scripts/check-claims.py
cd website/docs && npm run build && npm run typecheck
```

Expected: all pass.

- [ ] **Step 2: Check both rendered configs**

```bash
cd docker-e2e
docker compose config --images
docker compose -f docker-compose.yml -f docker-compose.build.yml config --images
```

Expected: first prints `ghcr.io/rmoriz/microwave-server:latest`; second prints
`microwave-server:e2e`. Neither prints anything else for `microwave`.

- [ ] **Step 3: Prove both paths**

```bash
docker compose pull microwave
docker compose -f docker-compose.yml -f docker-compose.build.yml build microwave
```

Expected: the pull fetches `latest`; the build compiles from the root context
and tags `microwave-server:e2e`.

- [ ] **Step 4: Confirm pull did not compile**

Run `docker image inspect ghcr.io/rmoriz/microwave-server:latest` and check
that the local `microwave-server:e2e` image exists only after Step 3's build.
Expected: pull creates no local e2e tag.
