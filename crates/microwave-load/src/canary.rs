//! The canary: real `chef-client` runs after the driver.
//!
//! The driver imitates `chef-client` over signed HTTP. A canary is the real
//! binary converging a seeded node, so a fidelity bug in the driver shows up as
//! a canary failure next to a green driver run. It is a diagnostic: its outcome
//! goes into the report and never into the exit status.

use crate::fixture::CANARY_ENVIRONMENT;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::{Id, JoinSet};
use tokio::time::timeout;

/// At most this many canary processes run at once.
pub const MAX_CONCURRENCY: usize = 4;
/// Stderr lines kept per canary; a failed converge can be verbose.
pub const STDERR_TAIL: usize = 20;
/// A single canary child may run this long, then it is killed and reported as
/// `failed (timeout)`.
///
/// Ten minutes is well past a slow real converge; the bound matters because the
/// canary runs after the driver, so a hung child would otherwise keep the run
/// from ever writing its report.
pub const CANARY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a killed child's stderr pipe may stay open, e.g. because a
/// grandchild inherited it.
const STDERR_GRACE: Duration = Duration::from_secs(1);

/// The note a run without a usable binary carries.
pub const SKIPPED: &str = "canary skipped: chef-client not found";

/// One canary node and the client key that signs for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub node: String,
    pub key: PathBuf,
}

/// A finished canary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub node: String,
    ok: bool,
    /// Why it failed: the exit code, or the spawn/wait error.
    reason: String,
    /// The last [`STDERR_TAIL`] stderr lines.
    stderr: Vec<String>,
}

impl Outcome {
    /// The `canary-<n>: ok|failed` report line, with the stderr tail indented
    /// under a failure.
    pub fn line(&self) -> String {
        let status = if self.ok { "ok" } else { "failed" };
        let mut line = format!("{}: {status}", self.node);
        if !self.reason.is_empty() {
            line.push_str(&format!(" ({})", self.reason));
        }
        for stderr in &self.stderr {
            line.push_str("\n  ");
            line.push_str(stderr);
        }
        line
    }
}

/// What the canary contributed to a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Canary {
    pub outcomes: Vec<Outcome>,
    pub note: Option<String>,
}

impl Canary {
    /// The report lines, one per outcome.
    pub fn lines(&self) -> Vec<String> {
        self.outcomes.iter().map(Outcome::line).collect()
    }
}

/// Run every target, four at a time, and never fail the caller.
///
/// A missing binary and an empty target list both end as a note instead of a
/// child.
pub async fn run(binary: &str, targets: Vec<Target>, server_url: &str) -> Canary {
    if targets.is_empty() {
        return Canary {
            outcomes: Vec::new(),
            note: Some("canary skipped: the manifest lists no canary entries".to_string()),
        };
    }
    let binary = match find_binary(binary) {
        Ok(binary) => binary,
        Err(note) => {
            return Canary {
                outcomes: Vec::new(),
                note: Some(note),
            }
        }
    };

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENCY));
    let mut tasks: JoinSet<Outcome> = JoinSet::new();
    let mut nodes: HashMap<Id, String> = HashMap::new();
    for target in targets {
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break;
        };
        let binary = binary.clone();
        let server_url = server_url.to_string();
        let node = target.node.clone();
        let handle = tasks.spawn(async move {
            let _permit = permit;
            converge(&binary, &target, &server_url, CANARY_TIMEOUT).await
        });
        nodes.insert(handle.id(), node);
    }

    let mut outcomes = Vec::new();
    while let Some(finished) = tasks.join_next_with_id().await {
        match finished {
            Ok((id, outcome)) => {
                nodes.remove(&id);
                outcomes.push(outcome);
            }
            // A panicked child task has no outcome of its own; name it after
            // the target the task was spawned for.
            Err(error) => outcomes.push(Outcome {
                node: nodes
                    .remove(&error.id())
                    .unwrap_or_else(|| "canary-?".to_string()),
                ok: false,
                reason: "panicked".to_string(),
                stderr: vec![error.to_string()],
            }),
        }
    }
    order(&mut outcomes);
    Canary {
        outcomes,
        note: None,
    }
}

/// Sort by the numeric index, so `canary-2` precedes `canary-10`.
fn order(outcomes: &mut [Outcome]) {
    outcomes.sort_by(|left, right| {
        index(&left.node)
            .cmp(&index(&right.node))
            .then_with(|| left.node.cmp(&right.node))
    });
}

/// The numeric suffix of a `canary-<n>` name; non-numeric names sort last.
fn index(node: &str) -> (u64, &str) {
    let number = node
        .rsplit_once('-')
        .and_then(|(_, suffix)| suffix.parse().ok())
        .unwrap_or(u64::MAX);
    (number, node)
}

/// Converge one canary like a real `chef-client`: its node, its environment,
/// its client key, the license flag accepted.
///
/// The child is killed after `limit`; its stderr is drained in a separate task
/// so a talkative child cannot block on a full pipe while we wait.
async fn converge(binary: &Path, target: &Target, server_url: &str, limit: Duration) -> Outcome {
    let spawned = Command::new(binary)
        .arg("--node-name")
        .arg(&target.node)
        .arg("--environment")
        .arg(CANARY_ENVIRONMENT)
        .arg("--client-key")
        .arg(&target.key)
        .arg("--server-url")
        .arg(server_url)
        .arg("--chef-license")
        .arg("accept")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();

    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            return Outcome {
                node: target.node.clone(),
                ok: false,
                reason: format!("spawn: {error}"),
                stderr: Vec::new(),
            }
        }
    };
    let reader = child.stderr.take().map(|mut pipe| {
        tokio::spawn(async move {
            let mut stderr = String::new();
            let _ = pipe.read_to_string(&mut stderr).await;
            stderr
        })
    });
    let (status, timed_out) = match timeout(limit, child.wait()).await {
        Ok(Ok(status)) => (Some(status), false),
        Ok(Err(error)) => {
            return Outcome {
                node: target.node.clone(),
                ok: false,
                reason: format!("wait: {error}"),
                stderr: Vec::new(),
            }
        }
        Err(_) => {
            // `kill` also reaps the child, so the reader reaches EOF.
            let _ = child.kill().await;
            (None, true)
        }
    };
    let stderr = match reader {
        // A killed child's grandchild may still hold the pipe; do not block the
        // run on it.
        Some(reader) if timed_out => timeout(STDERR_GRACE, reader)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default(),
        Some(reader) => reader.await.unwrap_or_default(),
        None => String::new(),
    };
    let stderr = tail(&stderr, STDERR_TAIL);

    if timed_out {
        return Outcome {
            node: target.node.clone(),
            ok: false,
            reason: "timeout".to_string(),
            stderr,
        };
    }
    let status = status.expect("a finished wait carries a status");
    if status.success() {
        return Outcome {
            node: target.node.clone(),
            ok: true,
            reason: String::new(),
            stderr,
        };
    }
    Outcome {
        node: target.node.clone(),
        ok: false,
        reason: exit(status),
        stderr,
    }
}

/// The last `count` non-empty stderr lines, trimmed.
fn tail(stderr: &str, count: usize) -> Vec<String> {
    let lines: Vec<String> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// How a canary ended: its exit code, or the signal that killed it.
fn exit(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit {code}"),
        None => "killed by a signal".to_string(),
    }
}

/// The binary as a path, or the note explaining why it cannot run.
///
/// A name with a slash is used as given; a bare name is looked up on `PATH`. An
/// existing file without the executable bit says so instead of passing for
/// missing.
fn find_binary(binary: &str) -> Result<PathBuf, String> {
    let check = |path: &Path| -> Result<(), String> {
        match std::fs::metadata(path) {
            Ok(metadata) if !metadata.is_file() => Err(format!(
                "canary skipped: chef-client is not a regular file: {}",
                path.display()
            )),
            Ok(metadata) if metadata.permissions().mode() & 0o111 == 0 => Err(format!(
                "canary skipped: chef-client is not executable: {}",
                path.display()
            )),
            Ok(_) => Ok(()),
            Err(_) => Err(SKIPPED.to_string()),
        }
    };
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        check(&path)?;
        return Ok(path);
    }
    let path = std::env::var_os("PATH").ok_or_else(|| SKIPPED.to_string())?;
    let mut first = None;
    for candidate in std::env::split_paths(&path).map(|directory| directory.join(binary)) {
        match check(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(note) => first = first.or(Some(note)),
        }
    }
    Err(first.unwrap_or_else(|| SKIPPED.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_stderr_lines_are_kept() {
        let stderr = (0..30)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let tail = tail(&stderr, STDERR_TAIL);

        assert_eq!(tail.len(), STDERR_TAIL);
        assert_eq!(tail[0], "line 10");
        assert_eq!(tail[STDERR_TAIL - 1], "line 29");
    }

    #[test]
    fn blank_stderr_lines_do_not_fill_the_tail() {
        assert_eq!(tail("\n\n  \n", 20), Vec::<String>::new());
        assert_eq!(tail("  warned  \n", 20), vec!["  warned"]);
    }

    #[test]
    fn a_line_carries_ok_or_failed_and_the_tail() {
        let ok = Outcome {
            node: "canary-0".into(),
            ok: true,
            reason: String::new(),
            stderr: vec!["a warning".into()],
        };
        assert_eq!(ok.line(), "canary-0: ok\n  a warning");

        let failed = Outcome {
            node: "canary-1".into(),
            ok: false,
            reason: "exit 1".into(),
            stderr: vec!["converge failed".into()],
        };
        assert_eq!(
            failed.line(),
            "canary-1: failed (exit 1)\n  converge failed"
        );
    }

    #[test]
    fn outcomes_order_by_the_numeric_index() {
        let make = |node: &str| Outcome {
            node: node.into(),
            ok: true,
            reason: String::new(),
            stderr: Vec::new(),
        };
        let mut outcomes = vec![make("canary-10"), make("canary-2"), make("canary-1")];

        order(&mut outcomes);

        let nodes: Vec<&str> = outcomes
            .iter()
            .map(|outcome| outcome.node.as_str())
            .collect();
        assert_eq!(nodes, ["canary-1", "canary-2", "canary-10"]);
    }

    #[test]
    fn a_missing_binary_and_a_file_without_the_executable_bit_differ() {
        assert_eq!(
            find_binary("/no/such/chef-client").expect_err("missing"),
            SKIPPED
        );

        let directory = tempfile::tempdir().expect("tempdir");
        let script = directory.path().join("chef-client");
        std::fs::write(&script, "#!/bin/sh\n").expect("script writes");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o644)).expect("mode");
        let note = find_binary(&script.display().to_string()).expect_err("not executable");
        assert!(note.contains("not executable"), "{note}");
        assert!(note.contains("chef-client"), "{note}");
    }

    /// A script that runs `body` and is executable.
    fn script(directory: &Path, body: &str) -> PathBuf {
        let path = directory.join("chef-client");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("script writes");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        path
    }

    fn target(directory: &Path) -> Target {
        Target {
            node: "canary-0".into(),
            key: directory.join("canary-0.pem"),
        }
    }

    #[tokio::test]
    async fn a_successful_child_still_contributes_its_stderr_tail() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary = script(directory.path(), "printf 'a warning\\n' >&2");

        let outcome = converge(
            &binary,
            &target(directory.path()),
            "http://127.0.0.1:1",
            CANARY_TIMEOUT,
        )
        .await;

        assert!(outcome.ok, "{outcome:?}");
        assert_eq!(outcome.stderr, vec!["a warning"]);
    }

    #[tokio::test]
    async fn a_hung_child_is_killed_and_reported_as_a_timeout() {
        let directory = tempfile::tempdir().expect("tempdir");
        let binary = script(directory.path(), "sleep 5");

        let outcome = converge(
            &binary,
            &target(directory.path()),
            "http://127.0.0.1:1",
            Duration::from_millis(200),
        )
        .await;

        assert!(!outcome.ok, "{outcome:?}");
        assert_eq!(outcome.reason, "timeout");
        assert_eq!(outcome.line(), "canary-0: failed (timeout)");
    }
}
