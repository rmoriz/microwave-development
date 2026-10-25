# Operational CLI surface — review/fix report

Branch: `ctl-surface`  
Base: `16378d4 feat(ctl): complete operational CLI surface`  
Fix round: working tree after `16378d4`

## Round 1 — initial implementation

Implemented `reconfigure`, remote `reindex` backfill plus embedded count
verification, the `pedant`/`test` harness runner, systemd
`service start|stop|restart|status|tail`, and tar backup/restore for SQLite and
PostgreSQL. The original report listed the remote stale-document purge,
per-document bulk performance, systemd-only supervision, the external pedant
harness dependency, and the tar payload's exclusions as remaining gaps.

## Round 2 — review fixes

### 1. `_bulk` HTTP 200 with per-item failures was treated as success — ADDRESSED

- `OpenSearchBackend::bulk` now decodes the response and checks `errors` plus
  every `items[].error` and `items[].status`.
- Any item error or non-2xx item status returns `SearchError::Response`, even
  when the HTTP status is 200.
- `errors:true` without item details is also a failure.
- An index request with zero acknowledged items fails instead of silently
  succeeding.
- `SearchBackend::index` now returns `IndexOutcome { acknowledged }`; the
  embedded backend acknowledges one document and OpenSearch returns the parsed
  acknowledged count.
- `reindex` counts only acknowledged documents and reports
  `acknowledged_documents` per run and per type. The CLI label is now
  `documents_acknowledged`; README, deployment docs, and TODO use the same
  wording. At the time of this report the stale-document note said a full purge
  required deleting the remote index; a later change added batched `_bulk`
  writes, `_refresh`, and the `reindex --purge` flag described in the current
  README and deployment guide.

Tests:

- `bulk_item_errors_fail_indexing_even_with_http_200` (mock `errors:true` plus a
  per-item mapper error).
- `bulk_item_statuses_fail_indexing_even_when_errors_is_false` (status 429).
- `bulk_errors_flag_without_item_details_is_still_a_failure`.
- `a_bulk_response_with_no_items_is_not_an_acknowledged_write`.
- `counts_only_documents_the_backend_acknowledges` (reindex report).
- `indexing_sends_a_bulk_index_action` now asserts `acknowledged == 1`.

### 2. Tar backup dereferenced symlinks under `bookshelf/` — ADDRESSED

- `copy_dir_all` now uses `DirEntry::file_type` (which does not follow links)
  and rejects symlinks with an explicit error.
- Other non-regular entries (FIFO, socket, device) are rejected before any open,
  so they cannot block the copy.
- `tar::Builder::follow_symlinks(false)` is set as defense in depth.
- Directory and tar backups share the same staging copy, so both formats are
  protected; restore also refuses symlinks copied from a directory backup.

Tests:

- `tar_backup_rejects_symlinked_bookshelf_content` proves an outside secret is
  not archived, the archive is not created, and the target is untouched.
- `tar_backup_rejects_a_fifo_without_blocking` creates a FIFO and runs the real
  CLI with a bounded watchdog; the backup fails fast instead of hanging.

### 3. `--unit` / `MW_SERVICE_UNIT` option injection — ADDRESSED

- Unit names are validated before any process spawn: empty, leading `-`,
  whitespace, control characters, and `/` are rejected.
- Validation applies to both `--unit` and the environment override.
- Errors do not echo the rejected value.

Tests:

- `rejects_option_like_and_malformed_units` (unit-level table).
- `rejects_an_invalid_environment_unit`.
- `service_rejects_option_like_units_before_running_systemctl` (CLI) also proves
  validation happens even when `systemctl` is absent.

### 4. `service tail` required systemctl unnecessarily — ADDRESSED

- `tail` now resolves the unit and invokes only `journalctl`.
- `start`/`stop`/`restart`/`status` still require `systemctl` and return the
  actionable unavailable error when it is missing.

Tests:

- `service_tail_uses_journalctl_without_requiring_systemctl` sets
  `MW_SYSTEMCTL` to a nonexistent path and still tails successfully.

### 5. Pedant discovery walked every ancestor to `/` — ADDRESSED

- Discovery now searches only up to the repository root: the nearest `.git`
  (including worktree `.git` files), otherwise the outermost `Cargo.toml`.
- If neither marker exists, only the supplied root is searched.
- `MW_PEDANT_SCRIPT` and `/usr/local/share/microwave/run-pedant.sh` remain
  supported.

Tests:

- `stops_looking_at_the_repository_root`.
- `uses_a_cargo_manifest_as_the_boundary_for_a_repo_without_git`.
- `without_a_repository_marker_only_the_given_directory_is_searched`.

### 6. Bad `reconfigure` config must not create the database — ADDRESSED

The existing reconfigure test already proved data preservation. A dedicated
test now uses a fresh data directory and asserts that an invalid bind address
fails before `microwave.db` is created.

Tests:

- `reconfigure_with_bad_config_does_not_create_the_database`.

## Verification

Executed on the fix round:

```sh
cargo fmt --all
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-ctl -p microwave-ctl-bin -p microwave-api --lib
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-ctl-bin --test cli_test
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-ctl -p microwave-ctl-bin -p microwave-api --all-targets -- -D warnings
PATH="/opt/homebrew/opt/libpq/bin:$PATH" CARGO_PROFILE_DEV_DEBUG=0 ./scripts/run-postgres-tests.sh
```

Results:

- `microwave-api` lib: 36 passed.
- `microwave-ctl` lib: 22 passed.
- `microwave-ctl-bin` CLI integration: 54 passed (includes PostgreSQL-gated
  tests when the URL is present).
- Clippy with `-D warnings`: clean.
- PostgreSQL script: exit 0. Store 18, API runtime 2, HTTP coverage 1, API
  additional suites, CLI 54, migrate CLI 10.

One intermediate PostgreSQL run hit the 10-second watchdog on a new tar backup
test while the suite was heavily loaded. The watchdog was raised to 30 seconds
(still bounded for the FIFO regression) and the final run above was green.

## Round 3 — second review fix

### 1. Directory restore destroyed live data before validating the backup bookshelf — ADDRESSED

- `restore_directory` now copies and validates `bookshelf/` from the backup
  into a staging directory inside the data directory before invoking the
  database restore or touching the live bookshelf.
- A symlink, FIFO, other non-regular entry, or copy error fails while the live
  database and bookshelf are still intact.
- After a successful database restore, the live bookshelf is renamed to a
  sidecar, the staged tree is renamed into place, and the sidecar is removed.
  If the final rename fails, the old tree is renamed back.
- A backup without `bookshelf/` still leaves the restored installation without
  one, matching the previous semantics.
- The tar path keeps its existing safe extraction and now goes through the same
  staged-bookshelf swap.

Tests:

- `directory_restore_rejects_a_symlink_before_touching_live_data`: live
  `persisted` and `transient` users and mutated/extra blobs all remain; the
  symlink target is untouched.
- `directory_restore_rejects_a_fifo_before_touching_live_data`: same sentinel
  checks, with a bounded watchdog.
- Existing `backup_and_restore_round_trip`,
  `tar_backup_and_restore_round_trip`, and PostgreSQL round-trip tests cover the
  successful swap.

### 2. Tar restore remains safe — ADDRESSED (unchanged)

Tar extraction still validates entries before unpacking, and the extracted tree
is now staged for bookshelf copying just like a directory backup.

### 3. Symlink TOCTOU — ADDRESSED (file-level) with a documented residual

- On Unix, regular bookshelf files are opened with `O_NOFOLLOW` and
  `O_NONBLOCK`, then `fstat`-checked before copying; a symlink or FIFO swapped
  in after the directory-entry check is rejected instead of followed or blocked
  on.
- Directories still use check-then-traverse rather than `openat`. That residual
  directory-level TOCTOU window is documented in `docs/deployment.md`: run
  backups and restores against a quiescent bookshelf.

### 4. Optional: OpenSearch delete 404 noise — ADDRESSED

- `_bulk` parsing accepts item status 404 only for delete operations.
- Index writes still reject 404 and every other non-2xx item, and index errors
  remain fatal.
- `deleting_a_missing_document_is_not_a_warning` and
  `an_index_item_with_status_404_is_still_a_failure` cover both sides.

## Round 3 verification

```sh
cargo fmt --all
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-ctl -p microwave-ctl-bin
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-ctl -p microwave-ctl-bin --all-targets -- -D warnings
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-api --lib
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-api --all-targets -- -D warnings
```

Results:

- `microwave-ctl` + `microwave-ctl-bin`: all tests passed, including 56 CLI
  integration tests.
- `microwave-api` lib: 38 passed (14 OpenSearch tests).
- Clippy with `-D warnings` for the CLI and API: clean.
