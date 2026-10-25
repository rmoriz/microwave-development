//! Run the vendored `oc-chef-pedant` harness.
//!
//! The harness is a repository script, not a Rust reimplementation: it owns
//! Ruby/bundler setup, a fresh server per tag, and artifact collection. This
//! module only locates it and passes tags through as separate arguments, so
//! there is no shell involved and no quoting or injection surface.

use crate::CtlError;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT_RELATIVE: &str = "scripts/run-pedant.sh";
const SCRIPT_ENV: &str = "MW_PEDANT_SCRIPT";

/// Run `scripts/run-pedant.sh` with the supplied tags.
///
/// Output is inherited so server and RSpec output stream live. A non-zero
/// harness status becomes [`CtlError::Exit`] and is preserved by the binary.
pub fn run(tags: &[String]) -> Result<(), CtlError> {
    let script = locate_script()?;
    let status = Command::new("bash")
        .arg(&script)
        .args(tags)
        .status()
        .map_err(|error| {
            CtlError::message(format!(
                "failed to run {} with bash: {error}; install bash or set {SCRIPT_ENV} to a runnable harness",
                script.display()
            ))
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(CtlError::exit(status))
    }
}

/// Resolve the harness path.
///
/// Precedence: `MW_PEDANT_SCRIPT`; then `scripts/run-pedant.sh` in the current
/// directory or one of its ancestors; then the same path relative to the
/// executable (so `target/debug/microwave-ctl` finds the repository checkout);
/// then `/usr/local/share/microwave/run-pedant.sh`.
pub fn locate_script() -> Result<PathBuf, CtlError> {
    if let Some(configured) = std::env::var_os(SCRIPT_ENV) {
        let configured = PathBuf::from(configured);
        if configured.is_file() {
            return Ok(configured);
        }
        return Err(CtlError::message(format!(
            "{SCRIPT_ENV} points at {}, which is not a file",
            configured.display()
        )));
    }

    let current_dir = std::env::current_dir().ok();
    let executable = std::env::current_exe().ok();
    let roots = current_dir.into_iter().chain(
        executable
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf)),
    );

    for root in roots {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join(SCRIPT_RELATIVE);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    let installed = Path::new("/usr/local/share/microwave/run-pedant.sh");
    if installed.is_file() {
        return Ok(installed.to_path_buf());
    }

    Err(CtlError::message(format!(
        "cannot find {SCRIPT_RELATIVE}; run from the repository checkout, install the harness at {}, or set {SCRIPT_ENV}",
        installed.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::locate_script;
    use std::sync::Mutex;

    // These tests mutate process-global environment variables. Keep them
    // serialized so parallel test threads cannot observe each other's value.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn locates_the_repository_harness_from_the_working_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("MW_PEDANT_SCRIPT");
        let script = locate_script().unwrap();
        assert!(script.ends_with("scripts/run-pedant.sh"));
        assert!(script.is_file());
    }

    #[test]
    fn honors_the_override_and_rejects_a_missing_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run-pedant.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\nexit 0\n").unwrap();

        std::env::set_var("MW_PEDANT_SCRIPT", &script);
        assert_eq!(locate_script().unwrap(), script);

        std::env::set_var("MW_PEDANT_SCRIPT", dir.path().join("missing.sh"));
        let error = locate_script().unwrap_err().to_string();
        assert!(error.contains("MW_PEDANT_SCRIPT"));
        assert!(error.contains("not a file"));
        std::env::remove_var("MW_PEDANT_SCRIPT");
    }
}
