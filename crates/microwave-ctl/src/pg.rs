//! PostgreSQL connection helpers for spawning client tools.
//!
//! The CLI must never put a database password in a child process argument
//! list: on most systems any local user can read `/proc/<pid>/cmdline` (or
//! equivalent process listings). These helpers strip the password from the
//! URL, pass the sanitized URL to `pg_dump`/`pg_restore`/`psql`, and hand the
//! password over in `PGPASSWORD` instead.

use crate::CtlError;
use std::process::Command;
use url::Url;

/// A PostgreSQL connection URL with the password removed, plus the password
/// needed for the child process environment.
pub struct PgTarget {
    url: String,
    password: Option<String>,
    display: String,
    database: String,
}

impl PgTarget {
    /// Parse a `postgres://` or `postgresql://` URL.
    ///
    /// All non-password URL components (host, port, username, database, and
    /// query parameters such as `sslmode`) are preserved for the client tool.
    pub fn parse(database_url: &str) -> Result<Self, CtlError> {
        let mut parsed = Url::parse(database_url).map_err(|_| invalid_url())?;
        if parsed.scheme() != "postgres" && parsed.scheme() != "postgresql" {
            return Err(invalid_url());
        }

        let password = parsed.password().map(percent_decode).transpose()?;
        let host = parsed.host_str().unwrap_or("localhost");
        let port = parsed.port().unwrap_or(5432);
        let database = parsed.path().trim_start_matches('/').to_string();
        if database.is_empty() {
            return Err(CtlError::message(
                "PostgreSQL database URL does not name a database",
            ));
        }
        let username = parsed.username();
        let display = if username.is_empty() {
            format!("{host}:{port}/{database}")
        } else {
            format!("{username}@{host}:{port}/{database}")
        };

        parsed.set_password(None).map_err(|_| invalid_url())?;
        Ok(Self {
            url: parsed.to_string(),
            password,
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

    /// Database name from the URL, used to compare a restore archive's source.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// Apply the password to a child command's environment.
    pub fn apply_environment(&self, command: &mut Command) {
        if let Some(password) = &self.password {
            command.env("PGPASSWORD", password);
        }
    }

    #[cfg(test)]
    fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
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
    use super::PgTarget;
    use std::process::Command;

    #[test]
    fn strips_password_from_url_and_display_but_sets_the_environment() {
        let target = PgTarget::parse(
            "postgres://microwave:p%40ss%2Fword@db.example:5544/microwave?sslmode=require",
        )
        .unwrap();

        assert_eq!(target.password(), Some("p@ss/word"));
        assert_eq!(target.display(), "microwave@db.example:5544/microwave");
        assert!(!target.url().contains("p%40ss"));
        assert!(!target.url().contains("p@ss"));
        assert!(target.url().contains("sslmode=require"));
        assert!(target.url().contains("db.example:5544/microwave"));

        let mut command = Command::new("pg_dump");
        target.apply_environment(&mut command);
        let password = command
            .get_envs()
            .find(|(key, _)| *key == "PGPASSWORD")
            .and_then(|(_, value)| value)
            .map(|value| value.to_string_lossy().into_owned());
        assert_eq!(password.as_deref(), Some("p@ss/word"));
    }

    #[test]
    fn leaves_a_passwordless_url_alone() {
        let target = PgTarget::parse("postgresql://microwave@db/microwave").unwrap();
        assert_eq!(target.password(), None);
        assert_eq!(target.display(), "microwave@db:5432/microwave");
        assert_eq!(target.url(), "postgresql://microwave@db/microwave");
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
