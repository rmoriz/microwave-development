//! Thin systemd service management with a foreground escape hatch.
//!
//! Microwave has no built-in process supervisor and must not pretend to have
//! one. When `systemctl` is available this delegates to it, inheriting stdout
//! and stderr so `status` and `tail` behave like the native tools. Everywhere
//! else it returns an actionable error instead of reporting success.
//!
//! `service start --foreground` is the supported container/foreground mode: it
//! resolves the `microwave-server` binary and runs `serve` directly. On Unix
//! the CLI is replaced with `exec`, so the server keeps the caller's PID and
//! receives signals (including Docker's `SIGTERM`) exactly as if it had been
//! started directly.

use crate::{CtlError, ServiceAction};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEFAULT_UNIT: &str = "microwave-server";
const UNIT_ENV: &str = "MW_SERVICE_UNIT";
const SYSTEMCTL_ENV: &str = "MW_SYSTEMCTL";
const JOURNALCTL_ENV: &str = "MW_JOURNALCTL";
const SERVER_BIN_ENV: &str = "MW_SERVER_BIN";
const FOREGROUND_ENV: &str = "MW_SERVICE_FOREGROUND";
const DEFAULT_SERVER_BIN: &str = "microwave-server";

/// Execute a service action against a systemd unit, or run the server in the
/// foreground when `start --foreground` (or `MW_SERVICE_FOREGROUND=1`) is
/// requested.
pub fn run(
    action: ServiceAction,
    unit: Option<&str>,
    lines: usize,
    follow: bool,
    config: Option<&Path>,
    foreground: bool,
) -> Result<(), CtlError> {
    if foreground && action != ServiceAction::Start {
        return Err(CtlError::message(
            "--foreground is only valid for `service start`; use `service stop`, `restart`, `status`, or `tail` for the systemd unit",
        ));
    }

    // The environment alternative is scoped to `start` so a container image can
    // set it globally without breaking `service status` and friends.
    let foreground = foreground || (action == ServiceAction::Start && foreground_from_env()?);
    if foreground {
        if unit.is_some() {
            return Err(CtlError::message(
                "--unit is not valid with `service start --foreground`; configure the server with `--config` or MW_* environment variables",
            ));
        }
        return start_foreground(config);
    }

    let unit = resolve_unit(unit)?;
    match action {
        // Reading the journal does not require a running systemctl binary.
        ServiceAction::Tail => tail(&unit, lines, follow),
        _ => {
            let systemctl = std::env::var(SYSTEMCTL_ENV).unwrap_or_else(|_| "systemctl".into());
            if !program_available(&systemctl) {
                return Err(unavailable(&systemctl));
            }
            let verb = match action {
                ServiceAction::Start => "start",
                ServiceAction::Stop => "stop",
                ServiceAction::Restart => "restart",
                ServiceAction::Status => "status",
                ServiceAction::Tail => unreachable!("tail was handled above"),
            };
            systemctl_action(&systemctl, verb, &unit)
        }
    }
}

/// Run `microwave-server serve` in this process.
///
/// `MW_SERVER_BIN` wins; otherwise a `microwave-server` next to the current
/// executable is preferred, then `PATH`. The configured path is passed as a
/// separate argument (never through a shell) and validated before execution.
fn start_foreground(config: Option<&Path>) -> Result<(), CtlError> {
    let program = resolve_server_binary()?;
    let config = validate_config_path(config)?;

    let mut command = Command::new(&program);
    command.arg("serve");
    if let Some(config) = config.as_deref() {
        command.arg("--config").arg(config);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Replace the CLI process with the server so PID 1/signal handling and
        // process supervisors see the server directly.
        let error = command.exec();
        Err(CtlError::message(format!(
            "failed to exec {} serve: {error}; the server was not started",
            program.display()
        )))
    }

    #[cfg(not(unix))]
    {
        let status = command.status().map_err(|error| {
            CtlError::message(format!(
                "failed to start {} serve: {error}; check MW_SERVER_BIN and the --config path",
                program.display()
            ))
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(CtlError::exit(status))
        }
    }
}

fn resolve_server_binary() -> Result<PathBuf, CtlError> {
    if let Some(value) = std::env::var_os(SERVER_BIN_ENV) {
        if value.is_empty() {
            return Err(CtlError::message(format!(
                "{SERVER_BIN_ENV} is empty; set it to the microwave-server path or unset it"
            )));
        }
        return validate_server_binary(Path::new(&value));
    }

    if let Ok(current) = std::env::current_exe() {
        if let Some(directory) = current.parent() {
            let candidate = directory.join(DEFAULT_SERVER_BIN);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
    }

    if let Some(found) = find_program(DEFAULT_SERVER_BIN) {
        return Ok(found);
    }

    Err(CtlError::message(format!(
        "could not find `{DEFAULT_SERVER_BIN}` to run in the foreground; install it beside microwave-ctl, put it on PATH, or set {SERVER_BIN_ENV} to its absolute path"
    )))
}

/// Validate an explicit `MW_SERVER_BIN` value.
///
/// A bare program name is resolved through `PATH`; anything with a path
/// separator must be a real executable file. A leading `-` is harmless for a
/// program name (it is not parsed as an option), but empty values are rejected
/// rather than silently falling back to another binary.
fn validate_server_binary(path: &Path) -> Result<PathBuf, CtlError> {
    if path.as_os_str().is_empty() {
        return Err(CtlError::message(format!(
            "{SERVER_BIN_ENV} is empty; set it to the microwave-server path or unset it"
        )));
    }
    if path.components().count() == 1 && !path.is_absolute() {
        return find_program(&path.to_string_lossy()).ok_or_else(|| {
            CtlError::message(format!(
                "`{}` from {SERVER_BIN_ENV} was not found on PATH; set {SERVER_BIN_ENV} to an absolute path",
                path.display()
            ))
        });
    }
    if !is_executable(path) {
        return Err(CtlError::message(format!(
            "`{}` from {SERVER_BIN_ENV} is not an executable file; set {SERVER_BIN_ENV} to the microwave-server binary",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

fn validate_config_path(config: Option<&Path>) -> Result<Option<PathBuf>, CtlError> {
    let Some(config) = config else {
        return Ok(None);
    };
    if config.as_os_str().is_empty() {
        return Err(CtlError::message(
            "--config must not be empty when starting the server in the foreground",
        ));
    }
    let metadata = std::fs::metadata(config).map_err(|error| {
        CtlError::message(format!(
            "cannot read --config {}: {error}; pass the path to the server TOML configuration file",
            config.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(CtlError::message(format!(
            "--config {} is not a regular file; pass the path to the server TOML configuration file",
            config.display()
        )));
    }
    Ok(Some(config.to_path_buf()))
}

fn foreground_from_env() -> Result<bool, CtlError> {
    match std::env::var(FOREGROUND_ENV) {
        Ok(value) => parse_foreground_value(&value),
        Err(std::env::VarError::NotPresent) => Ok(false),
        Err(std::env::VarError::NotUnicode(_)) => Err(CtlError::message(format!(
            "{FOREGROUND_ENV} is not valid UTF-8; use 1/true or 0/false"
        ))),
    }
}

fn parse_foreground_value(value: &str) -> Result<bool, CtlError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        other => Err(CtlError::message(format!(
            "{FOREGROUND_ENV}={other:?} is not a boolean; use 1/true or 0/false"
        ))),
    }
}

/// Resolve and validate the unit name.
///
/// A unit is passed to `systemctl`/`journalctl` as a separate argument, but a
/// value beginning with `-` would still be parsed as an option. Unit names also
/// cannot contain whitespace, control characters, or `/`; rejecting them here
/// keeps an environment override from turning into argument injection.
fn resolve_unit(unit: Option<&str>) -> Result<String, CtlError> {
    if let Some(unit) = unit {
        return validate_unit(unit);
    }
    match std::env::var(UNIT_ENV) {
        Ok(unit) => validate_unit(&unit),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_UNIT.to_string()),
        Err(std::env::VarError::NotUnicode(_)) => Err(CtlError::message(format!(
            "{UNIT_ENV} is not valid UTF-8; set a plain systemd unit name"
        ))),
    }
}

fn validate_unit(unit: &str) -> Result<String, CtlError> {
    if unit.is_empty() {
        return Err(CtlError::message(
            "the service unit must not be empty; pass --unit or set MW_SERVICE_UNIT",
        ));
    }
    if unit.starts_with('-') {
        return Err(CtlError::message(
            "the service unit must not start with '-'; this would be parsed as an option",
        ));
    }
    if unit
        .chars()
        .any(|character| character.is_whitespace() || character.is_control() || character == '/')
    {
        return Err(CtlError::message(
            "the service unit must not contain whitespace, control characters, or '/'; use a systemd unit name such as microwave-server",
        ));
    }
    Ok(unit.to_string())
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
        "`{program}` was not found, so there is no systemd service manager to control. For a container or any environment without systemd, run `microwave-ctl service start --foreground` (or `MW_SERVICE_FOREGROUND=1 microwave-ctl service start`) to run `microwave-server serve` in this process. You can also start the server directly with `microwave-server serve`. This CLI does not manage a process it did not start. Set {SYSTEMCTL_ENV} to an explicit systemctl path if systemd is installed elsewhere."
    ))
}

fn program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 || path.is_absolute() {
        return is_executable(path);
    }
    find_program(program).is_some()
}

/// Find a bare program name on `PATH`, returning the resolved path.
fn find_program(program: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
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
    use super::{
        find_program, parse_foreground_value, program_available, resolve_unit,
        validate_config_path, validate_server_binary,
    };
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolves_the_unit_from_the_flag_then_environment_then_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("MW_SERVICE_UNIT");
        assert_eq!(
            resolve_unit(Some("custom.service")).unwrap(),
            "custom.service"
        );
        assert_eq!(resolve_unit(None).unwrap(), "microwave-server");

        std::env::set_var("MW_SERVICE_UNIT", "env.service");
        assert_eq!(resolve_unit(None).unwrap(), "env.service");
        std::env::remove_var("MW_SERVICE_UNIT");
    }

    #[test]
    fn rejects_option_like_and_malformed_units() {
        let _guard = ENV_LOCK.lock().unwrap();
        for unit in [
            "",
            "-oops.service",
            "--now",
            "bad unit.service",
            "bad\tunit.service",
            "bad\nunit.service",
            "bad\u{7}unit.service",
            "path/unit.service",
        ] {
            let error = resolve_unit(Some(unit))
                .expect_err(&format!("unit {unit:?} should be rejected"))
                .to_string();
            assert!(
                error.contains("service unit"),
                "unexpected error for {unit:?}: {error}"
            );
        }

        let error = resolve_unit(Some("-oops.service")).unwrap_err().to_string();
        assert!(error.contains("must not start with '-'"));
    }

    #[test]
    fn rejects_an_invalid_environment_unit() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("MW_SERVICE_UNIT", "-injected.service");
        let error = resolve_unit(None).unwrap_err().to_string();
        assert!(error.contains("must not start with '-'"));
        std::env::remove_var("MW_SERVICE_UNIT");
    }

    #[test]
    fn recognises_paths_and_rejects_missing_programs() {
        assert!(program_available("sh") || program_available("bash"));
        assert!(!program_available("/definitely/not/here/systemctl"));
        assert!(find_program("sh").is_some() || find_program("bash").is_some());
        assert!(find_program("microwave-definitely-not-a-real-program").is_none());
    }

    #[test]
    fn parses_boolean_foreground_environment_values() {
        for value in ["1", "true", "TRUE", "yes", "on", " true "] {
            assert!(
                parse_foreground_value(value).unwrap(),
                "{value:?} should enable foreground mode"
            );
        }
        for value in ["", "0", "false", "FALSE", "no", "off", " 0 "] {
            assert!(
                !parse_foreground_value(value).unwrap(),
                "{value:?} should disable foreground mode"
            );
        }
        let error = parse_foreground_value("sometimes").unwrap_err().to_string();
        assert!(error.contains("MW_SERVICE_FOREGROUND"));
        assert!(error.contains("not a boolean"));
    }

    #[test]
    fn validates_the_foreground_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("microwave.toml");
        std::fs::write(&config, "[server]\nbind = \"127.0.0.1:8000\"\n").unwrap();

        assert_eq!(validate_config_path(None).unwrap(), None);
        assert_eq!(
            validate_config_path(Some(&config)).unwrap(),
            Some(config.clone())
        );

        let empty = validate_config_path(Some(std::path::Path::new(""))).unwrap_err();
        assert!(empty.to_string().contains("must not be empty"));

        let directory = validate_config_path(Some(dir.path())).unwrap_err();
        assert!(directory.to_string().contains("not a regular file"));

        let missing = validate_config_path(Some(&dir.path().join("missing.toml"))).unwrap_err();
        assert!(missing.to_string().contains("cannot read --config"));
    }

    #[test]
    fn validates_an_explicit_server_binary() {
        let dir = tempfile::tempdir().unwrap();
        let missing = validate_server_binary(&dir.path().join("missing-server")).unwrap_err();
        assert!(missing.to_string().contains("MW_SERVER_BIN"));
        assert!(missing.to_string().contains("not an executable file"));

        let directory = validate_server_binary(dir.path()).unwrap_err();
        assert!(directory.to_string().contains("not an executable file"));

        assert!(validate_server_binary(std::path::Path::new("")).is_err());
    }
}
