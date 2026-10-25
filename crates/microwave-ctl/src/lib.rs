//! Operational CLI for Microwave Server.
//!
//! The crate is shared by the `microwave-ctl` binary and the
//! `chef-server-ctl` compatibility alias, so both expose exactly the same
//! command surface.

mod backup;
mod cli;
mod commands;
mod context;
mod diagnostics;
mod error;
mod keys;
mod pedant;
mod pg;
mod secrets;
mod service;

pub use cli::{BackupFormat, Cli, Command, MaintenanceAction, ServiceAction};
pub use error::CtlError;

use clap::{CommandFactory, FromArgMatches};

/// Run the CLI using process arguments.
pub fn run() -> Result<(), CtlError> {
    let program = std::env::args_os()
        .next()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "microwave-ctl".into());

    let matches = Cli::command().bin_name(program).get_matches();
    let cli =
        Cli::from_arg_matches(&matches).map_err(|error| CtlError::Message(error.to_string()))?;
    dispatch(cli)
}

/// Execute an already-parsed command. Useful for tests and embedders.
pub fn dispatch(cli: Cli) -> Result<(), CtlError> {
    // Commands that do not touch configuration must run even when the
    // configured file is missing or invalid: a broken install still needs to
    // be able to run the pedant harness or stop the service.
    match cli.command {
        Command::Version => {
            println!("microwave-ctl {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Command::Pedant { tags } => return pedant::run(&tags),
        Command::Service {
            action,
            unit,
            lines,
            follow,
            foreground,
        } => {
            return service::run(
                action,
                unit.as_deref(),
                lines,
                follow,
                cli.config.as_deref(),
                foreground,
            )
        }
        _ => {}
    }

    let context = context::Context::load(cli.config.as_deref())?;

    match cli.command {
        Command::Version => unreachable!("version was handled above"),
        Command::Pedant { .. } => unreachable!("pedant was handled above"),
        Command::Service { .. } => unreachable!("service was handled above"),
        Command::Status => commands::status(&context),
        Command::UserCreate {
            name,
            email,
            password,
            admin,
        } => commands::user_create(&context, &name, email.as_deref(), password, admin),
        Command::UserList => commands::user_list(&context),
        Command::UserShow { name } => commands::user_show(&context, &name),
        Command::UserDelete { name } => commands::user_delete(&context, &name),
        Command::OrgCreate {
            name,
            full_name,
            creator,
        } => commands::org_create(&context, &name, full_name.as_deref(), creator.as_deref()),
        Command::OrgList => commands::org_list(&context),
        Command::OrgShow { name } => commands::org_show(&context, &name),
        Command::OrgDelete { name } => commands::org_delete(&context, &name),
        Command::OrgUserAdd { org, user } => commands::org_user_add(&context, &org, &user),
        Command::OrgUserRemove { org, user } => commands::org_user_remove(&context, &org, &user),
        Command::GrantServerAdminPermissions { user } => {
            commands::grant_server_admin_permissions(&context, &user)
        }
        Command::RemoveServerAdminPermissions { user } => {
            commands::remove_server_admin_permissions(&context, &user)
        }
        Command::ListServerAdmins => commands::list_server_admins(&context),
        Command::Password { user } => commands::password(&context, &user),
        Command::AddClientKey {
            org,
            client,
            key_name,
            expiration_date,
            public_key_path,
        } => keys::add_client_key(
            &context,
            &org,
            &client,
            &keys::AddKeyOptions {
                key_name: key_name.as_deref(),
                expiration_date: &expiration_date,
                public_key_path: public_key_path.as_deref(),
            },
        ),
        Command::AddUserKey {
            user,
            key_name,
            expiration_date,
            public_key_path,
        } => keys::add_user_key(
            &context,
            &user,
            &keys::AddKeyOptions {
                key_name: key_name.as_deref(),
                expiration_date: &expiration_date,
                public_key_path: public_key_path.as_deref(),
            },
        ),
        Command::ListClientKeys {
            org,
            client,
            verbose,
        } => keys::list_client_keys(&context, &org, &client, verbose),
        Command::ListUserKeys { user, verbose } => keys::list_user_keys(&context, &user, verbose),
        Command::DeleteClientKey { org, client, key } => {
            keys::delete_client_key(&context, &org, &client, &key)
        }
        Command::DeleteUserKey { user, key } => keys::delete_user_key(&context, &user, &key),
        Command::ShowSecret { group, name } => secrets::show_secret(&context, &group, &name),
        Command::SetSecret { group, name, value } => {
            secrets::set_secret(&context, &group, &name, &value)
        }
        Command::RemoveSecret { group, name } => secrets::remove_secret(&context, &group, &name),
        Command::Maintenance { action } => secrets::maintenance(&context, action),
        Command::CheckConfig => diagnostics::check_config(&context),
        Command::Reconfigure => commands::reconfigure(&context),
        Command::HaStatus => diagnostics::ha_status(&context),
        Command::Psql { args } => diagnostics::psql(&context, &args),
        Command::Reindex { org, purge } => commands::reindex(&context, org.as_deref(), purge),
        Command::Backup { path, format, tar } => backup::backup(
            &context,
            &path,
            if tar {
                BackupFormat::Tar
            } else {
                format.unwrap_or(BackupFormat::Directory)
            },
        ),
        Command::Restore {
            path,
            yes,
            format,
            tar,
        } => backup::restore(
            &context,
            &path,
            yes,
            if tar { Some(BackupFormat::Tar) } else { format },
        ),
        Command::MigrateFromLegacy { input, yes } => {
            commands::migrate_from_legacy(&context, &input, yes)
        }
    }
}
