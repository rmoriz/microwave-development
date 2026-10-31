//! The canary: real `chef-client` runs after the driver.
//!
//! The driver imitates `chef-client` over signed HTTP. A canary is the real
//! binary converging a seeded node, so a fidelity bug in the driver shows up as
//! a canary failure next to a green driver run. It is a diagnostic: its outcome
//! goes into the report and never into the exit status.

use crate::fixture::CANARY_ENVIRONMENT;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// At most this many canary processes run at once.
pub const MAX_CONCURRENCY: usize = 4;
/// Stderr lines kept per canary; a failed converge can be verbose.
pub const STDERR_TAIL: usize = 20;

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
    let Some(binary) = find_binary(binary) else {
        return Canary {
            outcomes: Vec::new(),
            note: Some(SKIPPED.to_string()),
        };
    };

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for target in targets {
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break;
        };
        let binary = binary.clone();
        let server_url = server_url.to_string();
        tasks.spawn(async move {
            let _permit = permit;
            converge(&binary, &target, &server_url).await
        });
    }

    let mut outcomes = Vec::new();
    while let Some(finished) = tasks.join_next().await {
        if let Ok(outcome) = finished {
            outcomes.push(outcome);
        }
    }
    outcomes.sort_by(|left, right| left.node.cmp(&right.node));
    Canary {
        outcomes,
        note: None,
    }
}

/// Converge one canary like a real `chef-client`: its node, its environment,
/// its client key, the license flag accepted.
async fn converge(binary: &Path, target: &Target, server_url: &str) -> Outcome {
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
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        // The pipe is drained before the wait, so a talkative child cannot
        // block on a full buffer.
        let _ = pipe.read_to_string(&mut stderr).await;
    }
    let status = match child.wait().await {
        Ok(status) => status,
        Err(error) => {
            return Outcome {
                node: target.node.clone(),
                ok: false,
                reason: format!("wait: {error}"),
                stderr: Vec::new(),
            }
        }
    };

    if status.success() {
        return Outcome {
            node: target.node.clone(),
            ok: true,
            reason: String::new(),
            stderr: Vec::new(),
        };
    }
    Outcome {
        node: target.node.clone(),
        ok: false,
        reason: exit(status),
        stderr: tail(&stderr, STDERR_TAIL),
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

/// The binary as a path, or `None` when it is missing or not executable.
///
/// A name with a slash is used as given; a bare name is looked up on `PATH`.
fn find_binary(binary: &str) -> Option<PathBuf> {
    let executable = |path: &Path| {
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    };
    if binary.contains('/') {
        let path = PathBuf::from(binary);
        return executable(&path).then_some(path);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(binary))
        .find(|candidate| executable(candidate))
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
            stderr: Vec::new(),
        };
        assert_eq!(ok.line(), "canary-0: ok");

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
}
