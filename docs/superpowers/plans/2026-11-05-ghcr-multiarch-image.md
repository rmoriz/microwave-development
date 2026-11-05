# GHCR Multi-Arch Release Image Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish arch-native `amd64` and `arm64` container images from the release matrix's already-built binaries, merged into one GHCR manifest on every `vX.Y.Z` tag.

**Architecture:** The release workflow becomes tag-only. A `meta` job turns the tag into version outputs and rejects non-semver tags. Each existing native matrix leg stages its three binaries into a temp Docker context, builds the `release` target of the shared-runtime `Dockerfile`, smoke-tests it, and pushes an arch-suffixed tag. A `manifest` job merges both digests into the exact, minor and (stable only) `latest` tags; `publish` waits for it.

**Tech Stack:** GitHub Actions, Bash, Python `unittest`, Docker, Debian `trixie-slim`, GHCR.

**Spec:** `docs/superpowers/specs/2026-11-05-ghcr-multiarch-image-design.md`

## Global Constraints

- Image name is `ghcr.io/rmoriz/microwave-server`; registry login uses `${{ github.actor }}` and `${{ secrets.GITHUB_TOKEN }}`.
- Release trigger is `push` on tags `["v*"]` only; `workflow_dispatch` is removed.
- A valid tag is `^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`. Anything else fails before a build.
- `latest` is written only for a stable tag (no `-` suffix); pre-releases write the exact version only.
- The release Docker target compiles nothing and copies only staged binaries.
- Base is `debian:trixie-slim`; UID/GID stay `10001`; the existing runtime contract and healthcheck stay unchanged.
- The default `docker build .` target keeps the Rust builder.
- `packages: write` is granted to the image-producing jobs only.
- The GitHub Release waits for a successful manifest.

## Review Focus

- A pre-release tag (`v1.2.3-rc.1`) must not move `latest`; the resolver reports `stable=0`.
- A tag such as `v1.2`, `vnext` or `1.2.3` must fail instead of publishing under a guessed version.
- A plain local `docker build .` must still compile and produce the same runtime contract.
- Each arch tag must receive the binary built by that matrix leg; a mismatched binary is caught by the smoke test.
- No GitHub Release may exist without both architecture images and the manifest.

---

### Task 1: Tag resolver

**Files:**
- Create: `scripts/release-image-tags.sh`
- Create: `scripts/tests/test-release-image-tags.sh`

**Interfaces:**
- Consumes: one argument, the git tag.
- Produces: `version=<x.y.z[-pre]>`, `major_minor=<x.y>` and `stable=<1|0>` on stdout, one `key=value` per line; exit 1 with a message on stderr for an invalid tag.

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
resolver="$root/scripts/release-image-tags.sh"
fail=0

assert_output() {
    local tag="$1" expected="$2"
    local out
    out="$("$resolver" "$tag")" || { echo "FAIL: $tag exited non-zero"; fail=1; return; }
    if [ "$out" != "$expected" ]; then
        printf 'FAIL: %s\n  expected: %q\n  actual:   %q\n' "$tag" "$expected" "$out"
        fail=1
    fi
}

assert_output "v1.2.3" $'version=1.2.3\nmajor_minor=1.2\nstable=1'
assert_output "v0.3.0-rc.1" $'version=0.3.0-rc.1\nmajor_minor=0.3\nstable=0'
assert_output "v10.20.30-alpha.2" $'version=10.20.30-alpha.2\nmajor_minor=10.20\nstable=0'

for bad in "1.2.3" "v1.2" "vnext" "v1.2.3.4"; do
    if "$resolver" "$bad" >/dev/null 2>&1; then
        echo "FAIL: $bad should have failed"
        fail=1
    fi
done

exit "$fail"
```

- [ ] **Step 2: Run it to verify it fails**

Run: `bash scripts/tests/test-release-image-tags.sh`
Expected: FAIL because `scripts/release-image-tags.sh` does not exist.

- [ ] **Step 3: Implement `scripts/release-image-tags.sh`**

Bash, `set -euo pipefail`, executable. Validate with
`^v([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.-]+)?$`; strip the leading `v`;
set `stable=1` unless the version contains `-`; print the three lines in the
order above. Reject a missing argument with usage.

- [ ] **Step 4: Run the test to verify it passes**

Run: `bash scripts/tests/test-release-image-tags.sh`
Expected: exit 0 with no output.

- [ ] **Step 5: Commit**

```bash
git add scripts/release-image-tags.sh scripts/tests/test-release-image-tags.sh
git commit -m "feat(release): resolve image tags from a release tag"
```

---

### Task 2: Shared-runtime Dockerfile with release target

**Files:**
- Modify: `Dockerfile`
- Create: `scripts/tests/test_release_image.py`

**Interfaces:**
- Consumes: a build context containing `microwave-server`, `microwave-ctl`, `microwave-migrate` and `packaging/microwave.toml.example`.
- Produces: Docker target `release`; target `local` stays the default (last stage). Both share the `runtime` stage.

- [ ] **Step 1: Write the failing Dockerfile test**

Create `scripts/tests/test_release_image.py` with a `DockerfileTest` that
reads `Dockerfile` and asserts:

```python
def test_local_is_the_last_stage(self):
    stages = [line for line in self.text.splitlines() if line.startswith("FROM ")]
    self.assertEqual(stages[-1], "FROM runtime AS local")

def test_release_does_not_use_the_builder(self):
    release = self.text.split("FROM runtime AS release", 1)[1].split("FROM runtime AS local", 1)[0]
    self.assertNotIn("--from=builder", release)
    self.assertIn("COPY --chmod=0755 microwave-server", release)

def test_runtime_contract_is_shared(self):
    runtime = self.text.split("FROM debian:trixie-slim AS runtime", 1)[1].split("FROM runtime AS release", 1)[0]
    self.assertIn("USER microwave:microwave", runtime)
    self.assertIn("HEALTHCHECK", runtime)
    self.assertIn("packaging/microwave.toml.example", runtime)
    self.assertIn("org.opencontainers.image.version", runtime)
```

- [ ] **Step 2: Run it to verify it fails**

Run: `python3 -m unittest scripts.tests.test_release_image.DockerfileTest -v`
Expected: FAIL because `FROM runtime AS release` is missing.

- [ ] **Step 3: Restructure `Dockerfile`**

Keep the Rust builder stage. Move the existing runtime stage under
`FROM debian:trixie-slim AS runtime`. In that stage add build args
`VERSION=dev`, `REVISION=unknown`, `CREATED=1970-01-01T00:00:00Z` and
`SOURCE=""`, and OCI labels for title, description, licences, source,
revision, version and created. Add the existing setup, config copy,
healthcheck and default command to `runtime`.

After it add:

```dockerfile
FROM runtime AS release
COPY --chmod=0755 microwave-server /usr/local/bin/microwave-server
COPY --chmod=0755 microwave-ctl /usr/local/bin/microwave-ctl
COPY --chmod=0755 microwave-migrate /usr/local/bin/microwave-migrate
RUN ln -s microwave-ctl /usr/local/bin/chef-server-ctl

FROM runtime AS local
COPY --from=builder /out/microwave-server /usr/local/bin/microwave-server
COPY --from=builder /out/microwave-ctl /usr/local/bin/microwave-ctl
RUN ln -s microwave-ctl /usr/local/bin/chef-server-ctl
COPY --from=builder /out/microwave-migrate /usr/local/bin/microwave-migrate
```

The `local` stage must stay last. Keep the builder's cache-mount build step
unchanged.

- [ ] **Step 4: Run the test and a stub build**

Run: `python3 -m unittest scripts.tests.test_release_image.DockerfileTest -v`
Expected: PASS.

Then verify the release target with stub binaries:

```bash
tmp="$(mktemp -d)"
for b in microwave-server microwave-ctl microwave-migrate; do
  printf '#!/bin/sh\necho stub\n' > "$tmp/$b"; chmod +x "$tmp/$b"
done
mkdir -p "$tmp/packaging"
cp packaging/microwave.toml.example "$tmp/packaging/"
docker build --target release -t mw-release-check "$tmp"
docker run --rm mw-release-check microwave-server --version
docker rm -f mw-release-check 2>/dev/null || true
docker rmi mw-release-check
rm -rf "$tmp"
```

Expected: build succeeds, `--version` runs as UID `10001`, then image is removed.

- [ ] **Step 5: Commit**

```bash
git add Dockerfile scripts/tests/test_release_image.py
git commit -m "feat(container): add release target using staged binaries"
```

---

### Task 3: Tag-only release workflow with per-arch push and manifest

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `scripts/tests/test_release_image.py`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `scripts/release-image-tags.sh` from Task 1 and the `release` target from Task 2.
- Produces: `meta` outputs `version`, `major_minor`, `stable`; matrix emission `matrix.arch`; job names `build`, `manifest`, `publish`.

- [ ] **Step 1: Add failing workflow assertions**

Add a `WorkflowTest` to `scripts/tests/test_release_image.py`. In `setUp`,
load the YAML and normalize PyYAML's boolean `on` key:

```python
self.workflow = yaml.safe_load(path.read_text())
if True in self.workflow:
    self.workflow["on"] = self.workflow.pop(True)
```

Then assert:

```python
def test_trigger_is_tag_only(self):
    self.assertNotIn("workflow_dispatch", self.workflow["on"])
    self.assertEqual(self.workflow["on"]["push"]["tags"], ["v*"])

def test_meta_job_resolves_the_tag(self):
    meta = self.workflow["jobs"]["meta"]
    self.assertIn("scripts/release-image-tags.sh", yaml.safe_dump(meta))
    self.assertEqual(set(meta["outputs"]), {"version", "major_minor", "stable"})

def test_image_jobs_may_write_packages(self):
    for job in ("build", "manifest"):
        self.assertEqual(self.workflow["jobs"][job]["permissions"]["packages"], "write")

def test_build_order(self):
    names = [step.get("name", "") for step in self.workflow["jobs"]["build"]["steps"]]
    self.assertLess(names.index("Upload the archive"), names.index("Stage the image context"))
    self.assertLess(names.index("Stage the image context"), names.index("Build the architecture image"))
    self.assertLess(names.index("Build the architecture image"), names.index("Smoke-test the image"))
    self.assertLess(names.index("Smoke-test the image"), names.index("Log in to GHCR"))
    self.assertLess(names.index("Log in to GHCR"), names.index("Push the architecture image"))

def test_publish_waits_for_the_manifest(self):
    self.assertEqual(set(self.workflow["jobs"]["publish"]["needs"]), {"build", "manifest"})
```

Import `yaml` in the module.

- [ ] **Step 2: Run it to verify it fails**

Run: `python3 -m unittest scripts.tests.test_release_image.WorkflowTest -v`
Expected: FAIL because the workflow still has `workflow_dispatch` and no `meta` job.

- [ ] **Step 3: Rewrite the trigger and add `meta`**

Replace the trigger with:

```yaml
on:
  push:
    tags: ["v*"]
```

Add a `meta` job on `ubuntu-24.04` with `permissions: {contents: read}`,
a checkout, and one step that runs
`scripts/release-image-tags.sh "$GITHUB_REF_NAME"`, writes each line to
`$GITHUB_OUTPUT`, and fails on an invalid tag. Compare the resolved `version`
with the `[workspace.package]` version from `Cargo.toml`; emit a `::warning::`
on a mismatch but do not fail. Job outputs are `version`, `major_minor` and
`stable`.

- [ ] **Step 4: Add the image steps to `build`**

Set `needs: meta`, add `permissions: {contents: read, packages: write}`, and
rename the matrix key `deb_arch` to `arch` (values stay `amd64`/`arm64`; the
`.deb` step now reads `DEB_ARCH: ${{ matrix.arch }}`).

After the existing upload step (rename it `Upload the archive`) add, in order:

1. `Stage the image context`: create `$RUNNER_TEMP/image-context`, install the
   three `target/${{ matrix.triple }}/release/` binaries there with `0755`,
   and copy `packaging/microwave.toml.example` to
   `$RUNNER_TEMP/image-context/packaging/`.
2. `Build the architecture image`: `docker build --target release` with
   `--build-arg VERSION=${{ needs.meta.outputs.version }}`,
   `--build-arg REVISION=$GITHUB_SHA`,
   `--build-arg CREATED=$(date -u +%Y-%m-%dT%H:%M:%SZ)`,
   `--build-arg SOURCE=https://github.com/$GITHUB_REPOSITORY`,
   `-t ghcr.io/${{ github.repository_owner }}/microwave-server:${VERSION}-${{ matrix.arch }}`
   and `-f Dockerfile "$RUNNER_TEMP/image-context"`.
3. `Smoke-test the image`:
   `docker run --rm <same tag> microwave-server --version`.
4. `Log in to GHCR` with `docker/login-action@v3`.
5. `Push the architecture image`: `docker push <same tag>`.

Use `env:` for `VERSION` and `ARCH` so the tag expression is written once.

- [ ] **Step 5: Add `manifest` and gate `publish`**

Add `manifest` with `needs: [meta, build]`, `permissions: {contents: read,
packages: write}`, `docker/setup-buildx-action@v3`, the same GHCR login, and
one bash step that builds a tag array:

```bash
base="ghcr.io/${GITHUB_REPOSITORY_OWNER}/microwave-server"
tags=(-t "${base}:${VERSION}")
if [ "${STABLE}" = "1" ]; then
  tags+=(-t "${base}:${MAJOR_MINOR}" -t "${base}:latest")
fi
docker buildx imagetools create "${tags[@]}" \
  "${base}:${VERSION}-amd64" "${base}:${VERSION}-arm64"
```

Set `VERSION`, `MAJOR_MINOR` and `STABLE` from `needs.meta.outputs`. Change
`publish` to `needs: [build, manifest]`; keep everything else in it.

- [ ] **Step 6: Wire the new tests into `claims`**

In `.github/workflows/ci.yml`, after the existing unittest command, add:

```yaml
- run: bash scripts/tests/test-release-image-tags.sh
```

- [ ] **Step 7: Run the tests**

Run: `python3 -m unittest discover -s scripts/tests -v`
Expected: all tests pass.

Run: `bash scripts/tests/test-release-image-tags.sh`
Expected: exit 0.

- [ ] **Step 8: Lint the workflows**

Run the same actionlint invocation as `ci.yml`:
`bash <(curl -fsSL https://raw.githubusercontent.com/rhysd/actionlint/main/scripts/download-actionlint.bash) 1.7.12` then `./actionlint`.
Expected: no findings.

- [ ] **Step 9: Commit**

```bash
git add .github/workflows/release.yml .github/workflows/ci.yml scripts/tests/test_release_image.py
git commit -m "feat(release): publish multi-arch images to GHCR"
```

---

### Task 4: Document the published image

**Files:**
- Modify: `docs/deployment.md` (`## 4. Docker`)

**Interfaces:**
- Consumes: the image name and tag rules from Task 3.
- Produces: operator instructions; no code interface.

- [ ] **Step 1: Add the pull path above the local build**

Before the existing `docker build` block, state that images are published for
`linux/amd64` and `linux/arm64`, show the pull and run with
`ghcr.io/rmoriz/microwave-server:<version>` in place of
`microwave-server:local`, and state that `latest` is stable-only while
pre-releases stay on their exact tag.

Add the one-time visibility note: the GHCR package must be public before
anonymous pulls work, and the OCI source label links it to the repository.

- [ ] **Step 2: Keep the local path**

Keep the existing `docker build -t microwave-server:local .` block and say
that it still compiles locally; the release images are the prebuilt
alternative.

- [ ] **Step 3: Verify**

Run: `python3 scripts/check-claims.py`
Expected: exit 0.

Run: `python3 -m unittest discover -s scripts/tests`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add docs/deployment.md
git commit -m "docs(deployment): document the GHCR image"
```

---

### Task 5: Final verification

- [ ] **Step 1: Run every touched suite**

```bash
bash scripts/tests/test-release-image-tags.sh
python3 -m unittest discover -s scripts/tests -v
python3 scripts/check-claims.py
bash scripts/tests/test-build-deb-version.sh
```

Expected: all exit 0.

- [ ] **Step 2: Re-run actionlint**

Use the `ci.yml` download command and `./actionlint`.
Expected: no findings.

- [ ] **Step 3: Confirm the default local image still builds**

Run `docker build -t microwave-server:local .` and
`docker run --rm microwave-server:local microwave-server --version`, then
remove the image. Expected: version prints.
