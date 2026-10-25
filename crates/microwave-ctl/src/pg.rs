//! PostgreSQL connection helpers for spawning client tools.
//!
//! The CLI must never put a database password in a child process argument
//! list: on most systems any local user can read `/proc/<pid>/cmdline` (or
//! equivalent process listings). These helpers strip authority and
//! query-string passwords from the URL, pass the sanitized URL to
//! `pg_dump`/`pg_restore`/`psql`, and hand passwords over in the child
//! environment (`PGPASSWORD`) or a private service file (`PGSERVICEFILE` +
//! `PGSERVICE`) when the secret is an `sslpassword`.

use crate::CtlError;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use url::form_urlencoded;
use url::Url;
use uuid::Uuid;

const SERVICE_NAME: &str = "microwave_cli";

/// A PostgreSQL connection URL with passwords removed, plus the secrets
/// needed for the child process environment.
pub struct PgTarget {
    url: String,
    password: Option<String>,
    service_file: Option<PathBuf>,
    display: String,
    database: Option<String>,
}

impl PgTarget {
    /// Parse a `postgres://` or `postgresql://` URL.
    ///
    /// Supports both the authority form (`postgres://user:pass@host/db`) and
    /// libpq/sqlx query parameters (`?host=...&port=...&user=...&dbname=...`).
    /// Non-secret query parameters are preserved for the client tool.
    pub fn parse(database_url: &str) -> Result<Self, CtlError> {
        let mut parsed = Url::parse(database_url).map_err(|_| invalid_url())?;
        if parsed.scheme() != "postgres" && parsed.scheme() != "postgresql" {
            return Err(invalid_url());
        }

        let mut password = parsed.password().map(percent_decode).transpose()?;
        let mut ssl_password: Option<String> = None;
        let mut host = parsed.host_str().map(str::to_string);
        let mut port = parsed.port();
        let mut username = if parsed.username().is_empty() {
            None
        } else {
            Some(percent_decode(parsed.username())?)
        };
        let path_database = parsed.path().trim_start_matches('/');
        let mut database = if path_database.is_empty() {
            None
        } else {
            Some(percent_decode(path_database)?)
        };

        let mut remaining_query = Vec::new();
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "password" => password = Some(value.into_owned()),
                "sslpassword" => ssl_password = Some(value.into_owned()),
                "dbname" => {
                    database = Some(value.clone().into_owned());
                    remaining_query.push((key, value));
                }
                "user" => {
                    username = Some(value.clone().into_owned());
                    remaining_query.push((key, value));
                }
                "host" => {
                    host = Some(value.clone().into_owned());
                    remaining_query.push((key, value));
                }
                "port" => {
                    if let Ok(parsed_port) = value.parse::<u16>() {
                        port = Some(parsed_port);
                    }
                    remaining_query.push((key, value));
                }
                _ => remaining_query.push((key, value)),
            }
        }

        if remaining_query.is_empty() {
            parsed.set_query(None);
        } else {
            let mut serializer = form_urlencoded::Serializer::new(String::new());
            for (key, value) in &remaining_query {
                serializer.append_pair(key, value);
            }
            parsed.set_query(Some(&serializer.finish()));
        }
        parsed.set_password(None).map_err(|_| invalid_url())?;

        let host = host.unwrap_or_else(|| "localhost".to_string());
        let port = port.unwrap_or(5432);
        let user_prefix = username
            .as_ref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("{value}@"))
            .unwrap_or_default();
        let display = match &database {
            Some(database) => format!("{user_prefix}{host}:{port}/{database}"),
            None => format!("{user_prefix}{host}:{port}"),
        };

        let service_file = create_service_file(ssl_password.as_deref())?;

        Ok(Self {
            url: parsed.to_string(),
            password,
            service_file,
            display,
            database,
        })
    }

    /// Sanitized URL safe to pass in argv and print.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Human-readable `user@host:port/database`, never including a password.
    pub fn display(&self) -> &str {
        &self.display
    }

    /// Database name from the URL or `dbname` query parameter.
    pub fn database(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// Apply passwords to a child command's environment.
    ///
    /// libpq has no `PGSSLPASSWORD` environment variable, so an `sslpassword`
    /// query parameter is written to a temporary service file with mode
    /// `0600` and referenced through `PGSERVICEFILE`/`PGSERVICE`.
    pub fn apply_environment(&self, command: &mut Command) {
        if let Some(password) = &self.password {
            command.env("PGPASSWORD", password);
        }
        if let Some(path) = &self.service_file {
            command.env("PGSERVICEFILE", path);
            command.env("PGSERVICE", SERVICE_NAME);
        }
    }

    #[cfg(test)]
    fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    #[cfg(test)]
    fn service_file(&self) -> Option<&std::path::Path> {
        self.service_file.as_deref()
    }
}

impl Drop for PgTarget {
    fn drop(&mut self) {
        if let Some(path) = &self.service_file {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn create_service_file(ssl_password: Option<&str>) -> Result<Option<PathBuf>, CtlError> {
    let Some(password) = ssl_password else {
        return Ok(None);
    };
    let path = std::env::temp_dir().join(format!(
        "microwave-pg-service-{}-{}.conf",
        std::process::id(),
        Uuid::new_v4().simple()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|error| {
        CtlError::message(format!(
            "failed to create temporary PostgreSQL service file: {error}"
        ))
    })?;
    let write_result = writeln!(file, "[{SERVICE_NAME}]")
        .and_then(|_| writeln!(file, "sslpassword={}", conninfo_quote(password)));
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&path);
        return Err(CtlError::message(format!(
            "failed to write temporary PostgreSQL service file: {error}"
        )));
    }
    Ok(Some(path))
}

fn conninfo_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\\' || character == '\'' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('\'');
    quoted
}

/// Percent-decode a database name for display and archive comparison.
///
/// A literal `%` that does not start a valid escape is preserved.
pub fn normalize_database_name(value: &str) -> String {
    percent_decode(value.trim_start_matches('/')).unwrap_or_else(|_| value.to_string())
}

fn percent_decode(input: &str) -> Result<String, CtlError> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(decoded)
        .map_err(|_| CtlError::message("PostgreSQL database URL password is not valid UTF-8"))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_url() -> CtlError {
    CtlError::message("invalid PostgreSQL database URL")
}

#[cfg(test)]
mod tests {
    use super::{normalize_database_name, PgTarget};
    use std::process::Command;

    #[test]
    fn strips_authority_and_query_password_secrets() {
        let target = PgTarget::parse(
            "postgres://microwave:p%40ss%2Fword@db.example:5544/microwave\
             ?sslmode=require&password=query-secret&sslpassword=ssl%20secret",
        )
        .unwrap();

        assert_eq!(target.password(), Some("query-secret"));
        assert_eq!(target.display(), "microwave@db.example:5544/microwave");
        assert!(!target.url().contains("p%40ss"));
        assert!(!target.url().contains("p@ss"));
        assert!(!target.url().contains("query-secret"));
        assert!(!target.url().contains("ssl%20secret"));
        assert!(!target.url().contains("sslpassword"));
        assert!(!target.url().contains("password="));
        assert!(target.url().contains("sslmode=require"));
        assert!(target.url().contains("db.example:5544/microwave"));

        let service_file = target.service_file().expect("sslpassword service file");
        let service_file_path = service_file.to_path_buf();
        let contents = std::fs::read_to_string(service_file).unwrap();
        assert!(contents.contains("sslpassword='ssl secret'"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(service_file)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        let mut command = Command::new("pg_dump");
        target.apply_environment(&mut command);
        let environment = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<Vec<_>>();
        assert!(environment.contains(&("PGPASSWORD".to_string(), Some("query-secret".to_string()))));
        assert!(environment.contains(&(
            "PGSERVICEFILE".to_string(),
            Some(service_file.to_string_lossy().into_owned())
        )));
        assert!(environment.contains(&("PGSERVICE".to_string(), Some("microwave_cli".to_string()))));
        drop(target);
        assert!(!service_file_path.exists());
    }

    #[test]
    fn supports_query_connection_parameters_and_a_missing_path_database() {
        let target = PgTarget::parse(
            "postgres://ignored@base:1234?host=db.example&port=5544&user=admin\
             &dbname=microwave%2Dprod&sslmode=disable",
        )
        .unwrap();

        assert_eq!(target.database(), Some("microwave-prod"));
        assert_eq!(target.display(), "admin@db.example:5544/microwave-prod");
        assert!(target.url().contains("sslmode=disable"));
        assert!(target.url().contains("dbname=microwave-prod"));
        assert!(target.url().contains("user=admin"));
        assert!(target.url().contains("host=db.example"));
        assert!(target.url().contains("port=5544"));
    }

    #[test]
    fn accepts_a_url_without_a_database() {
        let target = PgTarget::parse("postgres://microwave@db?sslmode=disable").unwrap();
        assert_eq!(target.password(), None);
        assert_eq!(target.database(), None);
        assert_eq!(target.display(), "microwave@db:5432");
        assert!(target.url().contains("sslmode=disable"));
        assert!(!target.url().contains("db/"));
    }

    #[test]
    fn normalizes_database_names_for_comparison() {
        assert_eq!(
            normalize_database_name("microwave%2Dprod"),
            "microwave-prod"
        );
        assert_eq!(normalize_database_name("/microwave"), "microwave");
    }

    #[test]
    fn rejects_non_postgres_urls_without_echoing_them() {
        let error = match PgTarget::parse("mysql://user:secret@db/microwave") {
            Ok(_) => panic!("non-postgres URL should be rejected"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(!message.contains("secret"));
        assert!(message.contains("invalid PostgreSQL database URL"));
    }
}
