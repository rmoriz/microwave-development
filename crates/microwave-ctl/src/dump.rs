//! Filtered metadata dumps and log collection.

use crate::{context::Context, CtlError};
use chrono::Utc;
use flate2::{write::GzEncoder, Compression as GzipCompression};
use microwave_config::DatabaseDriver;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::SystemTime;
use url::Url;

/// Maximum number of bytes captured from a single command, per stream.
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// Attribute keys that upstream `chef-server-ctl filtered-dump` replaces so a
/// dump can be indexed without exposing node, role, or environment data.
const FILTERED_ATTRIBUTES: [&str; 8] = [
    "default_attributes",
    "normal_attributes",
    "override_attributes",
    "automatic_attributes",
    "normal",
    "default",
    "override",
    "automatic",
];

const LOG_README: &str = "README.txt";
const LOG_SYSTEM: &str = "system.txt";
const LOG_STATUS: &str = "status.txt";
const LOG_CONFIG: &str = "config.toml";
const LOG_JOURNAL: &str = "journal.txt";
const LOG_DATA_LISTING: &str = "data-listing.txt";

/// Write redacted, indexable metadata for every organization, or only the
/// selected one, as pretty-printed JSON files.
pub fn filtered_dump(context: &Context, output: &Path, org: Option<&str>) -> Result<(), CtlError> {
    let store = context.open_metadata_store()?;
    let organizations = store.run(store.store().list_orgs())?;
    let organizations = match org {
        Some(name) => {
            let selected = organizations
                .into_iter()
                .find(|organization| organization.name == name)
                .ok_or_else(|| CtlError::message(format!("organization {name} does not exist")))?;
            vec![selected]
        }
        None => organizations,
    };

    std::fs::create_dir_all(output)?;

    println!("Exporting organizations");
    let org_values: Vec<Value> = organizations
        .iter()
        .map(|organization| {
            json!({
                "id": organization.id,
                "name": organization.name,
                "created_at": organization.created_at,
                "updated_at": organization.updated_at,
            })
        })
        .collect();
    write_json(&output.join("organizations.json"), &org_values)?;

    println!("Exporting nodes");
    let mut node_values = Vec::new();
    for organization in &organizations {
        let nodes = store.run(store.store().list_nodes(&organization.id))?;
        for node in nodes {
            node_values.push(json!({
                "org_id": organization.id,
                "id": node.id,
                "name": node.name,
                "environment": node.environment,
                "created_at": node.created_at,
                "updated_at": node.updated_at,
                "serialized_object": redact_serialized(&node.serialized_object, "node"),
            }));
        }
    }
    write_json(&output.join("nodes.json"), &node_values)?;

    println!("Exporting environments");
    let mut environment_values = Vec::new();
    for organization in &organizations {
        let environments = store.run(store.store().list_environments(&organization.id))?;
        for environment in environments {
            environment_values.push(json!({
                "org_id": organization.id,
                "id": environment.id,
                "name": environment.name,
                "created_at": environment.created_at,
                "updated_at": environment.updated_at,
                "serialized_object": redact_serialized(
                    &environment.serialized_object,
                    "environment",
                ),
            }));
        }
    }
    write_json(&output.join("environments.json"), &environment_values)?;

    println!("Exporting roles");
    let mut role_values = Vec::new();
    for organization in &organizations {
        let roles = store.run(store.store().list_roles(&organization.id))?;
        for role in roles {
            role_values.push(json!({
                "org_id": organization.id,
                "id": role.id,
                "name": role.name,
                "created_at": role.created_at,
                "updated_at": role.updated_at,
                "serialized_object": redact_serialized(&role.serialized_object, "role"),
            }));
        }
    }
    write_json(&output.join("roles.json"), &role_values)?;

    println!("Exporting data bag items");
    let mut data_bag_values = Vec::new();
    for organization in &organizations {
        let data_bags = store.run(store.store().list_data_bags(&organization.id))?;
        for data_bag in data_bags {
            let items = store.run(
                store
                    .store()
                    .list_data_bag_items(&organization.id, &data_bag.name),
            )?;
            for item in items {
                data_bag_values.push(json!({
                    "org_id": organization.id,
                    "id": item.id,
                    "data_bag_name": item.data_bag_name,
                    "item_name": item.item_name,
                    "created_at": item.created_at,
                    "updated_at": item.updated_at,
                    "serialized_object": redact_data_bag_item(
                        &item.serialized_object,
                        &item.data_bag_name,
                        &item.item_name,
                    ),
                }));
            }
        }
    }
    write_json(&output.join("data_bag_items.json"), &data_bag_values)?;

    println!("Exporting clients");
    let mut client_values = Vec::new();
    for organization in &organizations {
        let clients = store.run(store.store().list_clients(&organization.id))?;
        for client in clients {
            client_values.push(json!({
                "org_id": organization.id,
                "id": client.id,
                "name": client.name,
                "validator": client.validator,
                "admin": client.admin,
                "created_at": client.created_at,
                "updated_at": client.updated_at,
            }));
        }
    }
    write_json(&output.join("clients.json"), &client_values)?;

    println!("Output directory: {}", output.display());
    Ok(())
}

fn write_json(path: &Path, values: &[Value]) -> Result<(), CtlError> {
    let mut text = serde_json::to_string_pretty(values).map_err(|error| {
        CtlError::message(format!(
            "could not serialize {} as JSON: {error}",
            path.display()
        ))
    })?;
    text.push('\n');
    std::fs::write(path, text)?;
    Ok(())
}

fn redact_serialized(serialized_object: &str, kind: &str) -> Value {
    match serde_json::from_str::<Value>(serialized_object) {
        Ok(mut value) => {
            redact_attributes(&mut value);
            value
        }
        Err(_) => {
            json!({ "error": format!("could not parse {kind} serialized_object as json") })
        }
    }
}

/// Replace attribute payloads at any depth, matching upstream filtered-dump's
/// recursive walk. The replacement reports how many entries were removed.
fn redact_attributes(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                if FILTERED_ATTRIBUTES.contains(&key.as_str()) {
                    let entries = match child {
                        Value::Object(entries) => entries.len(),
                        Value::Array(entries) => entries.len(),
                        _ => 0,
                    };
                    *child = Value::String(format!("removed {entries} entries"));
                } else {
                    redact_attributes(child);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_attributes(value);
            }
        }
        _ => {}
    }
}

fn redact_data_bag_item(serialized_object: &str, data_bag_name: &str, item_name: &str) -> Value {
    let Ok(value) = serde_json::from_str::<Value>(serialized_object) else {
        return json!({
            "error": "could not parse data bag item serialized_object as json"
        });
    };
    let Value::Object(mut object) = value else {
        return json!({
            "error": "data bag item serialized_object is not a json object"
        });
    };

    if let Some(id) = object.get("id").and_then(Value::as_str) {
        if id != item_name {
            eprintln!(
                "warning: data bag item {data_bag_name}/{item_name} has id {id:?}; expected {item_name:?}"
            );
        }
    }
    for (key, value) in object.iter_mut() {
        if key != "id" {
            *value = Value::String("value removed".into());
        }
    }
    Value::Object(object)
}

/// Create a `.tar.gz` with recent logs and redacted system information.
pub fn gather_logs(context: &Context, output: Option<&Path>) -> Result<(), CtlError> {
    let output = match output {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(format!(
            "microwave-logs-{}.tar.gz",
            Utc::now().format("%Y%m%dT%H%M%SZ")
        )),
    };

    match std::fs::symlink_metadata(&output) {
        Ok(_) => {
            return Err(CtlError::message(format!(
                "log archive {} already exists; choose a new path or remove it",
                output.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;

    let entries = vec![
        (LOG_README, readme_report()),
        (LOG_SYSTEM, system_report(context)),
        (LOG_STATUS, status_report(context)),
        (LOG_CONFIG, config_report(context)),
        (LOG_JOURNAL, journal_report()),
        (LOG_DATA_LISTING, data_listing(context)?),
    ];
    let entry_count = entries.len();

    write_log_archive(&output, parent, &entries)?;
    println!("Log archive written to {}", output.display());
    println!("Entries: {entry_count}");
    Ok(())
}

fn readme_report() -> String {
    let mut out = String::new();
    writeln!(out, "Microwave Server diagnostic log bundle").unwrap();
    writeln!(out, "Generated at: {}", Utc::now().to_rfc3339()).unwrap();
    writeln!(out).unwrap();
    writeln!(out, "Contents:").unwrap();
    writeln!(
        out,
        "- system.txt: operating system, CLI version, data directory, database driver, \
         filesystem usage, and SQLite file size."
    )
    .unwrap();
    writeln!(
        out,
        "- status.txt: non-secret data directory, database readability, superuser, and \
         maintenance-mode state."
    )
    .unwrap();
    writeln!(
        out,
        "- config.toml: a redacted summary of non-secret configuration settings."
    )
    .unwrap();
    writeln!(
        out,
        "- journal.txt: the last 1000 lines of the service journal, when journalctl is \
         available."
    )
    .unwrap();
    writeln!(
        out,
        "- data-listing.txt: names and sizes of the data directory's top-level entries."
    )
    .unwrap();
    writeln!(out).unwrap();
    writeln!(out, "Excluded on purpose:").unwrap();
    writeln!(
        out,
        "- database files and database contents (including operator-managed secrets)"
    )
    .unwrap();
    writeln!(out, "- bookshelf and object-store blobs").unwrap();
    writeln!(out, "- private keys, TLS keys, and credentials").unwrap();
    writeln!(out, "- environment variables").unwrap();
    writeln!(
        out,
        "- arbitrary files from disk, and full command output beyond the 4 MiB capture cap"
    )
    .unwrap();
    out
}

fn system_report(context: &Context) -> String {
    let mut out = String::new();
    writeln!(out, "Microwave Server system information").unwrap();
    writeln!(out, "Generated at: {}", Utc::now().to_rfc3339()).unwrap();
    writeln!(out, "CLI version: {}", env!("CARGO_PKG_VERSION")).unwrap();
    writeln!(
        out,
        "Current directory: {}",
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|error| format!("unavailable: {error}"))
    )
    .unwrap();
    writeln!(out, "Data directory: {}", context.data_dir.display()).unwrap();
    writeln!(
        out,
        "Database driver: {}",
        database_driver_name(context.config.database.driver)
    )
    .unwrap();
    writeln!(out).unwrap();

    let mut uname = Command::new("uname");
    uname.arg("-a");
    command_section(&mut out, "uname -a", uname);

    writeln!(out, "== /etc/os-release ==").unwrap();
    match std::fs::read_to_string("/etc/os-release") {
        Ok(contents) => {
            out.push_str(&contents);
            if !contents.ends_with('\n') {
                out.push('\n');
            }
        }
        Err(error) => writeln!(out, "unavailable: {error}").unwrap(),
    }
    out.push('\n');

    if context.data_dir.exists() {
        let mut command = Command::new("df");
        command.arg("-h").arg(&context.data_dir);
        command_section(
            &mut out,
            &format!("df -h {}", context.data_dir.display()),
            command,
        );
    } else {
        writeln!(out, "== df -h ==").unwrap();
        writeln!(
            out,
            "unavailable: data directory {} does not exist",
            context.data_dir.display()
        )
        .unwrap();
        out.push('\n');
    }

    writeln!(out, "== SQLite database ==").unwrap();
    match &context.database_path {
        Some(path) => match std::fs::metadata(path) {
            Ok(metadata) => {
                writeln!(out, "{}: {} bytes", path.display(), metadata.len()).unwrap();
            }
            Err(error) => {
                writeln!(out, "{}: unavailable: {error}", path.display()).unwrap();
            }
        },
        None => {
            writeln!(out, "not applicable: the postgresql driver is configured").unwrap();
        }
    }
    out
}

fn status_report(context: &Context) -> String {
    let (readable, superuser_present, maintenance) = probe_store(context);
    let mut out = String::new();
    writeln!(out, "data_dir: {}", context.data_dir.display()).unwrap();
    writeln!(
        out,
        "database_driver: {}",
        database_driver_name(context.config.database.driver)
    )
    .unwrap();
    writeln!(out, "database_readable: {readable}").unwrap();
    writeln!(
        out,
        "superuser: {} ({})",
        context.superuser_name(),
        if superuser_present {
            "present"
        } else {
            "absent"
        }
    )
    .unwrap();
    let maintenance = match maintenance {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "unknown",
    };
    writeln!(out, "maintenance: {maintenance}").unwrap();
    out
}

/// Probe the metadata store without creating a database file. A probe failure
/// is reported as unreadable rather than making `gather-logs` fail: a broken
/// database is exactly when the operator needs the log bundle.
fn probe_store(context: &Context) -> (bool, bool, Option<bool>) {
    if let Some(path) = &context.database_path {
        if !path.is_file() {
            return (false, false, None);
        }
    }

    let Ok(store) = context.open_existing_metadata_store() else {
        return (false, false, None);
    };
    match store.run(store.store().find_user(context.superuser_name())) {
        Ok(user) => {
            let maintenance = store.run(store.store().maintenance_enabled()).ok();
            (true, user.is_some(), maintenance)
        }
        Err(_) => (false, false, None),
    }
}

fn config_report(context: &Context) -> String {
    let config = &context.config;
    let mut out = String::new();
    out.push_str(
        "# Redacted Microwave Server configuration summary.\n\
         # Generated by gather-logs. Credentials, secrets, and private key material are omitted.\n\n",
    );

    out.push_str("[server]\n");
    writeln!(out, "bind = {}", toml_string(&config.server.bind)).unwrap();
    writeln!(out, "tls_enabled = {}", config.server.tls.is_enabled()).unwrap();
    writeln!(
        out,
        "tls_cert_path = {}",
        toml_string(config.server.tls.cert_path.as_deref().unwrap_or(""))
    )
    .unwrap();
    writeln!(
        out,
        "tls_key_path = {}",
        toml_string(config.server.tls.key_path.as_deref().unwrap_or(""))
    )
    .unwrap();

    out.push_str("\n[paths]\n");
    writeln!(out, "data_dir = {}", toml_string(&config.paths.data_dir)).unwrap();

    out.push_str("\n[database]\n");
    writeln!(
        out,
        "driver = {}",
        toml_string(database_driver_name(config.database.driver))
    )
    .unwrap();

    out.push_str("\n[search]\n");
    writeln!(
        out,
        "backend = {}",
        toml_string(crate::diagnostics::search_backend_name(
            config.search.backend
        ))
    )
    .unwrap();
    writeln!(
        out,
        "url = {}",
        toml_string(&redacted_url(config.search.url.as_deref().unwrap_or("")))
    )
    .unwrap();

    out.push_str("\n[object_store]\n");
    writeln!(
        out,
        "backend = {}",
        toml_string(crate::diagnostics::object_store_backend_name(
            config.object_store.backend
        ))
    )
    .unwrap();
    writeln!(
        out,
        "endpoint = {}",
        toml_string(&redacted_url(
            config.object_store.endpoint.as_deref().unwrap_or("")
        ))
    )
    .unwrap();
    writeln!(
        out,
        "bucket = {}",
        toml_string(config.object_store.bucket.as_deref().unwrap_or(""))
    )
    .unwrap();

    out
}

/// Build a TOML basic string. JSON string escaping is compatible with TOML for
/// the characters that can appear in these values.
fn toml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

/// Strip userinfo, query, and fragment from a URL so diagnostics cannot leak
/// credentials embedded in a backend URL. Unparseable values are not echoed.
fn redacted_url(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let Ok(url) = Url::parse(value) else {
        return "<unparseable URL omitted>".into();
    };
    let host = url.host_str().unwrap_or_default();
    let port = url
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    let path = url.path();
    format!("{}://{host}{port}{path}", url.scheme())
}

fn journal_report() -> String {
    let unit = std::env::var("MW_SERVICE_UNIT")
        .ok()
        .map(|unit| unit.trim().to_string())
        .filter(|unit| {
            !unit.is_empty()
                && unit
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '@' | '-'))
        })
        .unwrap_or_else(|| "microwave-server".into());

    let mut out = String::new();
    writeln!(out, "Unit: {unit}").unwrap();
    writeln!(out, "Command: journalctl -u {unit} -n 1000 --no-pager\n").unwrap();

    let mut command = Command::new("journalctl");
    command.args(["-u", unit.as_str(), "-n", "1000", "--no-pager"]);
    match run_capped(command) {
        Ok(captured) => {
            if captured.status.success() {
                append_captured(&mut out, &captured.stdout, captured.stdout_truncated);
                if !captured.stderr.is_empty() {
                    out.push_str("[stderr]\n");
                    append_captured(&mut out, &captured.stderr, captured.stderr_truncated);
                }
            } else {
                writeln!(out, "journalctl failed with {}.", captured.status).unwrap();
                let stderr = String::from_utf8_lossy(&captured.stderr);
                if !stderr.trim().is_empty() {
                    writeln!(out, "reason:\n{stderr}").unwrap();
                }
                let stdout = String::from_utf8_lossy(&captured.stdout);
                if !stdout.trim().is_empty() {
                    writeln!(out, "stdout (truncated):\n{stdout}").unwrap();
                }
            }
        }
        Err(error) => {
            writeln!(out, "journalctl could not be run: {error}").unwrap();
        }
    }
    out
}

fn data_listing(context: &Context) -> Result<String, CtlError> {
    let mut out = String::new();
    writeln!(out, "Data directory: {}", context.data_dir.display()).unwrap();
    writeln!(out, "Names and sizes only. File contents are not included.").unwrap();
    writeln!(out).unwrap();

    if !context.data_dir.exists() {
        writeln!(out, "The data directory does not exist.").unwrap();
        return Ok(out);
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&context.data_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = std::fs::symlink_metadata(entry.path())?;
        let kind = if metadata.file_type().is_symlink() {
            "symlink"
        } else if metadata.is_dir() {
            "directory"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        let size = if metadata.is_file() {
            format!("{} bytes", metadata.len())
        } else {
            "-".to_string()
        };
        entries.push((name, kind, size));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (name, kind, size) in entries {
        writeln!(out, "{name}\t{kind}\t{size}").unwrap();
    }
    Ok(out)
}

fn write_log_archive(
    output: &Path,
    parent: &Path,
    entries: &[(&str, String)],
) -> Result<(), CtlError> {
    // Build beside the destination and rename, so a failure never leaves a
    // partial archive under the requested name.
    let temp = tempfile::Builder::new()
        .prefix(".microwave-logs-")
        .suffix(".tar.gz")
        .tempfile_in(parent)?;
    let temp_path = temp.into_temp_path();

    {
        let file = std::fs::File::create(&temp_path)?;
        let encoder = GzEncoder::new(file, GzipCompression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(contents.len() as u64);
            header.set_mode(0o600);
            header.set_mtime(archive_mtime());
            header.set_cksum();
            builder.append_data(&mut header, *name, contents.as_bytes())?;
        }
        builder.finish()?;
        let encoder = builder.into_inner()?;
        encoder.finish()?;
    }

    std::fs::rename(&temp_path, output)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(output, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn archive_mtime() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn command_section(out: &mut String, heading: &str, command: Command) {
    writeln!(out, "== {heading} ==").unwrap();
    match run_capped(command) {
        Ok(captured) => {
            if !captured.status.success() {
                writeln!(out, "[command exited with {}]", captured.status).unwrap();
            }
            append_captured(out, &captured.stdout, captured.stdout_truncated);
            if !captured.stderr.is_empty() {
                out.push_str("[stderr]\n");
                append_captured(out, &captured.stderr, captured.stderr_truncated);
            }
        }
        Err(error) => {
            writeln!(out, "unavailable: {error}").unwrap();
        }
    }
    out.push('\n');
}

fn append_captured(out: &mut String, bytes: &[u8], truncated: bool) {
    out.push_str(&String::from_utf8_lossy(bytes));
    if truncated {
        writeln!(out, "\n[output truncated at {MAX_CAPTURE_BYTES} bytes]").unwrap();
    } else if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        out.push('\n');
    }
}

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

/// Run a command and read each stream up to [`MAX_CAPTURE_BYTES`]. Reading with
/// a limit (rather than `Command::output`) bounds memory as well as the amount
/// written into the archive.
fn run_capped(mut command: Command) -> Result<Captured, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start command: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "stdout was not captured".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "stderr was not captured".to_string())?;

    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let status = child
        .wait()
        .map_err(|error| format!("could not wait for command: {error}"))?;

    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "stdout reader panicked".to_string())?
        .map_err(|error| format!("could not read stdout: {error}"))?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "stderr reader panicked".to_string())?
        .map_err(|error| format!("could not read stderr: {error}"))?;

    Ok(Captured {
        status,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_capped(mut reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut data = Vec::new();
    reader
        .by_ref()
        .take((MAX_CAPTURE_BYTES + 1) as u64)
        .read_to_end(&mut data)?;
    let truncated = data.len() > MAX_CAPTURE_BYTES;
    data.truncate(MAX_CAPTURE_BYTES);
    Ok((data, truncated))
}

fn database_driver_name(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => "sqlite",
        DatabaseDriver::Postgres => "postgresql",
    }
}
