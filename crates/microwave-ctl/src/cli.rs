use clap::{Parser, Subcommand};
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

    /// Generate and set a new password, printing it once.
    Password { user: String },

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
