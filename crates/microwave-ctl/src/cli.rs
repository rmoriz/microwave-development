use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "microwave-ctl",
    version,
    about = "Microwave Server operational CLI"
)]
pub struct Cli {
    /// Path to a TOML configuration file.
    #[arg(long, global = true, value_name = "PATH", allow_hyphen_values = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MaintenanceAction {
    /// Enable maintenance mode.
    On,
    /// Disable maintenance mode.
    Off,
    /// Report the current maintenance mode.
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackupFormat {
    /// A directory containing the database snapshot/dump and a bookshelf copy.
    Directory,
    /// A single uncompressed tar archive with the same contents.
    Tar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ServiceAction {
    /// Start the configured systemd unit.
    Start,
    /// Stop the configured systemd unit.
    Stop,
    /// Restart the configured systemd unit.
    Restart,
    /// Show the configured systemd unit's status.
    Status,
    /// Print recent journal entries for the configured systemd unit.
    Tail,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the CLI version.
    Version,
    /// Report storage paths, database readability, and superuser state.
    Status,

    /// Create a user. A password is generated when one is not supplied.
    UserCreate {
        /// Username.
        name: String,
        /// Email address. Defaults to `<name>@localhost`.
        #[arg(long, value_name = "EMAIL")]
        email: Option<String>,
        /// Password. Generated and printed when omitted.
        #[arg(long, value_name = "PASSWORD")]
        password: Option<String>,
        /// Grant platform administrator privileges.
        #[arg(long)]
        admin: bool,
    },
    /// List usernames.
    UserList,
    /// Show a user.
    UserShow { name: String },
    /// Delete a user.
    UserDelete { name: String },

    /// Create an organization and apply the default authorization policy.
    OrgCreate {
        /// Organization name.
        name: String,
        /// Human-readable organization name. Defaults to the name.
        #[arg(long = "full-name", value_name = "NAME")]
        full_name: Option<String>,
        /// User that receives the initial organization ACLs. Defaults to the
        /// configured superuser.
        #[arg(long, value_name = "USER")]
        creator: Option<String>,
    },
    /// List organizations.
    OrgList,
    /// Show an organization and its members.
    OrgShow { name: String },
    /// Delete an organization.
    OrgDelete { name: String },

    /// Associate a user with an organization.
    OrgUserAdd { org: String, user: String },
    /// Remove a user from an organization.
    OrgUserRemove { org: String, user: String },

    /// Grant a user platform administrator (server-admin) privileges.
    GrantServerAdminPermissions { user: String },
    /// Remove platform administrator (server-admin) privileges from a user.
    RemoveServerAdminPermissions { user: String },
    /// List users with platform administrator (server-admin) privileges.
    ListServerAdmins,

    /// Generate and set a new password, printing it once.
    Password { user: String },

    /// Create a new client key. The private key is printed once.
    AddClientKey {
        /// Organization name.
        org: String,
        /// Client name.
        client: String,
        /// Key name. Defaults to the fingerprint of the key.
        #[arg(long = "key-name", value_name = "NAME")]
        key_name: Option<String>,
        /// Expiration date (`infinity` or an ISO-8601 timestamp).
        #[arg(
            long = "expiration-date",
            value_name = "DATE",
            default_value = "infinity"
        )]
        expiration_date: String,
        /// Import an existing PEM public key instead of generating a keypair.
        #[arg(long = "public-key-path", value_name = "PATH")]
        public_key_path: Option<PathBuf>,
    },
    /// Create a new user key. The private key is printed once.
    AddUserKey {
        /// Username.
        user: String,
        /// Key name. Defaults to the fingerprint of the key.
        #[arg(long = "key-name", value_name = "NAME")]
        key_name: Option<String>,
        /// Expiration date (`infinity` or an ISO-8601 timestamp).
        #[arg(
            long = "expiration-date",
            value_name = "DATE",
            default_value = "infinity"
        )]
        expiration_date: String,
        /// Import an existing PEM public key instead of generating a keypair.
        #[arg(long = "public-key-path", value_name = "PATH")]
        public_key_path: Option<PathBuf>,
    },
    /// List keys for a client.
    ListClientKeys {
        /// Organization name.
        org: String,
        /// Client name.
        client: String,
        /// Include expiration dates and public keys.
        #[arg(short, long)]
        verbose: bool,
    },
    /// List keys for a user.
    ListUserKeys {
        /// Username.
        user: String,
        /// Include expiration dates and public keys.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Delete a client key.
    DeleteClientKey {
        /// Organization name.
        org: String,
        /// Client name.
        client: String,
        /// Key name.
        key: String,
    },
    /// Delete a user key.
    DeleteUserKey {
        /// Username.
        user: String,
        /// Key name.
        key: String,
    },

    /// Print a secret. This is the only command that reveals secret values.
    ShowSecret {
        /// Credential group, for example `object_store` or `bookshelf`.
        group: String,
        /// Credential name, for example `secret`.
        name: String,
    },
    /// Set or replace a secret.
    SetSecret {
        /// Credential group, for example `object_store` or `bookshelf`.
        group: String,
        /// Credential name, for example `secret`.
        name: String,
        /// Secret value. Prefer a shell without history expansion; the value
        /// is not echoed by the command.
        value: String,
    },
    /// Remove a secret.
    RemoveSecret {
        /// Credential group, for example `object_store` or `bookshelf`.
        group: String,
        /// Credential name, for example `secret`.
        name: String,
    },

    /// Control maintenance mode.
    Maintenance {
        /// `on`, `off`, or `status`.
        action: MaintenanceAction,
    },

    /// Load configuration and run preflight checks.
    CheckConfig,
    /// Validate the configuration, initialize or migrate the metadata backend,
    /// and report the resolved backends. This does not restart a running
    /// server or change existing data.
    Reconfigure,
    /// Show the status of high-availability services.
    HaStatus,
    /// Open `sqlite3` (or `psql`) against the configured database.
    Psql {
        /// Arguments passed through to `sqlite3` or `psql`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Rebuild remote search documents from the metadata store, or verify the
    /// embedded backend's authoritative relational counts.
    Reindex {
        /// Organization to reindex. Omit for all organizations.
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
        /// Delete each organization's existing remote documents before
        /// reindexing. Destructive: an interrupted run can leave the
        /// organization unsearchable until the reindex is run again. Without
        /// this flag, documents are only added or replaced.
        #[arg(long)]
        purge: bool,
    },

    /// Run the vendored `oc-chef-pedant` suite through
    /// `scripts/run-pedant.sh`. The script is located relative to the current
    /// directory, the executable, or `MW_PEDANT_SCRIPT`.
    #[command(alias = "test")]
    Pedant {
        /// Tags passed through to the harness. Omit for its default core set.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        tags: Vec<String>,
    },

    /// Manage the service through systemd, or run the server in the foreground
    /// for containers. Requires `systemctl` (and `journalctl` for `tail`) unless
    /// `start --foreground` is used.
    Service {
        /// Service action.
        action: ServiceAction,
        /// systemd unit name. Defaults to `MW_SERVICE_UNIT` or
        /// `microwave-server`.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
        /// Number of journal lines for `tail`.
        #[arg(long, value_name = "COUNT", default_value_t = 50)]
        lines: usize,
        /// Follow the journal for `tail`.
        #[arg(long)]
        follow: bool,
        /// Run `microwave-server serve` in this process instead of calling
        /// systemd. Only valid for `start`. On Unix the CLI is replaced by the
        /// server via `exec`, so signals and the PID behave as if the server
        /// were started directly. `MW_SERVICE_FOREGROUND=1` is equivalent for
        /// `start`.
        #[arg(long)]
        foreground: bool,
    },

    /// Create a consistent metadata and bookshelf backup. SQLite uses
    /// `VACUUM INTO`; PostgreSQL uses `pg_dump`. The default format is a
    /// directory; `--format tar` writes a single tar archive.
    Backup {
        /// Destination directory, or tar file when the tar format is selected.
        path: PathBuf,
        /// Archive format. Defaults to a directory.
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<BackupFormat>,
        /// Shorthand for `--format tar`.
        #[arg(long, conflicts_with = "format")]
        tar: bool,
        /// Also include the fixed server private-key/certificate set under
        /// `private/`. The backup then contains unencrypted key material.
        #[arg(long)]
        include_private_keys: bool,
    },
    /// Restore metadata and bookshelf data from a backup directory or tar
    /// archive. SQLite restores the staged database file; PostgreSQL replays
    /// the `pg_dump` archive. This is destructive and asks for confirmation
    /// unless `--yes` is given.
    Restore {
        /// Backup directory, or tar file when the tar format is selected.
        path: PathBuf,
        /// Skip the interactive confirmation. Required when stdin is not a
        /// terminal.
        #[arg(long)]
        yes: bool,
        /// Backup format. Directories and files are auto-detected when omitted.
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<BackupFormat>,
        /// Shorthand for `--format tar`.
        #[arg(long, conflicts_with = "format")]
        tar: bool,
        /// Also restore private key/certificate material from the backup's
        /// `private/` subtree. Restoring keys can invalidate existing clients.
        #[arg(long)]
        restore_private_keys: bool,
        /// Restore only the `private/` subtree; do not replace the database or
        /// bookshelf. Requires `--restore-private-keys`.
        #[arg(long, requires = "restore_private_keys")]
        private_only: bool,
    },

    /// Edit a user's profile, email, admin flag, or password. Without
    /// `--input`, the JSON document is opened in `$VISUAL`/`$EDITOR`.
    UserEdit {
        /// Username.
        user: String,
        /// Read the edited JSON document from this file instead of an editor.
        #[arg(long, value_name = "FILE")]
        input: Option<PathBuf>,
    },

    /// Show the client id, secret, and redirect URI of an OAuth application.
    OcIdShowApp {
        /// Application name.
        app: String,
    },

    /// Remove authorization entities no longer referenced by any actor or
    /// object. Use `--estimate-only` to report the count without deleting.
    CleanupBifrost {
        /// Report how many unused authorization entities would be removed.
        #[arg(long)]
        estimate_only: bool,
        /// Seconds to wait for in-flight requests before starting.
        #[arg(long, value_name = "SECONDS", default_value_t = 0)]
        wait_time: u64,
        /// Also remove stale internal cleanup tracking rows.
        #[arg(long)]
        force_cleanup: bool,
    },

    /// Write redacted, indexable objects for all organizations as JSON files.
    FilteredDump {
        /// Destination directory for the JSON files.
        #[arg(long, value_name = "DIR", default_value = ".")]
        output: PathBuf,
        /// Limit the dump to one organization.
        #[arg(long, value_name = "ORG")]
        org: Option<String>,
    },

    /// Create a tarball of recent logs and redacted system information.
    GatherLogs {
        /// Destination `.tar.gz` file. Defaults to
        /// `microwave-logs-<timestamp>.tar.gz` in the current directory.
        #[arg(long, value_name = "FILE")]
        output: Option<PathBuf>,
    },

    /// Rotate the stored credentials for one service group.
    RotateCredentials {
        /// Credential/service group, for example `bookshelf` or `oc_id`.
        service: String,
    },

    /// Rotate the stored credentials for every known service group.
    RotateAllCredentials,

    /// Rotate the shared secret/salt and every service credential.
    RotateSharedSecrets,

    /// Import a Microwave migration snapshot (produced by the
    /// `microwave-migrate` binary) into the configured SQLite or PostgreSQL
    /// store. This merges into live metadata and asks for confirmation unless
    /// `--yes` is given.
    MigrateFromLegacy {
        /// Snapshot JSON file to import.
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
        /// Skip the interactive confirmation. Required when stdin is not a
        /// terminal.
        #[arg(long)]
        yes: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::{BackupFormat, Cli, Command, ServiceAction};
    use clap::Parser;

    #[test]
    fn parses_the_new_operational_commands() {
        let cli = Cli::try_parse_from(["microwave-ctl", "reconfigure"]).unwrap();
        assert!(matches!(cli.command, Command::Reconfigure));

        let cli = Cli::try_parse_from(["microwave-ctl", "reindex", "--org", "acme"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Reindex {
                org: Some(ref org),
                purge: false
            } if org == "acme"
        ));

        let cli =
            Cli::try_parse_from(["microwave-ctl", "reindex", "--org", "acme", "--purge"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Reindex {
                org: Some(ref org),
                purge: true
            } if org == "acme"
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "pedant", "users", "nodes"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Pedant { ref tags } if tags == &["users", "nodes"]
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "service",
            "tail",
            "--unit",
            "custom.service",
            "--lines",
            "5",
            "--follow",
        ])
        .unwrap();
        match cli.command {
            Command::Service {
                action,
                unit,
                lines,
                follow,
                ..
            } => {
                assert_eq!(action, ServiceAction::Tail);
                assert_eq!(unit.as_deref(), Some("custom.service"));
                assert_eq!(lines, 5);
                assert!(follow);
            }
            _ => panic!("expected service command"),
        }
    }

    #[test]
    fn parses_service_start_foreground_with_a_config() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "service",
            "start",
            "--foreground",
            "--config",
            "/etc/microwave/microwave.toml",
        ])
        .unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/etc/microwave/microwave.toml"))
        );
        match cli.command {
            Command::Service {
                action, foreground, ..
            } => {
                assert_eq!(action, ServiceAction::Start);
                assert!(foreground);
            }
            _ => panic!("expected service command"),
        }

        // `--config` is a global option and also works before the subcommand.
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "--config",
            "/etc/microwave/microwave.toml",
            "service",
            "start",
            "--foreground",
        ])
        .unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/etc/microwave/microwave.toml"))
        );
        assert!(matches!(
            cli.command,
            Command::Service {
                foreground: true,
                ..
            }
        ));

        // A hyphen-leading path is accepted as a value, not treated as an
        // option, so it can be forwarded safely to the server.
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "service",
            "start",
            "--foreground",
            "--config",
            "-microwave.toml",
        ])
        .unwrap();
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("-microwave.toml"))
        );
    }

    #[test]
    fn parses_both_tar_spellings() {
        let cli =
            Cli::try_parse_from(["microwave-ctl", "backup", "out.tar", "--format", "tar"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Backup {
                format: Some(BackupFormat::Tar),
                ..
            }
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "backup", "out.tar", "--tar"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Backup {
                format: None,
                tar: true,
                ..
            }
        ));

        let cli =
            Cli::try_parse_from(["microwave-ctl", "restore", "out.tar", "--tar", "--yes"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Restore {
                format: None,
                tar: true,
                yes: true,
                ..
            }
        ));
    }

    #[test]
    fn parses_private_key_backup_and_restore_flags() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "backup",
            "out.tar",
            "--tar",
            "--include-private-keys",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Backup {
                tar: true,
                include_private_keys: true,
                ..
            }
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "restore",
            "out.tar",
            "--tar",
            "--yes",
            "--restore-private-keys",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Restore {
                tar: true,
                yes: true,
                restore_private_keys: true,
                private_only: false,
                ..
            }
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "restore",
            "out",
            "--yes",
            "--restore-private-keys",
            "--private-only",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Restore {
                yes: true,
                restore_private_keys: true,
                private_only: true,
                ..
            }
        ));
        assert!(Cli::try_parse_from([
            "microwave-ctl",
            "restore",
            "out",
            "--yes",
            "--private-only",
        ])
        .is_err());
    }

    #[test]
    fn rejects_conflicting_tar_flags() {
        assert!(Cli::try_parse_from([
            "microwave-ctl",
            "backup",
            "out.tar",
            "--format",
            "tar",
            "--tar",
        ])
        .is_err());
    }

    #[test]
    fn backup_defaults_to_the_directory_format() {
        let cli = Cli::try_parse_from(["microwave-ctl", "backup", "out"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Backup {
                format: None,
                tar: false,
                ..
            }
        ));
    }

    #[test]
    fn parses_the_remaining_compatibility_commands() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "user-edit",
            "alice",
            "--input",
            "/tmp/alice.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::UserEdit { ref user, input: Some(ref path) }
                if user == "alice" && path == std::path::Path::new("/tmp/alice.json")
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "oc-id-show-app", "supermarket"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::OcIdShowApp { ref app } if app == "supermarket"
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "cleanup-bifrost",
            "--estimate-only",
            "--wait-time",
            "5",
            "--force-cleanup",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::CleanupBifrost {
                estimate_only: true,
                wait_time: 5,
                force_cleanup: true
            }
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "filtered-dump",
            "--output",
            "/tmp/dump",
            "--org",
            "acme",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::FilteredDump { ref output, org: Some(ref org) }
                if output == std::path::Path::new("/tmp/dump") && org == "acme"
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "gather-logs",
            "--output",
            "/tmp/logs.tar.gz",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::GatherLogs { output: Some(ref path) }
                if path == std::path::Path::new("/tmp/logs.tar.gz")
        ));

        let cli =
            Cli::try_parse_from(["microwave-ctl", "rotate-credentials", "bookshelf"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::RotateCredentials { ref service } if service == "bookshelf"
        ));
        assert!(matches!(
            Cli::try_parse_from(["microwave-ctl", "rotate-all-credentials"])
                .unwrap()
                .command,
            Command::RotateAllCredentials
        ));
        assert!(matches!(
            Cli::try_parse_from(["microwave-ctl", "rotate-shared-secrets"])
                .unwrap()
                .command,
            Command::RotateSharedSecrets
        ));
    }
}
