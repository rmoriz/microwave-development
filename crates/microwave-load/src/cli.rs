//! Command line surface of the load test.
//!
//! `seed` writes the fixture straight into the metadata store; `run` drives
//! the HTTP converges. The defaults are the reference profile the report is
//! measured against.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "microwave-load",
    about = "Seed and run the converge load test against a Microwave server",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Write the fixture into the store and the keys into the manifest
    Seed(SeedArgs),
    /// Converge every target node over HTTP
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct SeedArgs {
    /// Base URL of the server the fixture is for
    #[arg(long)]
    pub server_url: String,
    /// Manifest path for the generated keys
    #[arg(long)]
    pub keys: PathBuf,
    /// Data directory holding the SQLite database and the bookshelf
    #[arg(long)]
    pub data_dir: PathBuf,
    #[arg(long, default_value_t = 20_000)]
    pub nodes: usize,
    #[arg(long, default_value_t = 1_000)]
    pub roles: usize,
    #[arg(long, default_value_t = 600)]
    pub versions: usize,
    #[arg(long, default_value_t = 5_000)]
    pub targets: usize,
    #[arg(long, default_value_t = 20)]
    pub canary: usize,
    /// Reuse one keypair for every actor instead of generating one each
    #[arg(long)]
    pub shared_key: bool,
    /// Seed an existing PostgreSQL database instead of the SQLite file
    #[arg(long)]
    pub postgres_url: Option<String>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Base URL of the server to converge
    #[arg(long)]
    pub server_url: String,
    /// Manifest written by `seed`
    #[arg(long)]
    pub keys: PathBuf,
    /// Directory for the report
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
    #[arg(long, default_value_t = 5_000)]
    pub concurrency: usize,
    /// Seconds over which node starts are spread
    #[arg(long, default_value_t = 0)]
    pub ramp: u64,
    #[arg(long, default_value_t = 20)]
    pub canary: usize,
    #[arg(long, default_value = "chef-client")]
    pub chef_client: String,
    /// Upload one blob per sequence through a presigned URL
    #[arg(long)]
    pub upload: bool,
    #[arg(long)]
    pub stats_user: Option<String>,
    #[arg(long)]
    pub stats_pass: Option<String>,
    /// Sign every request as the manifest's superuser instead of the node's client
    #[arg(long)]
    pub superuser: bool,
    /// PostgreSQL URL for the fingerprint
    #[arg(long)]
    pub postgres_url: Option<String>,
    /// Host fingerprint JSON written by the wrapper
    #[arg(long)]
    pub fingerprint: Option<PathBuf>,
    /// Server commit printed into the report
    #[arg(long)]
    pub commit: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn seed_defaults_match_the_reference_profile() {
        let cli = Cli::try_parse_from([
            "microwave-load",
            "seed",
            "--server-url",
            "u",
            "--keys",
            "k",
            "--data-dir",
            "d",
        ])
        .expect("parses");
        let Command::Seed(seed) = cli.command else {
            panic!("seed subcommand expected");
        };
        assert_eq!(seed.nodes, 20_000);
        assert_eq!(seed.roles, 1_000);
        assert_eq!(seed.versions, 600);
        assert_eq!(seed.targets, 5_000);
        assert_eq!(seed.canary, 20);
        assert!(!seed.shared_key);
    }

    #[test]
    fn run_defaults_match_the_reference_profile() {
        let cli =
            Cli::try_parse_from(["microwave-load", "run", "--server-url", "u", "--keys", "k"])
                .expect("parses");
        let Command::Run(run) = cli.command else {
            panic!("run subcommand expected");
        };
        assert_eq!(run.concurrency, 5_000);
        assert_eq!(run.ramp, 0);
        assert_eq!(run.canary, 20);
        assert_eq!(run.chef_client, "chef-client");
        assert_eq!(run.out, PathBuf::from("."));
    }
}
