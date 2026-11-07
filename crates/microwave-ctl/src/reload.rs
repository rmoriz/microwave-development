//! Reload the object store tenant map without restarting the server.
//!
//! The tenant map and the per-organization credentials are validated locally
//! first; any issue aborts before a signal is sent. Otherwise a random token is
//! written to the request file, `systemctl reload` triggers the server's SIGHUP
//! handler, and this command waits for the token to come back in the status
//! file. The token tells our own answer apart from a concurrent reload's.
//!
//! Exit codes: 1 for a rejected reload or an invalid tenant map, 2 on timeout,
//! 3 when another reload answered first, and 4 when the service is not running.

use crate::{context::Context, service, CtlError};
use microwave_config::{tenant_issues, OrgCredentials};
use microwave_store::OBJECT_STORE_SECRET_GROUP;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) const STATUS_FILE: &str = "objectstore.reload";
const REQUEST_FILE: &str = "objectstore.reload.request";
const TIMEOUT_ENV: &str = "MW_RELOAD_TIMEOUT_MS";
const POLL_ENV: &str = "MW_RELOAD_POLL_MS";
const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_POLL_MS: u64 = 50;

/// How a poll for our own token ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Ok,
    Rejected(String),
    Timeout,
    Superseded,
}

/// Validate, signal, and wait for a server-side reload.
pub(crate) fn run(unit: Option<&str>, config: Option<&Path>) -> Result<(), CtlError> {
    let context = Context::load(config)?;
    let store = context.open_existing_metadata_store()?;

    let organizations: BTreeSet<String> = store
        .run(store.store().list_orgs())?
        .into_iter()
        .map(|organization| organization.name)
        .collect();

    let mut credentials = BTreeMap::new();
    for organization in &organizations {
        let access_key = store
            .run(store.store().get_secret(
                OBJECT_STORE_SECRET_GROUP,
                &format!("{organization}.access_key"),
            ))?
            .unwrap_or_default();
        let secret_key = store
            .run(store.store().get_secret(
                OBJECT_STORE_SECRET_GROUP,
                &format!("{organization}.secret_key"),
            ))?
            .unwrap_or_default();
        credentials.insert(
            organization.clone(),
            OrgCredentials {
                access_key: access_key.trim().to_string(),
                secret_key: secret_key.trim().to_string(),
            },
        );
    }

    let issues = tenant_issues(&context.config.object_store, &organizations, &credentials);
    if !issues.is_empty() {
        for issue in &issues {
            eprintln!("{issue}");
        }
        return Err(CtlError::coded(
            1,
            format!("the object store tenant map has {} issue(s)", issues.len()),
        ));
    }

    // The boot id keeps a status from an older process out. Without a boot
    // status the service never started (or does not run this reload support).
    let status_path = context.data_dir.join(STATUS_FILE);
    let Some(starting) = read_json(&status_path) else {
        return Err(service::not_running());
    };
    let Some(boot_id) = starting
        .get("boot_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    else {
        return Err(service::not_running());
    };

    let token = uuid::Uuid::new_v4().simple().to_string();
    context.prepare_data_dir()?;
    let request_path: PathBuf = context.data_dir.join(REQUEST_FILE);
    std::fs::write(&request_path, &token)?;

    if let Err(error) = service::reload(unit) {
        // Only withdraw a request the server has not consumed yet.
        remove_while_ours(&request_path, &token);
        return Err(error);
    }

    let timeout = duration_from_env(TIMEOUT_ENV, DEFAULT_TIMEOUT_MS);
    let outcome = await_status(
        &context.data_dir,
        &boot_id,
        &token,
        &starting,
        timeout,
        duration_from_env(POLL_ENV, DEFAULT_POLL_MS),
    );
    remove_while_ours(&request_path, &token);

    match outcome {
        Outcome::Ok => {
            println!("reloaded the object store tenant map");
            Ok(())
        }
        Outcome::Rejected(message) => Err(CtlError::coded(1, message)),
        Outcome::Timeout => Err(CtlError::coded(
            2,
            format!(
                "timed out after {} ms waiting for the reload status",
                timeout.as_millis()
            ),
        )),
        Outcome::Superseded => Err(CtlError::coded(
            3,
            "another reload answered first; the snapshot may still have been applied — check `microwave-ctl diagnostics`",
        )),
    }
}

/// Poll the status file until the deadline.
///
/// A status from another boot is ignored. A different token means another
/// reload answered (or is still running); the answer counts as superseded once
/// the status moved away from the snapshot read before signalling.
fn await_status(
    data_dir: &Path,
    boot_id: &str,
    token: &str,
    starting: &serde_json::Value,
    timeout: Duration,
    interval: Duration,
) -> Outcome {
    let status_path = data_dir.join(STATUS_FILE);
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(status) = read_json(&status_path) {
            if status.get("boot_id").and_then(serde_json::Value::as_str) == Some(boot_id) {
                if status.get("token").and_then(serde_json::Value::as_str) == Some(token) {
                    return if status.get("outcome").and_then(serde_json::Value::as_str)
                        == Some("ok")
                    {
                        Outcome::Ok
                    } else {
                        Outcome::Rejected(
                            status
                                .get("message")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                                .unwrap_or_else(|| "reload rejected".to_string()),
                        )
                    };
                }
                if !same_snapshot(&status, starting) {
                    return Outcome::Superseded;
                }
            }
        }

        if Instant::now() >= deadline {
            return Outcome::Timeout;
        }
        std::thread::sleep(interval);
    }
}

fn same_snapshot(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    left.get("at") == right.get("at") && left.get("token") == right.get("token")
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Remove the request file only while it still holds `token`.
fn remove_while_ours(path: &Path, token: &str) {
    let still = std::fs::read_to_string(path).ok();
    if still.as_deref().map(str::trim) != Some(token) {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!("warning: failed to remove {}: {error}", path.display()),
    }
}

/// Read a millisecond duration from the environment, falling back on an
/// unparsable value.
fn duration_from_env(name: &str, default_ms: u64) -> Duration {
    let millis = std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default_ms);
    Duration::from_millis(millis)
}

#[cfg(test)]
mod tests {
    use super::{await_status, read_json, Outcome};
    use std::path::Path;
    use std::time::Duration;

    const BOOT: &str = "boot-1";
    const TOKEN: &str = "ours";
    const AT: &str = "2026-11-07T00:00:00Z";

    fn status(dir: &Path, boot_id: &str, token: Option<&str>, at: &str, outcome: &str) {
        let token = token.map_or_else(|| "null".to_string(), |token| format!("\"{token}\""));
        let message = if outcome == "ok" {
            "null".to_string()
        } else {
            "\"bucket mw-acme is unreachable\"".to_string()
        };
        std::fs::write(
            dir.join("objectstore.reload"),
            format!(
                "{{\"boot_id\":\"{boot_id}\",\"token\":{token},\"trigger\":\"sighup\",\"at\":\"{at}\",\"outcome\":\"{outcome}\",\"message\":{message}}}"
            ),
        )
        .unwrap();
    }

    fn starting(dir: &Path) -> serde_json::Value {
        status(dir, BOOT, None, AT, "ok");
        read_json(&dir.join("objectstore.reload")).unwrap()
    }

    #[test]
    fn ignores_a_status_from_another_boot() {
        let dir = tempfile::tempdir().unwrap();
        let starting = starting(dir.path());
        status(dir.path(), "boot-0", Some(TOKEN), AT, "ok");

        let path = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            status(&path, BOOT, Some(TOKEN), "2026-11-07T00:00:01Z", "ok");
        });

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_secs(2),
            Duration::from_millis(5),
        );
        writer.join().unwrap();
        assert_eq!(outcome, Outcome::Ok);
    }

    #[test]
    fn returns_ok_on_a_matching_token() {
        let dir = tempfile::tempdir().unwrap();
        let starting = starting(dir.path());
        status(dir.path(), BOOT, Some(TOKEN), "2026-11-07T00:00:01Z", "ok");

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_secs(1),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, Outcome::Ok);
    }

    #[test]
    fn carries_the_rejection_message() {
        let dir = tempfile::tempdir().unwrap();
        let starting = starting(dir.path());
        status(
            dir.path(),
            BOOT,
            Some(TOKEN),
            "2026-11-07T00:00:01Z",
            "rejected",
        );

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_secs(1),
            Duration::from_millis(5),
        );
        assert_eq!(
            outcome,
            Outcome::Rejected("bucket mw-acme is unreachable".to_string())
        );
    }

    #[test]
    fn times_out_without_an_answer() {
        let dir = tempfile::tempdir().unwrap();
        let starting = starting(dir.path());

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, Outcome::Timeout);
    }

    #[test]
    fn a_newer_status_from_another_token_supersedes() {
        let dir = tempfile::tempdir().unwrap();
        let starting = starting(dir.path());
        status(
            dir.path(),
            BOOT,
            Some("someone-else"),
            "2026-11-07T00:00:02Z",
            "ok",
        );

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_secs(1),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, Outcome::Superseded);
    }

    #[test]
    fn the_starting_snapshot_does_not_supersede() {
        let dir = tempfile::tempdir().unwrap();
        // The status read before signalling carries an earlier reload's token
        // and is identical to `starting`, so it is not an answer to us.
        status(dir.path(), BOOT, Some("someone-else"), AT, "ok");
        let starting = read_json(&dir.path().join("objectstore.reload")).unwrap();

        let outcome = await_status(
            dir.path(),
            BOOT,
            TOKEN,
            &starting,
            Duration::from_millis(30),
            Duration::from_millis(5),
        );
        assert_eq!(outcome, Outcome::Timeout);
    }
}
