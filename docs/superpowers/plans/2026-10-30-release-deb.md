# Release-built .deb Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the release workflow build `microwave-server_<tag>_<arch>.deb` and attach it to the GitHub Release, with the systemd unit writing to a rotated log file.

**Architecture:** The existing matrix job keeps its per-architecture build and adds a packaging step that calls `scripts/build-deb.sh --skip-build` with `DEB_ARCH` from the matrix and `DEB_VERSION` from the validated tag. The unit appends to `/var/log/microwave/microwave.log` and the package ships `/etc/logrotate.d/microwave-server` using `copytruncate`, because the server has no log reopen signal.

**Tech Stack:** Bash, dpkg-deb, systemd unit files, logrotate, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-10-30-release-deb-design.md`

## Global Constraints

- Package version comes from the tag. Strip one leading `v`; reject anything that does not match the Debian upstream-version grammar `^[0-9][0-9A-Za-z.+~-]*$`.
- The `.deb` is only built when `GITHUB_REF_TYPE == tag`. A branch dispatch still produces tarballs.
- `logrotate` is a hard `Depends`.
- The log file is `/var/log/microwave/microwave.log`; the unit uses `StandardOutput=append:` and `LogsDirectory=microwave`.
- The rule uses `copytruncate`, `daily`, `rotate 14`, `compress`, `delaycompress`, `missingok`, `notifempty`, `su root root`.
- No apt repository, no registry, no changes to `Dockerfile` or `docker-e2e`.
- Packaging stays a single script: CI runs `scripts/build-deb.sh`, never a second copy of the staging logic.

## Review Focus

- A log line written between `copytruncate`'s copy and truncate is lost — accepted, but the rule must not silently rotate an empty file (`notifempty`).
- `/var/log/microwave` is owned by `microwave` 0750 and the rule runs as root with `su`: a rule that forgets `su` rotates a file logrotate cannot write, and the next process start fails on the `append:` sink.
- A tag like `v1.2.3-rc.1` must package as `1.2.3-rc.1`, and `vnot-a-version` must fail the job rather than produce a package.
- `ProtectSystem=strict` with `LogsDirectory=` must still leave the log writable, and `systemd-analyze verify` must stay clean.
- A purge must remove `/var/log/microwave`, otherwise the next install inherits a stale log and a rotation state file.

---

### Task 1: Version override with `--print-version`

**Files:**
- Modify: `scripts/build-deb.sh:26-45`
- Create: `scripts/tests/test-build-deb-version.sh`

**Interfaces:**
- Produces: `DEB_VERSION` env var (optional; one leading `v` stripped, validated, overrides `Cargo.toml`) and `--print-version`, which prints the resolved version and exits 0 before any arch, maintainer, or Docker logic runs.

- [ ] **Step 1: Write the failing test**

```bash
#!/usr/bin/env bash
# Version resolution of scripts/build-deb.sh. Runs without Docker or a build.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

fail() { echo "FAIL: $*" >&2; exit 1; }

expected="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"

unset DEB_VERSION
got="$(scripts/build-deb.sh --print-version)"
[ "$got" = "$expected" ] || fail "default should be the workspace version, got '$got'"

got="$(DEB_VERSION=v1.2.3 scripts/build-deb.sh --print-version)"
[ "$got" = "1.2.3" ] || fail "expected 1.2.3, got '$got'"

got="$(DEB_VERSION=2.0.0~rc.1 scripts/build-deb.sh --print-version)"
[ "$got" = "2.0.0~rc.1" ] || fail "expected 2.0.0~rc.1, got '$got'"

DEB_VERSION=not_a_version scripts/build-deb.sh --print-version >/dev/null 2>&1 \
    && fail "an underscore is not a Debian version character"

DEB_VERSION=v scripts/build-deb.sh --print-version >/dev/null 2>&1 \
    && fail "an empty version must fail"

echo "ok"
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash scripts/tests/test-build-deb-version.sh`
Expected: FAIL — `--print-version` is an unknown argument.

- [ ] **Step 3: Implement the override**

In the argument loop add `--print-version) PRINT_VERSION=1 ;;` (initialise `PRINT_VERSION=0` beside `SKIP_BUILD=0`). After the existing `VERSION=` assignment from `Cargo.toml`, and before the arch block, resolve and validate:

```bash
if [ -n "${DEB_VERSION:-}" ]; then
    VERSION="${DEB_VERSION#v}"
fi
case "$VERSION" in
    ''|*[!0-9A-Za-z.+~-]*|[!0-9]*) ;;
    *) VALID=1 ;;
esac
```

Fail with `error: '<value>' is not a Debian version` and exit 1 otherwise. Then, still before the arch block, `if [ "$PRINT_VERSION" -eq 1 ]; then echo "$VERSION"; exit 0; fi`. Extend the header comment block with `DEB_VERSION` and the new flag, and bump the `sed -n '2,24p'` range so `--help` still prints the whole header.

- [ ] **Step 4: Run the test to verify it passes**

Run: `bash scripts/tests/test-build-deb-version.sh`
Expected: `ok`

- [ ] **Step 5: Commit**

```bash
git add scripts/build-deb.sh scripts/tests/test-build-deb-version.sh
git commit -m "feat(deb): accept DEB_VERSION and add --print-version"
```

---

### Task 2: Log file, rotation rule, and dependency

**Files:**
- Modify: `packaging/debian/microwave-server.service:12-36`
- Create: `packaging/debian/microwave-server.logrotate`
- Modify: `scripts/build-deb.sh:128-146` (staging) and `packaging/debian/control.in:7`

**Interfaces:**
- Produces: `/etc/logrotate.d/microwave-server` (0644 root:root) and the unit's `StandardOutput=append:/var/log/microwave/microwave.log`. Task 3 asserts both.

- [ ] **Step 1: Write the failing test**

Add to `scripts/test-deb.sh` inside the quoted container script, after the existing unit check:

```bash
            grep -qx "StandardOutput=append:/var/log/microwave/microwave.log" \
                /usr/lib/systemd/system/microwave-server.service
            grep -qx "LogsDirectory=microwave" \
                /usr/lib/systemd/system/microwave-server.service
            test -f /etc/logrotate.d/microwave-server
            stat -c "%a %U:%G" /etc/logrotate.d/microwave-server | grep -qx "644 root:root"
            grep -q "^    copytruncate$" /etc/logrotate.d/microwave-server
            grep -q "^    notifempty$" /etc/logrotate.d/microwave-server
            grep -q "^    su root root$" /etc/logrotate.d/microwave-server
            logrotate -d /etc/logrotate.d/microwave-server 2>&1 | grep -vq "error:"
            dpkg -s microwave-server | grep -q "^Depends:.*logrotate"
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `scripts/build-deb.sh && scripts/test-deb.sh`
Expected: FAIL on the `StandardOutput=append:` grep.

- [ ] **Step 3: Ship the unit and rule**

In the unit, after `ExecStart=`, add `StandardOutput=append:/var/log/microwave/microwave.log` and `StandardError=inherit`, with a comment naming `copytruncate` as the reason (no reopen signal; `SIGHUP` is TLS only). In the `StateDirectory=` block add `LogsDirectory=microwave` and `LogsDirectoryMode=0750`.

`packaging/debian/microwave-server.logrotate`:

```
# The server has no log-file reopen signal (SIGHUP reloads TLS only), so
# rotation copies the file and truncates it in place.
/var/log/microwave/microwave.log {
    daily
    rotate 14
    compress
    delaycompress
    missingok
    notifempty
    su root root
    copytruncate
}
```

In `scripts/build-deb.sh`, install it with `install -m 0644 packaging/debian/microwave-server.logrotate "$STAGE/etc/logrotate.d/microwave-server"` (add `"$STAGE/etc/logrotate.d"` to the `install -d` line). Add `logrotate` to `Depends:` in `control.in`.

- [ ] **Step 4: Run the test to verify it passes**

Run: `scripts/build-deb.sh && scripts/test-deb.sh`
Expected: `all images passed`

- [ ] **Step 5: Commit**

```bash
git add packaging/debian scripts/build-deb.sh scripts/test-deb.sh
git commit -m "feat(deb): log to a rotated file and ship a logrotate rule"
```

---

### Task 3: Purge the log directory

**Files:**
- Modify: `packaging/debian/postrm:11-14`
- Modify: `scripts/test-deb.sh` (purge assertions)

**Interfaces:**
- Consumes: Task 2's `/var/log/microwave`.

- [ ] **Step 1: Write the failing test**

After the existing `test ! -d /var/lib/microwave` line, add:

```bash
            test ! -d /var/log/microwave
            test ! -f /etc/logrotate.d/microwave-server
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `scripts/build-deb.sh --skip-build && scripts/test-deb.sh`
Expected: FAIL — `/var/log/microwave` survives the purge.

- [ ] **Step 3: Remove the directory on purge**

In `postrm`, next to `rm -rf /var/lib/microwave`, add `rm -rf /var/log/microwave`. Extend the comment: a purge drops the mutable state, including the service log and its rotation state.

- [ ] **Step 4: Run the test to verify it passes**

Run: `scripts/build-deb.sh --skip-build && scripts/test-deb.sh`
Expected: `all images passed`

- [ ] **Step 5: Commit**

```bash
git add packaging/debian/postrm scripts/test-deb.sh
git commit -m "fix(deb): drop the service log on purge"
```

---

### Task 4: Build and attach the package on a tag

**Files:**
- Modify: `.github/workflows/release.yml:15-67`

**Interfaces:**
- Consumes: `DEB_VERSION` and `--skip-build` from Task 1; the package layout from Task 2.
- Produces: `dist/microwave-server_<version>_<arch>.deb` and its `.sha256` in the artifact, uploaded by the existing `dist/*` step and published by the unchanged `publish` job.

- [ ] **Step 1: Add the arch to the matrix**

Give each matrix entry a `deb_arch` (`amd64` for `linux-amd64`, `arm64` for `linux-arm64`).

- [ ] **Step 2: Add the version gate**

Insert before the build step:

```yaml
      - name: Resolve the package version
        id: deb
        shell: bash
        run: |
          set -euo pipefail
          if [ "$GITHUB_REF_TYPE" != "tag" ]; then
            echo "enabled=0" >> "$GITHUB_OUTPUT"
            exit 0
          fi
          version="${GITHUB_REF_NAME#v}"
          case "$version" in
            '' | *[!0-9A-Za-z.+~-]* | [!0-9]*) ;;
            *) echo "enabled=1" >> "$GITHUB_OUTPUT"
               echo "version=$version" >> "$GITHUB_OUTPUT"
               exit 0 ;;
          esac
          echo "error: '$GITHUB_REF_NAME' is not a Debian version" >&2
          exit 1
```

- [ ] **Step 3: Add the packaging step**

After the tarball step, gated with `if: steps.deb.outputs.enabled == '1'`:

```yaml
      - name: Package the .deb
        if: steps.deb.outputs.enabled == '1'
        shell: bash
        env:
          DEB_VERSION: ${{ steps.deb.outputs.version }}
          DEB_ARCH: ${{ matrix.deb_arch }}
          CARGO_TARGET_DIR: target/${{ matrix.triple }}
        run: |
          set -euo pipefail
          scripts/build-deb.sh --skip-build
          sha256sum dist/*.deb > dist/microwave-server_${DEB_VERSION}_${DEB_ARCH}.deb.sha256
```

- [ ] **Step 4: Verify**

Run: `actionlint .github/workflows/release.yml` (install with `brew install actionlint` if missing).
Expected: no output.
Run: `scripts/build-deb.sh --skip-build && scripts/test-deb.sh` on the host once more, so the step's `CARGO_TARGET_DIR` use is proven against a real `target/<triple>/release`.
Expected: `all images passed`.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): build and attach the .deb on a tag"
```

---

### Task 5: Documentation and claims

**Files:**
- Modify: `docs/debian-package.md` (build, install, operate sections)
- Modify: `docs/deployment.md:1878-1890`
- Modify: `docs/claims.toml` (new claims near `deb-pkg-build-script`)

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Document the release build and the log**

In `docs/debian-package.md` add a short "Release builds" section: tag pushes attach `microwave-server_<version>_<arch>.deb` and its `.sha256` to the Release for `amd64` and `arm64`; a `workflow_dispatch` on a branch builds no package. Under "Operate" add the log path `/var/log/microwave/microwave.log`, the systemd ownership of it, and that `/etc/logrotate.d/microwave-server` rotates daily keeping 14 compressed files. Note journald now carries only unit start/stop output, and that `systemctl reload` does not rotate (use `logrotate -f`).

In `docs/deployment.md`, replace the "no file logging" sentence in the Logs section: the packaged service appends to the log file and rotates it via logrotate; containers still use the log driver.

- [ ] **Step 2: Add the claims**

Two `[[claim]]` blocks, `area = "deployment"`, `status = "verified"`: one asserting the release workflow attaches the package (`surfaces` needle `microwave-server_<version>_<arch>.deb` in `docs/debian-package.md`, `verify = { kind = "file", path = ".github/workflows/release.yml" }`), one asserting the rotation (`needle` `/etc/logrotate.d/microwave-server`, `verify = { kind = "file", path = "packaging/debian/microwave-server.logrotate" }`).

- [ ] **Step 3: Verify**

Run: `python3 scripts/check-claims.py`
Expected: exit 0.
Run: `cd website/homepage && npm test`
Expected: pass, the claims content test included.

- [ ] **Step 4: Commit**

```bash
git add docs/debian-package.md docs/deployment.md docs/claims.toml
git commit -m "docs(deb): document release packages and log rotation"
```
