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
/// directory or the executable's directory, searching up to the repository
/// root (the nearest `.git`, or the outermost `Cargo.toml` for a workspace);
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
        if let Some(script) = locate_script_under(&root) {
            return Ok(script);
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

/// Search `root` and its ancestors for the harness, stopping at the repository
/// boundary.
///
/// The boundary is the nearest ancestor containing `.git` (including a `.git`
/// file used by worktrees). Without one, the outermost ancestor containing a
/// `Cargo.toml` is used, which covers both a single-crate repository and a
/// workspace member. If neither marker exists only `root` is searched, so the
/// lookup never walks the whole filesystem to `/`.
fn locate_script_under(root: &Path) -> Option<PathBuf> {
    let ancestors: Vec<&Path> = root.ancestors().collect();
    let boundary = ancestors
        .iter()
        .position(|ancestor| ancestor.join(".git").exists())
        .or_else(|| {
            ancestors
                .iter()
                .enumerate()
                .filter(|(_, ancestor)| ancestor.join("Cargo.toml").is_file())
                .map(|(index, _)| index)
                .max()
        })
        .unwrap_or(0);

    ancestors
        .into_iter()
        .take(boundary + 1)
        .map(|ancestor| ancestor.join(SCRIPT_RELATIVE))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{locate_script, locate_script_under};
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

    #[test]
    fn stops_looking_at_the_repository_root() {
        let outer = tempfile::tempdir().unwrap();
        let scripts = outer.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("run-pedant.sh"), "#!/usr/bin/env bash\n").unwrap();

        let repo = outer.path().join("repo");
        let nested = repo.join("sub").join("deep");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(&nested).unwrap();

        // The script above the `.git` boundary must not be picked up.
        assert_eq!(locate_script_under(&nested), None);

        let repo_script = repo.join("scripts").join("run-pedant.sh");
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::write(&repo_script, "#!/usr/bin/env bash\n").unwrap();
        assert_eq!(locate_script_under(&nested), Some(repo_script));
    }

    #[test]
    fn uses_a_cargo_manifest_as_the_boundary_for_a_repo_without_git() {
        let outer = tempfile::tempdir().unwrap();
        let scripts = outer.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("run-pedant.sh"), "#!/usr/bin/env bash\n").unwrap();

        let repo = outer.path().join("repo");
        let nested = repo.join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        assert_eq!(locate_script_under(&nested), None);

        let repo_script = repo.join("scripts").join("run-pedant.sh");
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::write(&repo_script, "#!/usr/bin/env bash\n").unwrap();
        assert_eq!(locate_script_under(&nested), Some(repo_script));
    }

    #[test]
    fn without_a_repository_marker_only_the_given_directory_is_searched() {
        let outer = tempfile::tempdir().unwrap();
        let scripts = outer.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("run-pedant.sh"), "#!/usr/bin/env bash\n").unwrap();

        let nested = outer.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(locate_script_under(&nested), None);
    }
}
