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
    #[arg(long, global = true, value_name = "PATH")]
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
        /// reindexing. Without this flag, documents are only added or replaced.
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

    /// Manage the systemd service. Requires `systemctl` (and `journalctl` for
    /// `tail`); there is no built-in process supervisor.
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
    },

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
}
