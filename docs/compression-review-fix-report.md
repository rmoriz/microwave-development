# Transparent `serialized_object` compression — review/fix report

Branch: `compression`  
Base: `d81cd45`  
Initial implementation: `82ac3e7 feat(store): compress serialized_object payloads transparently`  
Fix round: working tree after `82ac3e7`

## Round 1 — initial implementation

- Shared MWZ1 codec in `microwave-store`: `mwz1:` + base64(zlib(JSON)).
- Transparent writes/reads on SQLite and PostgreSQL for every
  `serialized_object` column.
- Legacy plain JSON remains readable.
- Bounded decompression (64 MiB) with `StoreError::Compression`.
- `storage.compression = "zlib" | "none"` / `MW_STORAGE__COMPRESSION`.
- Snapshot export emits decoded plain JSON; import re-encodes for the target.
- SQLite, PostgreSQL, snapshot, corrupt-value, config, and CLI tests.

## Round 2 — review fixes

### 1. Standalone `microwave-migrate` import and compression — ADDRESSED

- `microwave-migrate import` now accepts:
  - `--compression zlib|none`, and
  - `--config <path>` to derive the default from `[storage] compression`,
    `MW_CONFIG`, `./microwave.toml`, or `MW_STORAGE__COMPRESSION`.
- An explicit `--compression` overrides the config-derived value.
- The chosen mode is passed through `import_with_compression` /
  `import_postgres_with_compression`; the standalone binary no longer
  hard-codes zlib.
- `microwave-ctl migrate-from-legacy` continues to use the configured server
  settings.
- Deployment documentation now describes the standalone flags and no longer
  implies it implicitly reads the server config.

Tests:

- `import_with_compression_none_stores_plain_serialized_objects` (SQLite CLI,
  raw column assertion).
- `import_compression_defaults_from_config_file` (SQLite CLI with `--config`).
- `postgres_import_with_compression_none_stores_plain_serialized_objects`
  (PostgreSQL-gated CLI test).

### 2. Inaccurate upstream-compatibility wording — ADDRESSED

- `StorageCompression` docs now describe a custom MWZ1 envelope, explicitly
  not byte-compatible with upstream storage.
- `TODO.md` and the design spec use the same wording; no `chef_db_compression`
  parity claim remains.
- Docs state that legacy plain JSON remains readable.

### 3. Test gaps — ADDRESSED

- Added SQLite test proving a store opened with `Compression::None` still
  reads pre-existing compressed rows.
- Added `data_bags` to the raw-prefix assertion.
- Added PostgreSQL tests for `Compression::None`:
  - writes plain values and reads pre-existing compressed values;
  - snapshot import writes plain values and reads back decoded JSON.

### 4. 64 MiB decompression ceiling — ADDRESSED

- Documented in `docs/deployment.md`, alongside the import compression flags.

## Verification

Executed on the fix round:

```sh
cargo fmt --all
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-store -p microwave-config -p microwave-migrate
CARGO_PROFILE_DEV_DEBUG=0 cargo test -p microwave-migrate-bin
CARGO_PROFILE_DEV_DEBUG=0 cargo clippy -p microwave-store -p microwave-store-postgres -p microwave-migrate --all-targets -- -D warnings
PATH="/opt/homebrew/opt/libpq/bin:$PATH" CARGO_PROFILE_DEV_DEBUG=0 ./scripts/run-postgres-tests.sh
```

Results:

- SQLite/compression suite: 7 passed.
- Config suite: 12 passed.
- Migrate library/CLI suites: 10 passed (PostgreSQL cases active in the script).
- PostgreSQL store tests: 18 passed.
- PostgreSQL snapshot tests: 2 passed.
- PostgreSQL API runtime/coverage: 8 passed.
- `microwave-ctl` CLI: 37 passed.
- `microwave-migrate` CLI: 10 passed.
- Clippy: clean with `-D warnings`.
