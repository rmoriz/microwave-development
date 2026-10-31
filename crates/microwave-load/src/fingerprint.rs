//! The host fingerprint a report is meaningless without.
//!
//! The driver measures what it can see from inside its container: CPU model
//! and count, `MemTotal`, the kernel, the `--out` filesystem and its free
//! bytes, the cgroup CPU/memory limits, the server commit, the resolved pool
//! size and, through the `--postgres-url` pool, the PostgreSQL version and its
//! `max_connections`/`shared_buffers`.
//!
//! `--fingerprint <path>` names a JSON file the Compose wrapper writes with
//! the facts the driver cannot measure: host facts, image digests and the
//! resolved Compose limits. It is merged *under* the measured values — a
//! measured field wins, a field only the wrapper has survives. Every key the
//! struct does not name lands verbatim in [`Fingerprint::wrapper`], so the
//! wrapper can add facts without a code change. Write a colliding host fact
//! beside the measured field (e.g. `host_cores` next to `cores`) when both
//! must survive: in the Compose stack the measured `cores` is the container's
//! view, not the host's.
//!
//! Known keys: `cpu_model`, `cores`, `memory_total_bytes`, `kernel`,
//! `out_filesystem`, `out_free_bytes`, `cgroup_cpu_max`, `cgroup_memory_max`,
//! `postgres_version`, `postgres_max_connections`, `postgres_shared_buffers`,
//! `server_commit`, `pool_size`.
//!
//! An unreadable or malformed `--fingerprint` file is a diagnostic failure
//! (exit `3`), never a silently thinner report.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// What the run was measured on.
///
/// Every field is optional: a probe that is unavailable leaves its field
/// `None` instead of failing the run, and the report says `unknown`. The
/// struct is stable because the Compose wrapper writes into it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub cpu_model: Option<String>,
    pub cores: Option<u32>,
    pub memory_total_bytes: Option<u64>,
    pub kernel: Option<String>,
    pub out_filesystem: Option<String>,
    pub out_free_bytes: Option<u64>,
    pub cgroup_cpu_max: Option<String>,
    pub cgroup_memory_max: Option<String>,
    pub postgres_version: Option<String>,
    pub postgres_max_connections: Option<u32>,
    pub postgres_shared_buffers: Option<String>,
    pub server_commit: Option<String>,
    pub pool_size: Option<u32>,
    /// Facts only the wrapper has: image digests, Compose limits, host facts.
    #[serde(default, flatten)]
    pub wrapper: BTreeMap<String, Value>,
}

impl Fingerprint {
    /// Read a wrapper file and merge it under the measured values.
    pub fn merge_under(&mut self, wrapper: &Fingerprint) {
        /// Take `wrapper`'s value only where nothing was measured.
        macro_rules! under {
            ($($field:ident),*) => {
                $(
                    if self.$field.is_none() {
                        self.$field = wrapper.$field.clone();
                    }
                )*
            };
        }
        under!(
            cpu_model,
            cores,
            memory_total_bytes,
            kernel,
            out_filesystem,
            out_free_bytes,
            cgroup_cpu_max,
            cgroup_memory_max,
            postgres_version,
            postgres_max_connections,
            postgres_shared_buffers,
            server_commit,
            pool_size
        );
        for (key, value) in &wrapper.wrapper {
            self.wrapper
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
    }

    /// Fill the PostgreSQL facts from `url`; the pool is opened read-only, so
    /// an existing schema is enough.
    pub async fn measure_postgres(&mut self, url: &str) -> Result<(), String> {
        use microwave_store::Compression;
        let store =
            microwave_store_postgres::PostgresStore::connect_existing_with_compression_and_pool(
                url,
                Compression::None,
                1,
            )
            .await
            .map_err(|error| error.to_string())?;
        let pool = store.pool();
        self.postgres_version = Some(
            sqlx::query_scalar::<_, String>("SELECT version()")
                .fetch_one(pool)
                .await
                .map_err(|error| error.to_string())?,
        );
        self.postgres_max_connections = sqlx::query_scalar::<_, String>("SHOW max_connections")
            .fetch_one(pool)
            .await
            .map_err(|error| error.to_string())?
            .parse()
            .ok();
        self.postgres_shared_buffers = Some(
            sqlx::query_scalar::<_, String>("SHOW shared_buffers")
                .fetch_one(pool)
                .await
                .map_err(|error| error.to_string())?,
        );
        Ok(())
    }
}

/// Measure the driver's own environment; `out` is the report directory.
pub fn collect(out: &Path) -> Fingerprint {
    collect_from(Path::new("/proc"), Path::new("/sys/fs/cgroup"), out)
}

/// [`collect`] against explicit pseudo-filesystem roots.
pub fn collect_from(proc_root: &Path, cgroup_root: &Path, out: &Path) -> Fingerprint {
    let (cpu_model, cores) = cpu(proc_root);
    let (out_filesystem, out_free_bytes) = out_space(out);
    Fingerprint {
        cpu_model,
        cores,
        memory_total_bytes: memory(proc_root),
        kernel: kernel(),
        out_filesystem,
        out_free_bytes,
        cgroup_cpu_max: limit(&cgroup_root.join("cpu.max")),
        cgroup_memory_max: limit(&cgroup_root.join("memory.max")),
        ..Fingerprint::default()
    }
}

/// Read the wrapper JSON at `path`.
pub fn read(path: &Path) -> Result<Fingerprint, String> {
    let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The CPU model and the `processor` count of a `cpuinfo` file.
///
/// ARM and some other architectures name the model differently, so `Processor`
/// and `Hardware` are fallbacks; a file without either still yields the count.
fn cpu(proc_root: &Path) -> (Option<String>, Option<u32>) {
    let Some(text) = fs::read_to_string(proc_root.join("cpuinfo")).ok() else {
        return (None, None);
    };
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == name)
                .then(|| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    };
    let model = field("model name")
        .or_else(|| field("Processor"))
        .or_else(|| field("Hardware"));
    let cores = u32::try_from(
        text.lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(key, _)| key.trim() == "processor")
            })
            .count(),
    )
    .ok()
    .filter(|count| *count > 0);
    (model, cores)
}

/// `MemTotal` of a `meminfo` file, in bytes.
fn memory(proc_root: &Path) -> Option<u64> {
    let text = fs::read_to_string(proc_root.join("meminfo")).ok()?;
    text.lines().find_map(|line| {
        let value = line.strip_prefix("MemTotal:")?;
        value
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
            .map(|kib| kib.saturating_mul(1024))
    })
}

/// The kernel release and machine of `uname(2)`.
fn kernel() -> Option<String> {
    let mut uts: libc::utsname = {
        // SAFETY: `utsname` is plain data and all-zero is a valid start state.
        unsafe { std::mem::zeroed() }
    };
    // SAFETY: `uts` is writable and `uname` only fills it.
    if unsafe { libc::uname(&mut uts) } != 0 {
        return None;
    }
    let release = c_string(&uts.release)?;
    let machine = c_string(&uts.machine)?;
    Some(format!("{release} {machine}"))
}

/// A NUL-terminated `uname` field as a string.
fn c_string(buffer: &[libc::c_char]) -> Option<String> {
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    String::from_utf8(bytes).ok()
}

/// The filesystem of `out` and its free bytes for unprivileged users.
fn out_space(out: &Path) -> (Option<String>, Option<u64>) {
    let path = CString::new(out.as_os_str().as_bytes()).ok();
    let mut stats: libc::statvfs = {
        // SAFETY: `statvfs` is plain data and all-zero is a valid start state.
        unsafe { std::mem::zeroed() }
    };
    if let Some(path) = path {
        // SAFETY: `path` is a valid C string and `stats` is writable.
        if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } == 0 {
            let free = (stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64);
            return (device(out), Some(free));
        }
    }
    (None, None)
}

/// The device of `out` as `df` names it.
fn device(out: &Path) -> Option<String> {
    let output = std::process::Command::new("df")
        .arg("-P")
        .arg(out)
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().nth(1)?;
    line.split_whitespace()
        .next()
        .filter(|device| !device.is_empty())
        .map(str::to_string)
}

/// The raw value of a cgroup limit file, `None` when absent.
fn limit(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, contents).expect("write");
    }

    fn sources() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("tempdir");
        let proc_root = directory.path().join("proc");
        let cgroup_root = directory.path().join("cgroup");
        write(
            &proc_root.join("cpuinfo"),
            "processor\t: 0\nmodel name\t: Example CPU @ 3.0GHz\n\
             processor\t: 1\nmodel name\t: Example CPU @ 3.0GHz\n\
             processor\t: 2\nmodel name\t: Example CPU @ 3.0GHz\n\
             processor\t: 3\nmodel name\t: Example CPU @ 3.0GHz\n",
        );
        write(&proc_root.join("meminfo"), "MemTotal:        8388608 kB\n");
        write(&cgroup_root.join("cpu.max"), "400000 100000\n");
        write(&cgroup_root.join("memory.max"), "4294967296\n");
        (directory, proc_root, cgroup_root)
    }

    #[test]
    fn the_measured_facts_come_from_the_pseudo_filesystems() {
        let (directory, proc_root, cgroup_root) = sources();
        let out = directory.path().join("out");
        fs::create_dir_all(&out).expect("out");

        let fingerprint = collect_from(&proc_root, &cgroup_root, &out);

        assert_eq!(fingerprint.cores, Some(4));
        assert_eq!(
            fingerprint.cpu_model.as_deref(),
            Some("Example CPU @ 3.0GHz")
        );
        assert_eq!(fingerprint.memory_total_bytes, Some(8_388_608 * 1024));
        assert_eq!(fingerprint.cgroup_cpu_max.as_deref(), Some("400000 100000"));
        assert_eq!(fingerprint.cgroup_memory_max.as_deref(), Some("4294967296"));
        assert!(fingerprint.out_filesystem.is_some());
        assert!(fingerprint.out_free_bytes.is_some());
    }

    #[test]
    fn a_missing_pseudo_file_is_unknown_not_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let fingerprint = collect_from(
            &directory.path().join("no-proc"),
            &directory.path().join("no-cgroup"),
            directory.path(),
        );

        assert_eq!(fingerprint.cores, None);
        assert_eq!(fingerprint.memory_total_bytes, None);
        assert_eq!(fingerprint.cgroup_cpu_max, None);
    }

    #[test]
    fn wrapper_facts_merge_under_the_measured_ones() {
        let (directory, proc_root, cgroup_root) = sources();
        let out = directory.path().join("out");
        fs::create_dir_all(&out).expect("out");
        let path = directory.path().join("host-fingerprint.json");
        write(
            &path,
            r#"{"cores": 64, "server_commit": "abc1234", "image_digests": ["sha256:1"]}"#,
        );
        let wrapper = read(&path).expect("wrapper reads");
        assert_eq!(wrapper.cores, Some(64));

        let mut fingerprint = collect_from(&proc_root, &cgroup_root, &out);
        fingerprint.merge_under(&wrapper);

        assert_eq!(fingerprint.cores, Some(4), "the measured cores win");
        assert_eq!(
            fingerprint.server_commit.as_deref(),
            Some("abc1234"),
            "a wrapper-only field survives"
        );
        assert_eq!(
            fingerprint.wrapper["image_digests"][0],
            Value::from("sha256:1")
        );
    }

    #[test]
    fn a_malformed_wrapper_file_names_the_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("broken.json");
        write(&path, "{not json");

        let error = read(&path).expect_err("broken json");
        assert!(error.contains(&path.display().to_string()), "{error}");
    }

    #[test]
    fn a_missing_wrapper_file_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("absent.json");

        assert!(read(&path).is_err());
    }
}
