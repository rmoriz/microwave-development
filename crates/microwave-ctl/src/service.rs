//! Thin systemd service management.
//!
//! Microwave has no built-in process supervisor and must not pretend to have
//! one. When `systemctl` is available this delegates to it, inheriting stdout
//! and stderr so `status` and `tail` behave like the native tools. Everywhere
//! else it returns an actionable error instead of reporting success.

use crate::{CtlError, ServiceAction};
use std::path::Path;
use std::process::Command;

const DEFAULT_UNIT: &str = "microwave-server";
const UNIT_ENV: &str = "MW_SERVICE_UNIT";
const SYSTEMCTL_ENV: &str = "MW_SYSTEMCTL";
const JOURNALCTL_ENV: &str = "MW_JOURNALCTL";

/// Execute a service action against a systemd unit.
pub fn run(
    action: ServiceAction,
    unit: Option<&str>,
    lines: usize,
    follow: bool,
) -> Result<(), CtlError> {
    let unit = resolve_unit(unit);
    let systemctl = std::env::var(SYSTEMCTL_ENV).unwrap_or_else(|_| "systemctl".into());
    if !program_available(&systemctl) {
        return Err(unavailable(&systemctl));
    }

    match action {
        ServiceAction::Tail => tail(&unit, lines, follow),
        ServiceAction::Start => systemctl_action(&systemctl, "start", &unit),
        ServiceAction::Stop => systemctl_action(&systemctl, "stop", &unit),
        ServiceAction::Restart => systemctl_action(&systemctl, "restart", &unit),
        ServiceAction::Status => systemctl_action(&systemctl, "status", &unit),
    }
}

fn resolve_unit(unit: Option<&str>) -> String {
    unit.map(str::trim)
        .filter(|unit| !unit.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(UNIT_ENV)
                .ok()
                .map(|unit| unit.trim().to_string())
                .filter(|unit| !unit.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_UNIT.to_string())
}

fn systemctl_action(program: &str, verb: &str, unit: &str) -> Result<(), CtlError> {
    let mut command = Command::new(program);
    command.args([verb, unit]);
    run_inherited(command, "systemctl", unit)
}

fn tail(unit: &str, lines: usize, follow: bool) -> Result<(), CtlError> {
    if lines == 0 {
        return Err(CtlError::message(
            "--lines must be greater than zero for `service tail`",
        ));
    }
    let journalctl = std::env::var(JOURNALCTL_ENV).unwrap_or_else(|_| "journalctl".into());
    if !program_available(&journalctl) {
        return Err(CtlError::message(format!(
            "`{journalctl}` was not found, so journal output cannot be read; install systemd's journalctl, read the service log through your init system, or run the server in the foreground and capture stderr. Set {JOURNALCTL_ENV} to an explicit journalctl path if it is not on PATH."
        )));
    }

    let mut command = Command::new(&journalctl);
    command.args(["-u", unit, "-n", &lines.to_string()]);
    if follow {
        command.arg("-f");
    }
    run_inherited(command, "journalctl", unit)
}

fn run_inherited(mut command: Command, tool: &str, unit: &str) -> Result<(), CtlError> {
    let status = command.status().map_err(|error| {
        CtlError::message(format!(
            "failed to run {tool} for unit {unit}: {error}; manage the service with your init system or set the corresponding MW_* command override"
        ))
    })?;
    if status.success() {
        Ok(())
    } else {
        // Preserve systemctl's status (3 means "inactive" for `status`) so
        // scripts can distinguish a stopped unit from a successful probe.
        Err(CtlError::exit(status))
    }
}

fn unavailable(program: &str) -> CtlError {
    CtlError::message(format!(
        "`{program}` was not found, so there is no systemd service manager to control. Start the server with `microwave-server serve`, use your container runtime (for example `docker compose`), or run your platform's service manager. This CLI does not manage a process it did not start. Set {SYSTEMCTL_ENV} to an explicit systemctl path if systemd is installed elsewhere."
    ))
}

fn program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return is_executable(path);
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|directory| is_executable(&directory.join(program)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{program_available, resolve_unit};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_the_unit_from_the_flag_then_environment_then_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("MW_SERVICE_UNIT");
        assert_eq!(resolve_unit(Some("custom.service")), "custom.service");
        assert_eq!(resolve_unit(Some("  ")), "microwave-server");

        std::env::set_var("MW_SERVICE_UNIT", "env.service");
        assert_eq!(resolve_unit(None), "env.service");
        std::env::remove_var("MW_SERVICE_UNIT");
    }

    #[test]
    fn recognises_paths_and_rejects_missing_programs() {
        assert!(program_available("sh") || program_available("bash"));
        assert!(!program_available("/definitely/not/here/systemctl"));
    }
}
