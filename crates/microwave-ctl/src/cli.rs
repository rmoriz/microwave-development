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
    /// Show the status of high-availability services.
    HaStatus,
    /// Open `sqlite3` (or `psql`) against the configured database.
    Psql {
        /// Arguments passed through to `sqlite3` or `psql`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Report the search reindex status.
    Reindex {
        /// Organization to reindex. Omit for all organizations.
        org: Option<String>,
    },

    /// Create a consistent metadata and bookshelf backup.
    Backup { path: PathBuf },
    /// Restore metadata and bookshelf data from a backup.
    Restore { path: PathBuf },

    /// Import a Microwave migration snapshot (produced by the
    /// `microwave-migrate` binary) into the local store.
    MigrateFromLegacy {
        /// Snapshot JSON file to import.
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
    },
}
