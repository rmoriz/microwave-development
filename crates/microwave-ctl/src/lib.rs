//! Operational CLI for Microwave Server.
//!
//! The crate is shared by the `microwave-ctl` binary and the
//! `chef-server-ctl` compatibility alias, so both expose exactly the same
//! command surface.

mod admin;
mod backup;
mod cli;
mod commands;
mod context;
mod diagnostics;
mod dump;
mod error;
mod keys;
mod maintenance;
mod pedant;
mod pg;
mod rotation;
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
        Command::Start {
            service,
            unit,
            foreground,
        } => {
            let unit = unit.or(service);
            return service::run(
                ServiceAction::Start,
                unit.as_deref(),
                50,
                false,
                cli.config.as_deref(),
                foreground,
            );
        }
        Command::Stop { service, unit } => {
            let unit = unit.or(service);
            return service::run(
                ServiceAction::Stop,
                unit.as_deref(),
                50,
                false,
                cli.config.as_deref(),
                false,
            );
        }
        Command::Restart { service, unit } => {
            let unit = unit.or(service);
            return service::run(
                ServiceAction::Restart,
                unit.as_deref(),
                50,
                false,
                cli.config.as_deref(),
                false,
            );
        }
        Command::Hup { service, unit } => {
            let unit = unit.or(service);
            // The server is one systemd unit, so a HUP is implemented as a
            // restart. This is the same action as `service restart`.
            return service::run(
                ServiceAction::Restart,
                unit.as_deref(),
                50,
                false,
                cli.config.as_deref(),
                false,
            );
        }
        Command::Tail {
            service,
            unit,
            lines,
            follow,
        } => {
            let unit = unit.or(service);
            return service::run(
                ServiceAction::Tail,
                unit.as_deref(),
                lines,
                follow,
                cli.config.as_deref(),
                false,
            );
        }
        Command::ServiceList { unit } => return commands::service_list(unit.as_deref()),
        _ => {}
    }

    let context = context::Context::load(cli.config.as_deref())?;

    match cli.command {
        Command::Version => unreachable!("version was handled above"),
        Command::Pedant { .. } => unreachable!("pedant was handled above"),
        Command::Service { .. } => unreachable!("service was handled above"),
        Command::Start { .. }
        | Command::Stop { .. }
        | Command::Restart { .. }
        | Command::Hup { .. }
        | Command::Tail { .. }
        | Command::ServiceList { .. } => unreachable!("service alias was handled above"),
        Command::Status => commands::status(&context),
        Command::UserCreate {
            positional,
            email,
            password,
            admin,
            filename,
            public_key_path,
        } => commands::user_create(
            &context,
            &positional,
            email.as_deref(),
            password,
            admin,
            filename.as_deref(),
            public_key_path.as_deref(),
        ),
        Command::UserList => commands::user_list(&context),
        Command::UserShow { name } => commands::user_show(&context, &name),
        Command::UserDelete { name } => commands::user_delete(&context, &name),
        Command::OrgCreate {
            name,
            full_name,
            full_name_flag,
            creator,
            association_user,
            filename,
        } => commands::org_create(
            &context,
            &name,
            full_name_flag.or(full_name).as_deref(),
            creator.as_deref(),
            association_user.as_deref(),
            filename.as_deref(),
        ),
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
        Command::ShowServiceCredentials => commands::show_service_credentials(&context),
        Command::RequireCredentialRotation { yes } => {
            commands::require_credential_rotation(&context, yes)
        }
        Command::Maintenance { action } => secrets::maintenance(&context, action),
        Command::CheckConfig => diagnostics::check_config(&context),
        Command::Reconfigure => commands::reconfigure(&context),
        Command::HaStatus => diagnostics::ha_status(&context),
        Command::Psql { args } => {
            let invocation = diagnostics::parse_psql_args(&args)?;
            diagnostics::psql(&context, &invocation)
        }
        Command::Reindex { org, purge } => commands::reindex(&context, org.as_deref(), purge),
        Command::Backup {
            path,
            format,
            tar,
            yes,
            config_only,
            pg_options,
            include_private_keys,
        } => backup::backup(
            &context,
            path.as_deref(),
            if tar {
                BackupFormat::Tar
            } else {
                format.unwrap_or(BackupFormat::Directory)
            },
            &backup::BackupOptions {
                yes,
                config_only,
                pg_options: pg_options.as_deref(),
                include_private_keys,
            },
        ),
        Command::Restore {
            path,
            yes,
            format,
            tar,
            pg_options,
            restore_private_keys,
            private_only,
        } => backup::restore(
            &context,
            &path,
            yes,
            if tar { Some(BackupFormat::Tar) } else { format },
            pg_options.as_deref(),
            restore_private_keys,
            private_only,
        ),
        Command::MigrateFromLegacy { input, yes } => {
            commands::migrate_from_legacy(&context, &input, yes)
        }
        Command::UserEdit { user, input } => admin::user_edit(&context, &user, input.as_deref()),
        Command::OcIdShowApp { app } => admin::oc_id_show_app(&context, &app),
        Command::CleanupBifrost {
            estimate_only,
            wait_time,
            force_cleanup,
        } => maintenance::cleanup_bifrost(&context, estimate_only, wait_time, force_cleanup),
        Command::CleanupOrphanBlobs { estimate_only } => {
            commands::cleanup_orphan_blobs(&context, estimate_only)
        }
        Command::FilteredDump { output, org } => {
            dump::filtered_dump(&context, &output, org.as_deref())
        }
        Command::GatherLogs { output } => dump::gather_logs(&context, output.as_deref()),
        Command::RotateCredentials { service } => rotation::rotate_credentials(&context, &service),
        Command::RotateAllCredentials => rotation::rotate_all_credentials(&context),
        Command::RotateSharedSecrets => {
            rotation::rotate_shared_secrets(&context)?;
            commands::clear_credential_rotation_marker(&context)
        }
    }
}
