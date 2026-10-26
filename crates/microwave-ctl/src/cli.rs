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
    ///
    /// This is a storage report, not the systemd service status. For the
    /// service, use `service status` or the `service-list` alias.
    Status,

    /// Create a user. A password is generated when one is not supplied.
    ///
    /// Supports both the flag form (`user-create USER --email ... --password
    /// ...`) and the upstream positional form (`user-create USER FIRST
    /// [MIDDLE] LAST EMAIL PASSWORD`). Unless an existing public key is
    /// supplied, a default key is generated and its private key is printed
    /// once (or written to `--filename`).
    UserCreate {
        /// Positional arguments: `USER` or the upstream `USER FIRST [MIDDLE]
        /// LAST EMAIL PASSWORD`.
        #[arg(value_name = "ARGS")]
        positional: Vec<String>,
        /// Email address. Defaults to `<name>@localhost`.
        #[arg(long, value_name = "EMAIL")]
        email: Option<String>,
        /// Password. Generated and printed when omitted.
        #[arg(long, value_name = "PASSWORD")]
        password: Option<String>,
        /// Grant platform administrator privileges.
        #[arg(long)]
        admin: bool,
        /// Write the generated private key to this file instead of printing it.
        #[arg(short = 'f', long = "filename", value_name = "FILE")]
        filename: Option<PathBuf>,
        /// Import an existing PEM public key instead of generating a keypair.
        #[arg(long = "public-key-path", alias = "user-key", value_name = "PATH")]
        public_key_path: Option<PathBuf>,
    },
    /// List usernames.
    UserList,
    /// Show a user.
    UserShow { name: String },
    /// Delete a user.
    UserDelete { name: String },

    /// Create an organization and apply the default authorization policy.
    ///
    /// Creates the `<org>-validator` client with a default key. The validator
    /// private key is printed once unless `--filename` is used.
    OrgCreate {
        /// Organization name.
        name: String,
        /// Human-readable organization name (upstream positional form).
        /// Defaults to the name.
        #[arg(value_name = "FULL_NAME")]
        full_name: Option<String>,
        /// Human-readable organization name (flag form). Defaults to the
        /// positional name or the organization name.
        #[arg(long = "full-name", value_name = "NAME")]
        full_name_flag: Option<String>,
        /// User that receives the initial organization ACLs. Defaults to the
        /// configured superuser.
        #[arg(long, value_name = "USER")]
        creator: Option<String>,
        /// Associate an existing user with the organization and add them to
        /// the `admins` and `billing-admins` groups.
        #[arg(short = 'a', long = "association-user", value_name = "USER")]
        association_user: Option<String>,
        /// Write the validator private key to this file (mode 0600) instead of
        /// printing it.
        #[arg(short = 'f', long = "filename", value_name = "FILE")]
        filename: Option<PathBuf>,
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
    /// Print every stored service credential. This intentionally reveals
    /// secret values and is intended for runbooks that need to copy them to
    /// another node.
    ShowServiceCredentials,

    /// Mark the installation as disabled until credentials have been rotated.
    /// Writes `<data_dir>/credential_rotation_required`; it does not stop
    /// systemd. `rotate-shared-secrets` clears the marker.
    RequireCredentialRotation {
        /// Agree to disable the server and require credential rotation without
        /// an interactive prompt.
        #[arg(short = 'y', long)]
        yes: bool,
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
    ///
    /// Accepts the upstream `psql SERVICE_NAME [--write] [--options VALUE]`
    /// form. Any service name maps to the configured database. Arguments after
    /// the options are passed through to the client tool unchanged.
    Psql {
        /// Optional service name, client options, and trailing arguments. The
        /// CLI recognizes `--write` and `--options VALUE`; everything else is
        /// forwarded to `sqlite3` or `psql`.
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

    /// Alias for `service start`. Accepts the upstream optional SERVICE_NAME.
    Start {
        /// Upstream service name. Treated as the systemd unit when `--unit` is
        /// omitted.
        service: Option<String>,
        /// systemd unit name. Defaults to `MW_SERVICE_UNIT` or
        /// `microwave-server`.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
        /// Run `microwave-server serve` in this process. See `service start`.
        #[arg(long)]
        foreground: bool,
    },
    /// Alias for `service stop`. Accepts the upstream optional SERVICE_NAME.
    Stop {
        /// Upstream service name. Treated as the systemd unit when `--unit` is
        /// omitted.
        service: Option<String>,
        /// systemd unit name.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
    },
    /// Alias for `service restart`. Accepts the upstream optional SERVICE_NAME.
    Restart {
        /// Upstream service name. Treated as the systemd unit when `--unit` is
        /// omitted.
        service: Option<String>,
        /// systemd unit name.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
    },
    /// Send SIGHUP to the service. Microwave runs as one systemd unit, so this
    /// maps to `systemctl restart` (a configuration reload in this
    /// deployment). Accepts the upstream optional SERVICE_NAME.
    Hup {
        /// Upstream service name. Treated as the systemd unit when `--unit` is
        /// omitted.
        service: Option<String>,
        /// systemd unit name.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
    },
    /// Alias for `service tail`. Accepts the upstream optional SERVICE_NAME.
    Tail {
        /// Upstream service name. Treated as the systemd unit when `--unit` is
        /// omitted.
        service: Option<String>,
        /// systemd unit name.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
        /// Number of journal lines.
        #[arg(long, value_name = "COUNT", default_value_t = 50)]
        lines: usize,
        /// Follow the journal.
        #[arg(long)]
        follow: bool,
    },
    /// List the services managed by this installation. Microwave runs as a
    /// single systemd unit; enabled services are marked with `*`.
    ServiceList {
        /// systemd unit name. Defaults to `MW_SERVICE_UNIT` or
        /// `microwave-server`.
        #[arg(long, value_name = "UNIT")]
        unit: Option<String>,
    },

    /// Create a consistent metadata and bookshelf backup. SQLite uses
    /// `VACUUM INTO`; PostgreSQL uses `pg_dump`. The default format is a
    /// directory; `--tar` writes a single tar archive.
    ///
    /// PATH is optional and defaults to a timestamped
    /// `microwave-backup-<UTC>` directory in the current directory.
    Backup {
        /// Destination directory, or tar file when the tar format is selected.
        /// Defaults to `microwave-backup-<UTC>` (or `.tar` for `--tar`).
        path: Option<PathBuf>,
        /// Archive format. Defaults to a directory.
        #[arg(long, value_enum, value_name = "FORMAT")]
        format: Option<BackupFormat>,
        /// Shorthand for `--format tar`.
        #[arg(short = 't', long, conflicts_with = "format")]
        tar: bool,
        /// Accepted for compatibility with upstream backup runbooks. This
        /// backup runs online and does not stop the service.
        #[arg(short = 'y', long)]
        yes: bool,
        /// Back up the resolved configuration only, without metadata or
        /// bookshelf data. Private keys can still be included with
        /// `--include-private-keys`.
        #[arg(short = 'c', long = "config-only")]
        config_only: bool,
        /// Additional options passed to `pg_dump` (and `pg_restore` during a
        /// restore from a backup made with this flag). Conflicting options
        /// managed by this CLI are rejected.
        #[arg(long = "pg-options", value_name = "VALUE", allow_hyphen_values = true)]
        pg_options: Option<String>,
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
        /// Additional options passed to `pg_restore` for PostgreSQL backups.
        #[arg(long = "pg-options", value_name = "VALUE", allow_hyphen_values = true)]
        pg_options: Option<String>,
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

    /// Remove local bookshelf blobs that are not referenced by the metadata
    /// store. Use `--estimate-only` to report the count and size without
    /// deleting anything.
    CleanupOrphanBlobs {
        /// Report orphaned blobs without deleting them.
        #[arg(long)]
        estimate_only: bool,
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
    fn backup_without_path_and_include_private_keys_combines() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "backup",
            "-t",
            "-y",
            "--config-only",
            "--include-private-keys",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Backup {
                path: None,
                tar: true,
                yes: true,
                config_only: true,
                include_private_keys: true,
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

    #[test]
    fn backup_path_is_optional_and_accepts_upstream_flags() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "backup",
            "-y",
            "--config-only",
            "--pg-options",
            "--no-sync",
            "-t",
        ])
        .unwrap();
        match cli.command {
            Command::Backup {
                path,
                tar,
                yes,
                config_only,
                pg_options,
                ..
            } => {
                assert!(path.is_none());
                assert!(tar);
                assert!(yes);
                assert!(config_only);
                assert_eq!(pg_options.as_deref(), Some("--no-sync"));
            }
            _ => panic!("expected backup command"),
        }
    }

    #[test]
    fn parses_the_top_level_service_aliases() {
        let cli = Cli::try_parse_from(["microwave-ctl", "restart", "erchef"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Restart {
                service: Some(ref service),
                unit: None
            } if service == "erchef"
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "start", "--foreground", "--unit", "demo"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::Start {
                foreground: true,
                ref unit,
                ..
            } if unit.as_deref() == Some("demo")
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "hup", "--unit", "demo"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hup { ref unit, .. } if unit.as_deref() == Some("demo")
        ));

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "tail",
            "--unit",
            "demo",
            "--lines",
            "7",
            "--follow",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Tail {
                lines: 7,
                follow: true,
                ..
            }
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "service-list"]).unwrap();
        assert!(matches!(cli.command, Command::ServiceList { unit: None }));
    }

    #[test]
    fn parses_user_create_in_both_forms() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "user-create",
            "alice",
            "Alice",
            "Marie",
            "Smith",
            "alice@example.com",
            "secret",
        ])
        .unwrap();
        match cli.command {
            Command::UserCreate {
                ref positional,
                email,
                password,
                admin,
                filename,
                public_key_path,
            } => {
                assert_eq!(positional.len(), 6);
                assert!(email.is_none());
                assert!(password.is_none());
                assert!(!admin);
                assert!(filename.is_none());
                assert!(public_key_path.is_none());
            }
            _ => panic!("expected user-create"),
        }

        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "user-create",
            "alice",
            "--email",
            "alice@example.com",
            "--password",
            "secret",
            "-f",
            "/tmp/alice.pem",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::UserCreate {
                filename: Some(ref path),
                ref email,
                ..
            } if path == std::path::Path::new("/tmp/alice.pem")
                && email.as_deref() == Some("alice@example.com")
        ));
    }

    #[test]
    fn parses_org_create_association_user_and_filename() {
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "org-create",
            "acme",
            "Acme Corp",
            "-a",
            "bob",
            "-f",
            "/tmp/acme-validator.pem",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::OrgCreate {
                association_user: Some(ref user),
                filename: Some(ref path),
                full_name: Some(ref full_name),
                full_name_flag: None,
                ..
            } if user == "bob"
                && path == std::path::Path::new("/tmp/acme-validator.pem")
                && full_name == "Acme Corp"
        ));

        // The flag form remains available for existing scripts.
        let cli = Cli::try_parse_from([
            "microwave-ctl",
            "org-create",
            "acme",
            "--full-name",
            "Acme Corp",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::OrgCreate {
                full_name: None,
                full_name_flag: Some(ref full_name),
                ..
            } if full_name == "Acme Corp"
        ));

        // The upstream positional form carries the full name.
        let cli =
            Cli::try_parse_from(["microwave-ctl", "org-create", "acme", "Acme Corp"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::OrgCreate {
                full_name: Some(ref full_name),
                full_name_flag: None,
                ..
            } if full_name == "Acme Corp"
        ));
    }

    #[test]
    fn parses_credential_and_blob_cleanup_commands() {
        assert!(matches!(
            Cli::try_parse_from(["microwave-ctl", "show-service-credentials"])
                .unwrap()
                .command,
            Command::ShowServiceCredentials
        ));

        let cli =
            Cli::try_parse_from(["microwave-ctl", "require-credential-rotation", "-y"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::RequireCredentialRotation { yes: true }
        ));

        let cli = Cli::try_parse_from(["microwave-ctl", "cleanup-orphan-blobs", "--estimate-only"])
            .unwrap();
        assert!(matches!(
            cli.command,
            Command::CleanupOrphanBlobs {
                estimate_only: true
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
