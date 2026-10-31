//! The report: no number without the machine that produced it.
//!
//! [`write`] puts the fingerprint and the resolved configuration above the
//! first table, because a latency that cannot be paired with the host, the
//! cgroup limits and the image digests is not comparable to anything. The
//! Markdown is for a human, the sibling JSON for diffing two runs later.

use crate::fingerprint::Fingerprint;
use crate::metrics::Summary;
use crate::LoadError;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// One endpoint row of the report, independent of the metrics internals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointResult {
    pub endpoint: String,
    pub count: u64,
    pub statuses: BTreeMap<String, u64>,
    pub retries: u64,
    pub solver_503: u64,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
}

impl EndpointResult {
    fn new(endpoint: &str, summary: &Summary) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            count: summary.count,
            statuses: summary
                .statuses
                .iter()
                .map(|(status, count)| (status.to_string(), *count))
                .collect(),
            retries: summary.retries,
            solver_503: summary.solver_503,
            p50_us: summary.p50,
            p95_us: summary.p95,
            p99_us: summary.p99,
            max_us: summary.max,
        }
    }
}

/// Everything the run measured, besides the fingerprint and the configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Results {
    pub nodes: usize,
    pub completed: u64,
    pub failed: u64,
    /// The collected failure lines, at most [`crate::metrics::MAX_FAILURES`].
    pub failures: Vec<String>,
    pub endpoints: Vec<EndpointResult>,
    /// Raw `db_pool` documents of the `/_stats` poller.
    pub pool_samples: Vec<Value>,
    /// Every driver request signed as the superuser.
    pub superuser: bool,
    /// One `canary-<n>: ok|failed` line per canary; empty without a canary.
    pub canary: Vec<String>,
    /// Why the canary did not run, e.g. a missing `chef-client`.
    pub canary_note: Option<String>,
}

impl Results {
    /// Collect the report results from the merged metrics.
    pub fn new(
        nodes: usize,
        completed: u64,
        failed: u64,
        failures: Vec<String>,
        summaries: BTreeMap<&'static str, Summary>,
        pool_samples: Vec<Value>,
        superuser: bool,
    ) -> Self {
        Self {
            nodes,
            completed,
            failed,
            failures,
            endpoints: summaries
                .iter()
                .map(|(endpoint, summary)| EndpointResult::new(endpoint, summary))
                .collect(),
            pool_samples,
            superuser,
            canary: Vec::new(),
            canary_note: None,
        }
    }
}

/// The whole report, serialized into the sibling JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub generated_at: String,
    pub fingerprint: Fingerprint,
    /// Resolved configuration the run actually used.
    pub config: Value,
    pub results: Results,
}

/// Write `load-report-<UTC>.md` and its sibling JSON into `out` and return the
/// Markdown path.
///
/// The JSON is written first: a run whose Markdown fails leaves the machine
/// readable document behind rather than only the human one.
pub fn write(
    out: &Path,
    fingerprint: &Fingerprint,
    config: &Value,
    results: &Results,
) -> Result<PathBuf, LoadError> {
    fs::create_dir_all(out)?;
    let now = Utc::now();
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let markdown_path = out.join(format!("load-report-{stamp}.md"));
    let json_path = out.join(format!("load-report-{stamp}.json"));
    let report = Report {
        generated_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        fingerprint: fingerprint.clone(),
        config: config.clone(),
        results: results.clone(),
    };

    write_file(&json_path, &serde_json::to_vec_pretty(&report)?)?;
    write_file(&markdown_path, render(&report).as_bytes())?;
    Ok(markdown_path)
}

/// One report file, `0o644` regardless of the umask.
fn write_file(path: &Path, contents: &[u8]) -> Result<(), LoadError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// Render the human report: fingerprint and configuration above every table.
fn render(report: &Report) -> String {
    let mut out = String::new();
    let results = &report.results;
    let fingerprint = &report.fingerprint;

    let _ = writeln!(out, "# Microwave load report");
    let _ = writeln!(out);
    let _ = writeln!(out, "Generated {}.", report.generated_at);
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "The fingerprint comes first: every number below was measured on the \
         machine it names."
    );

    let _ = writeln!(out);
    let _ = writeln!(out, "## Fingerprint");
    let _ = writeln!(out);
    let _ = writeln!(out, "- cpu_model: {}", shown(&fingerprint.cpu_model));
    let _ = writeln!(out, "- cores: {}", shown(&fingerprint.cores));
    let _ = writeln!(
        out,
        "- memory_total_bytes: {}",
        bytes(fingerprint.memory_total_bytes)
    );
    let _ = writeln!(out, "- kernel: {}", shown(&fingerprint.kernel));
    let _ = writeln!(
        out,
        "- out_filesystem: {}",
        shown(&fingerprint.out_filesystem)
    );
    let _ = writeln!(
        out,
        "- out_free_bytes: {}",
        bytes(fingerprint.out_free_bytes)
    );
    let _ = writeln!(
        out,
        "- cgroup_cpu_max: {}",
        shown(&fingerprint.cgroup_cpu_max)
    );
    let _ = writeln!(
        out,
        "- cgroup_memory_max: {}",
        shown(&fingerprint.cgroup_memory_max)
    );
    let _ = writeln!(
        out,
        "- server_commit: {}",
        shown(&fingerprint.server_commit)
    );
    let _ = writeln!(
        out,
        "- pool_size: {} (from the last /_stats db_pool.max sample)",
        shown(&fingerprint.pool_size)
    );
    match (
        &fingerprint.postgres_version,
        fingerprint.postgres_max_connections,
    ) {
        (None, _) => {
            let _ = writeln!(out, "- postgres: unknown");
        }
        (Some(version), max) => {
            let _ = writeln!(
                out,
                "- postgres: {version} (max_connections {}, shared_buffers {})",
                shown(&max),
                shown(&fingerprint.postgres_shared_buffers)
            );
        }
    }
    for (key, value) in &fingerprint.wrapper {
        let _ = writeln!(out, "- {key}: {value}");
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Resolved configuration");
    let _ = writeln!(out);
    let _ = writeln!(out, "```json");
    let _ = writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(&report.config).unwrap_or_default()
    );
    let _ = writeln!(out, "```");

    let _ = writeln!(out);
    let _ = writeln!(out, "## Endpoints");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| Endpoint | n | statuses | p50 µs | p95 µs | p99 µs | max µs | retries | solver 503s |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|---|---|");
    for endpoint in &results.endpoints {
        let statuses: Vec<String> = endpoint
            .statuses
            .iter()
            .map(|(status, count)| format!("{status}: {count}"))
            .collect();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            endpoint.endpoint,
            endpoint.count,
            statuses.join(", "),
            endpoint.p50_us,
            endpoint.p95_us,
            endpoint.p99_us,
            endpoint.max_us,
            endpoint.retries,
            endpoint.solver_503
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Status class `0` is a request that produced no status (transport \
         exhausted or unparseable response)."
    );

    let _ = writeln!(out);
    let _ = writeln!(out, "## Pool occupancy");
    let _ = writeln!(out);
    if results.pool_samples.is_empty() {
        let _ = writeln!(out, "No samples (no pool, or no /_stats credentials).");
    } else {
        let field = |name: &str| -> Vec<u64> {
            results
                .pool_samples
                .iter()
                .filter_map(|sample| sample.get(name).and_then(Value::as_u64))
                .collect()
        };
        let idle = field("idle");
        let in_use = field("in_use");
        let max = field("max");
        let _ = writeln!(out, "{} samples.", results.pool_samples.len());
        let _ = writeln!(out);
        let _ = writeln!(out, "- idle: {}", spread(&idle));
        let _ = writeln!(out, "- in_use: {}", spread(&in_use));
        let _ = writeln!(out, "- max: {}", spread(&max));
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Canary");
    let _ = writeln!(out);
    if let Some(note) = &results.canary_note {
        let _ = writeln!(out, "{note}");
    }
    if results.canary.is_empty() {
        if results.canary_note.is_none() {
            let _ = writeln!(out, "No canary ran.");
        }
    } else {
        for line in &results.canary {
            let _ = writeln!(out, "- {line}");
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Node outcomes");
    let _ = writeln!(out);
    let _ = writeln!(out, "- nodes: {}", results.nodes);
    let _ = writeln!(out, "- completed: {}", results.completed);
    let _ = writeln!(out, "- failed: {}", results.failed);
    if results.superuser {
        let _ = writeln!(
            out,
            "- every driver request signed as the superuser; ACLs were only \
             exercised by the canary"
        );
    }
    if results.failed > 0 {
        let _ = writeln!(out);
        let _ = writeln!(out, "{} nodes did not complete.", results.failed);
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "## Failures");
    let _ = writeln!(out);
    if results.failures.is_empty() {
        let _ = writeln!(out, "None.");
    } else {
        for failure in &results.failures {
            let _ = writeln!(out, "- `{failure}`");
        }
        let unlisted = results.failed as usize - results.failures.len();
        if unlisted > 0 {
            let _ = writeln!(out, "- … {unlisted} more failures not listed");
        }
    }
    out
}

/// An optional value, or `unknown`.
fn shown<T: ToString>(value: &Option<T>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "unknown".to_string(),
    }
}

/// A byte count with a human suffix, or `unknown`.
fn bytes(value: Option<u64>) -> String {
    match value {
        Some(value) if value >= 1 << 30 => {
            format!("{value} ({:.1} GiB)", value as f64 / (1u64 << 30) as f64)
        }
        Some(value) => format!("{value}"),
        None => "unknown".to_string(),
    }
}

/// `min`/median/`max` of one pool gauge.
fn spread(values: &[u64]) -> String {
    if values.is_empty() {
        return "unknown".to_string();
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    format!(
        "min {}, median {}, max {}",
        sorted[0],
        sorted[sorted.len() / 2],
        sorted[sorted.len() - 1]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fingerprint::Fingerprint;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    fn fingerprint() -> Fingerprint {
        Fingerprint {
            cores: Some(8),
            cpu_model: Some("Example CPU @ 3.0GHz".into()),
            memory_total_bytes: Some(34_359_738_368),
            kernel: Some("6.9.0 example x86_64".into()),
            out_filesystem: Some("/dev/example".into()),
            out_free_bytes: Some(1_000_000),
            cgroup_cpu_max: Some("400000 100000".into()),
            cgroup_memory_max: Some("4294967296".into()),
            server_commit: Some("abc1234".into()),
            pool_size: Some(10),
            ..Fingerprint::default()
        }
    }

    fn results() -> Results {
        let mut summaries = BTreeMap::new();
        summaries.insert(
            "GET nodes/:node",
            Summary {
                count: 3,
                statuses: BTreeMap::from([(200, 2), (0, 1)]),
                retries: 2,
                solver_503: 1,
                p50: 1_000,
                p95: 2_000,
                p99: 3_000,
                max: 3_000,
            },
        );
        let mut results = Results::new(
            3,
            2,
            1,
            vec!["GET nodes/:node: transport".into()],
            summaries,
            vec![json!({ "idle": 9, "in_use": 1, "max": 10 })],
            false,
        );
        results.canary = vec!["canary-0: ok".into()];
        results
    }

    fn written() -> (tempfile::TempDir, String, Vec<u8>) {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = write(
            directory.path(),
            &fingerprint(),
            &json!({ "concurrency": 5_000, "ramp_seconds": 0 }),
            &results(),
        )
        .expect("report writes");
        let markdown = fs::read_to_string(&path).expect("markdown reads");
        let json = fs::read(path.with_extension("json")).expect("json reads");
        (directory, markdown, json)
    }

    #[test]
    fn the_markdown_carries_the_fingerprint_above_the_first_table() {
        let (_directory, markdown, _json) = written();

        assert!(markdown.contains("cores: 8"), "{markdown}");
        assert!(markdown.contains("pool_size: 10"), "{markdown}");
        let first_row = markdown
            .lines()
            .position(|line| line.starts_with('|'))
            .expect("an endpoint table");
        assert!(markdown.lines().any(|line| line.starts_with("| Endpoint")));
        let cores = markdown
            .lines()
            .position(|line| line.contains("cores: 8"))
            .expect("cores");
        let pool = markdown
            .lines()
            .position(|line| line.contains("pool_size: 10"))
            .expect("pool size");
        assert!(cores < first_row, "cores at {cores}, table at {first_row}");
        assert!(pool < first_row, "pool at {pool}, table at {first_row}");

        assert!(markdown.contains("1 nodes did not complete"));
        assert!(markdown.contains("GET nodes/:node: transport"));
        assert!(markdown.contains("canary-0: ok"));
    }

    #[test]
    fn the_json_round_trips_with_the_same_fingerprint() {
        let (_directory, _markdown, json) = written();

        let report: Report = serde_json::from_slice(&json).expect("json round-trips");
        assert_eq!(report.fingerprint, fingerprint());
        assert_eq!(report.results.failed, 1);
        assert_eq!(report.results.endpoints[0].statuses["200"], 2);
        assert_eq!(report.results.pool_samples.len(), 1);
        assert_eq!(report.config["concurrency"], 5_000);
    }

    #[test]
    fn both_files_are_world_readable() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path =
            write(directory.path(), &fingerprint(), &json!({}), &results()).expect("report writes");

        for path in [path.clone(), path.with_extension("json")] {
            let mode = fs::metadata(&path).expect("file exists").mode() & 0o777;
            assert_eq!(mode, 0o644, "{}", path.display());
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            let stamp = name
                .strip_prefix("load-report-")
                .and_then(|rest| rest.strip_suffix(".md").or(rest.strip_suffix(".json")))
                .expect("the report names carry the prefix");
            assert!(is_utc_stamp(stamp), "{name}: stamp is not YYYYMMDDTHHMMSSZ");
        }
    }

    /// `YYYYMMDDTHHMMSSZ`.
    fn is_utc_stamp(stamp: &str) -> bool {
        let bytes = stamp.as_bytes();
        bytes.len() == 16
            && bytes[8] == b'T'
            && bytes[15] == b'Z'
            && bytes[..8].iter().all(u8::is_ascii_digit)
            && bytes[9..15].iter().all(u8::is_ascii_digit)
    }
}
