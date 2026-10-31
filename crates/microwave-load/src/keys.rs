//! Keygen and the `--keys` manifest.
//!
//! One RSA-2048 keypair per target and canary actor protects the run from a
//! shared-key artifact; `--shared-key` trades that for generation time. The
//! manifest lists every entry as `<kind> <name> <private-key-path>` and is
//! written `0600` because the paths lead to private keys.

use crate::fixture::{stamp, Fixture, KeyEntry, KeyKind};
use crate::LoadError;
use microwave_crypto::{generate_rsa_keypair, public_key_fingerprint};
use microwave_store::ActorKeyRow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// A generated actor key, in manifest order.
#[derive(Debug, Clone)]
pub struct ActorKey {
    pub entry: KeyEntry,
    pub private_key_pem: String,
}

/// Directory that holds the private key files for `manifest`.
pub fn key_dir(manifest: &Path) -> PathBuf {
    let mut name = manifest.file_name().unwrap_or_default().to_os_string();
    name.push(".d");
    manifest.with_file_name(name)
}

/// Generate a keypair per fixture entry and fill the `actor_keys` rows.
pub fn generate(fixture: &mut Fixture, shared: bool) -> Result<Vec<ActorKey>, LoadError> {
    let entries = fixture.entries.clone();
    let pairs = if shared && !entries.is_empty() {
        let pair = generate_rsa_keypair().map_err(LoadError::Crypto)?;
        vec![pair; entries.len()]
    } else {
        generate_parallel(entries.len())?
    };

    let mut actors = Vec::with_capacity(entries.len());
    for (entry, (private_key_pem, public_key)) in entries.into_iter().zip(pairs) {
        let key_name = public_key_fingerprint(&public_key).map_err(LoadError::Crypto)?;
        fixture.snapshot.actor_keys.push(ActorKeyRow {
            actor_id: entry.actor_id.clone(),
            actor_type: match entry.kind {
                KeyKind::Superuser => "user".to_string(),
                KeyKind::Client | KeyKind::Canary => "client".to_string(),
            },
            key_name,
            public_key,
            key_version: 1,
            expires_at: "infinity".to_string(),
            last_updated_by: entry.name.clone(),
            created_at: stamp(),
            updated_at: stamp(),
        });
        actors.push(ActorKey {
            entry,
            private_key_pem,
        });
    }
    Ok(actors)
}

/// Generate `count` keypairs on a thread pool bounded by the CPU count.
fn generate_parallel(count: usize) -> Result<Vec<(String, String)>, LoadError> {
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(count.max(1));
    let mut slots = vec![None; count];
    let chunk = count.div_ceil(workers).max(1);
    std::thread::scope(|scope| -> Result<(), LoadError> {
        let mut handles = Vec::new();
        for slice in slots.chunks_mut(chunk) {
            handles.push(scope.spawn(move || -> Result<(), LoadError> {
                for slot in slice {
                    *slot = Some(generate_rsa_keypair().map_err(LoadError::Crypto)?);
                }
                Ok(())
            }));
        }
        for handle in handles {
            handle
                .join()
                .map_err(|_| LoadError::Message("keygen thread panicked".into()))??;
        }
        Ok(())
    })?;
    Ok(slots
        .into_iter()
        .map(|slot| slot.expect("every slot is filled"))
        .collect())
}

/// Write the private keys next to `manifest` and the manifest itself `0600`.
pub fn write_manifest(manifest: &Path, actors: &[ActorKey]) -> Result<(), LoadError> {
    if let Some(parent) = manifest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let directory = key_dir(manifest);
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;

    let mut lines = String::new();
    for actor in actors {
        let path = directory.join(format!("{}.pem", actor.entry.name));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(actor.private_key_pem.as_bytes())?;
        file.sync_all()?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        lines.push_str(&format!(
            "{} {} {}\n",
            actor.entry.kind.as_str(),
            actor.entry.name,
            path.display()
        ));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(manifest)?;
    file.write_all(lines.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(manifest, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// One manifest line with the private key already read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFile {
    pub kind: KeyKind,
    pub name: String,
    pub private_key: String,
}

/// Read the manifest [`write_manifest`] produced.
pub fn read_manifest(manifest: &Path) -> Result<Vec<KeyFile>, LoadError> {
    let text = fs::read_to_string(manifest)?;
    parse_manifest(&text)
}

/// Parse manifest text; the path is the rest of the line, so a key directory
/// with spaces stays usable.
fn parse_manifest(text: &str) -> Result<Vec<KeyFile>, LoadError> {
    let mut entries = Vec::new();
    for (number, line) in text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
    {
        let mut fields = line.splitn(3, ' ');
        let kind = match fields.next() {
            Some("superuser") => KeyKind::Superuser,
            Some("client") => KeyKind::Client,
            Some("canary") => KeyKind::Canary,
            other => {
                return Err(LoadError::Message(format!(
                    "manifest line {}: unknown kind {other:?}",
                    number + 1
                )))
            }
        };
        let name = fields.next().filter(|name| !name.is_empty());
        let path = fields.next().filter(|path| !path.is_empty());
        let (Some(name), Some(path)) = (name, path) else {
            return Err(LoadError::Message(format!(
                "manifest line {}: expected '<kind> <name> <private-key-path>'",
                number + 1
            )));
        };
        entries.push(KeyFile {
            kind,
            name: name.to_string(),
            private_key: fs::read_to_string(path)?,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Sizes;
    use std::os::unix::fs::MetadataExt;

    fn fixture() -> Fixture {
        Fixture::build(Sizes {
            nodes: 6,
            roles: 2,
            versions: 15,
            targets: 3,
            canary: 1,
        })
        .expect("fixture builds")
    }

    #[test]
    fn manifest_is_private_and_has_one_line_per_actor() {
        let mut fixture = fixture();
        let actors = generate(&mut fixture, true).expect("keys generate");
        let directory = tempfile::tempdir().expect("tempdir");
        let manifest = directory.path().join("load.keys");
        write_manifest(&manifest, &actors).expect("manifest writes");

        let mode = fs::metadata(&manifest).expect("manifest exists").mode() & 0o777;
        assert_eq!(mode, 0o600);
        let contents = fs::read_to_string(&manifest).expect("manifest reads");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("superuser load-superuser "));
        assert_eq!(
            lines[1..4]
                .iter()
                .filter(|line| line.starts_with("client "))
                .count(),
            3
        );
        assert!(lines[4].starts_with("canary canary-0 "));
        for line in &lines {
            let path = line.rsplit(' ').next().expect("path");
            let path = Path::new(path);
            assert!(path.is_file());
            assert_eq!(
                fs::metadata(path).expect("key exists").mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn shared_keys_reuse_one_public_key() {
        let mut fixture = fixture();
        generate(&mut fixture, true).expect("keys generate");
        let keys: Vec<&str> = fixture
            .snapshot
            .actor_keys
            .iter()
            .map(|row| row.public_key.as_str())
            .collect();
        assert_eq!(keys.len(), fixture.entries.len());
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn distinct_keys_differ_per_actor() {
        let mut fixture = fixture();
        generate(&mut fixture, false).expect("keys generate");
        let keys: Vec<&str> = fixture
            .snapshot
            .actor_keys
            .iter()
            .map(|row| row.public_key.as_str())
            .collect();
        assert_eq!(keys.len(), fixture.entries.len());
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn actor_key_types_match_the_owner() {
        let mut fixture = fixture();
        generate(&mut fixture, true).expect("keys generate");
        for row in &fixture.snapshot.actor_keys {
            let entry = fixture
                .entries
                .iter()
                .find(|entry| entry.actor_id == row.actor_id)
                .expect("entry exists");
            let expected = if entry.kind == KeyKind::Superuser {
                "user"
            } else {
                "client"
            };
            assert_eq!(row.actor_type, expected);
        }
    }

    #[test]
    fn a_written_manifest_reads_back_with_its_keys() {
        let mut fixture = fixture();
        let actors = generate(&mut fixture, true).expect("keys generate");
        let directory = tempfile::tempdir().expect("tempdir");
        let manifest = directory.path().join("load.keys");
        write_manifest(&manifest, &actors).expect("manifest writes");

        let read = read_manifest(&manifest).expect("manifest reads");
        let expected: Vec<(KeyKind, &str)> = actors
            .iter()
            .map(|actor| (actor.entry.kind, actor.entry.name.as_str()))
            .collect();
        let actual: Vec<(KeyKind, &str)> = read
            .iter()
            .map(|entry| (entry.kind, entry.name.as_str()))
            .collect();
        assert_eq!(actual, expected);
        for (entry, actor) in read.iter().zip(&actors) {
            assert_eq!(entry.private_key, actor.private_key_pem);
        }
    }

    #[test]
    fn a_malformed_manifest_names_the_line() {
        let error = parse_manifest("superuser load-superuser\n").expect_err("two fields");
        assert!(error.to_string().contains("line 1"), "{error}");
        let error = parse_manifest("bogus name /ignored.pem").expect_err("unknown kind");
        assert!(error.to_string().contains("unknown kind"), "{error}");
    }
}
