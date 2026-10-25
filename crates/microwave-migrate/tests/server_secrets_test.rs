use flate2::write::GzEncoder;
use flate2::Compression as GzipCompression;
use microwave_migrate::{apply_server_secrets, prepare_server_secrets, ServerSecretsOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tar::{Builder, EntryType, Header};

fn write(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn make_options(source: &Path, target: &Path) -> ServerSecretsOptions {
    ServerSecretsOptions {
        source_dir: Some(source.to_path_buf()),
        source_archive: None,
        target_data_dir: target.to_path_buf(),
        tls_cert_target: None,
        tls_key_target: None,
        force: false,
    }
}

fn secret_directory(root: &Path) -> PathBuf {
    let source = root.join("secrets");
    write(&source.join("pivotal.pem"), b"PIVOTAL PRIVATE\n");
    write(&source.join("pivotal.password"), b"PIVOTAL PASSWORD\n");
    write(&source.join("webui_priv.pem"), b"WEBUI PRIVATE\n");
    write(&source.join("webui_pub.pem"), b"WEBUI PUBLIC\n");
    write(&source.join("objectstore.secret"), b"OBJECTSTORE SECRET\n");
    write(
        &source.join("private-chef-secrets.json"),
        br#"{"opscode_erchef":{"sql_password":"secret"}}"#,
    );
    write(&source.join("ssl/cert.pem"), b"TLS CERT\n");
    write(&source.join("ssl/key.pem"), b"TLS KEY\n");
    write(&source.join("ca/root.pem"), b"CA ROOT\n");
    write(
        &source.join("nginx/ca/intermediate.pem"),
        b"CA INTERMEDIATE\n",
    );
    source
}

fn header(size: u64, mode: u32, entry_type: EntryType) -> Header {
    let mut header = Header::new_gnu();
    header.set_size(size);
    header.set_mode(mode);
    header.set_entry_type(entry_type);
    header
}

fn append_file<W: Write>(builder: &mut Builder<W>, name: &str, data: &[u8]) {
    let mut header = header(data.len() as u64, 0o600, EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, data).unwrap();
}

fn gzip_archive(path: &Path, files: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let encoder = GzEncoder::new(file, GzipCompression::default());
    let mut builder = Builder::new(encoder);
    for (name, data) in files {
        append_file(&mut builder, name, data);
    }
    let encoder = builder.into_inner().unwrap();
    encoder.finish().unwrap();
}

fn plain_archive(path: &Path, files: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut builder = Builder::new(file);
    for (name, data) in files {
        append_file(&mut builder, name, data);
    }
    builder.finish().unwrap();
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn imports_a_secrets_directory_with_modes_and_default_targets() {
    let root = tempfile::tempdir().unwrap();
    let source = secret_directory(root.path());
    let target = root.path().join("data");

    let plan = prepare_server_secrets(&make_options(&source, &target)).unwrap();
    let debug = format!("{plan:?}");
    assert!(!debug.contains("PIVOTAL PRIVATE"));
    assert!(!debug.contains("TLS KEY"));
    assert_eq!(plan.writes.len(), 10);
    assert_eq!(plan.private_count(), 6);
    assert_eq!(plan.public_count(), 4);
    assert_eq!(apply_server_secrets(&plan).unwrap(), 10);

    for (relative, contents) in [
        ("pivotal.pem", b"PIVOTAL PRIVATE\n".as_slice()),
        ("pivotal.password", b"PIVOTAL PASSWORD\n"),
        ("webui_priv.pem", b"WEBUI PRIVATE\n"),
        ("webui_pub.pem", b"WEBUI PUBLIC\n"),
        ("objectstore.secret", b"OBJECTSTORE SECRET\n"),
        (
            "private-chef-secrets.json",
            br#"{"opscode_erchef":{"sql_password":"secret"}}"#,
        ),
        ("ssl/cert.pem", b"TLS CERT\n"),
        ("ssl/key.pem", b"TLS KEY\n"),
        ("ca/root.pem", b"CA ROOT\n"),
        ("nginx/ca/intermediate.pem", b"CA INTERMEDIATE\n"),
    ] {
        assert_eq!(
            std::fs::read(target.join(relative)).unwrap(),
            contents,
            "{relative}"
        );
    }

    #[cfg(unix)]
    {
        for relative in [
            "pivotal.pem",
            "pivotal.password",
            "webui_priv.pem",
            "objectstore.secret",
            "private-chef-secrets.json",
            "ssl/key.pem",
        ] {
            assert_eq!(mode(&target.join(relative)), 0o600, "{relative}");
        }
        for relative in [
            "webui_pub.pem",
            "ssl/cert.pem",
            "ca/root.pem",
            "nginx/ca/intermediate.pem",
        ] {
            assert_eq!(mode(&target.join(relative)), 0o644, "{relative}");
        }
        for relative in ["", "ssl", "ca", "nginx", "nginx/ca"] {
            assert_eq!(mode(&target.join(relative)), 0o700, "{relative:?}");
        }
    }
}

#[test]
fn imports_a_gzip_tar_archive_and_uses_root_tls_candidates() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("secrets.tar.gz");
    gzip_archive(
        &archive,
        &[
            ("pivotal.pem", b"ARCHIVE PIVOTAL\n"),
            ("webui_pub.pem", b"ARCHIVE PUBLIC\n"),
            ("cert.pem", b"ROOT CERT\n"),
            ("key.pem", b"ROOT KEY\n"),
            ("ca/root.pem", b"ARCHIVE CA\n"),
            ("nginx/ca/intermediate.pem", b"ARCHIVE NGINX CA\n"),
        ],
    );
    let target = root.path().join("data");
    let mut options = make_options(&archive, &target);
    options.source_dir = None;
    options.source_archive = Some(archive);
    let plan = prepare_server_secrets(&options).unwrap();
    assert_eq!(plan.writes.len(), 6);
    apply_server_secrets(&plan).unwrap();

    assert_eq!(
        std::fs::read(target.join("pivotal.pem")).unwrap(),
        b"ARCHIVE PIVOTAL\n"
    );
    assert_eq!(
        std::fs::read(target.join("cert.pem")).unwrap(),
        b"ROOT CERT\n"
    );
    assert_eq!(
        std::fs::read(target.join("key.pem")).unwrap(),
        b"ROOT KEY\n"
    );
    assert_eq!(
        std::fs::read(target.join("ca/root.pem")).unwrap(),
        b"ARCHIVE CA\n"
    );
    assert_eq!(
        std::fs::read(target.join("nginx/ca/intermediate.pem")).unwrap(),
        b"ARCHIVE NGINX CA\n"
    );
}

#[test]
fn accepts_the_underscore_private_chef_secrets_name() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    write(
        &source.join("private_chef_secrets.json"),
        br#"{"secret":"underscore"}"#,
    );
    let target = root.path().join("data");
    let plan = prepare_server_secrets(&make_options(&source, &target)).unwrap();
    apply_server_secrets(&plan).unwrap();
    assert_eq!(
        std::fs::read(target.join("private-chef-secrets.json")).unwrap(),
        br#"{"secret":"underscore"}"#
    );
}

#[test]
fn refuses_to_overwrite_without_force_and_replaces_with_it() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    write(&source.join("pivotal.pem"), b"new private key\n");
    let target = root.path().join("data");
    write(&target.join("pivotal.pem"), b"old private key\n");

    let error = prepare_server_secrets(&make_options(&source, &target))
        .unwrap_err()
        .to_string();
    assert!(error.contains("already exist"), "{error}");
    assert!(error.contains("pivotal.pem"), "{error}");
    assert!(!error.contains("old private key"));
    assert_eq!(
        std::fs::read(target.join("pivotal.pem")).unwrap(),
        b"old private key\n"
    );

    let mut forced = make_options(&source, &target);
    forced.force = true;
    let plan = prepare_server_secrets(&forced).unwrap();
    apply_server_secrets(&plan).unwrap();
    assert_eq!(
        std::fs::read(target.join("pivotal.pem")).unwrap(),
        b"new private key\n"
    );
    #[cfg(unix)]
    assert_eq!(mode(&target.join("pivotal.pem")), 0o600);
}

#[test]
fn explicit_tls_targets_are_used() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    write(&source.join("cert.pem"), b"cert\n");
    write(&source.join("key.pem"), b"key\n");
    let target = root.path().join("data");
    let cert_target = root.path().join("config/tls/server.crt");
    let key_target = root.path().join("config/tls/server.key");
    let mut options = make_options(&source, &target);
    options.tls_cert_target = Some(cert_target.clone());
    options.tls_key_target = Some(key_target.clone());
    let plan = prepare_server_secrets(&options).unwrap();
    apply_server_secrets(&plan).unwrap();
    assert_eq!(std::fs::read(&cert_target).unwrap(), b"cert\n");
    assert_eq!(std::fs::read(&key_target).unwrap(), b"key\n");
    #[cfg(unix)]
    {
        assert_eq!(mode(&cert_target), 0o644);
        assert_eq!(mode(&key_target), 0o600);
        assert_eq!(mode(cert_target.parent().unwrap()), 0o700);
    }
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_secret_target_even_with_force() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    write(&source.join("pivotal.pem"), b"new\n");
    let target = root.path().join("data");
    std::fs::create_dir_all(&target).unwrap();
    let outside = root.path().join("outside.pem");
    std::fs::write(&outside, b"outside\n").unwrap();
    symlink(&outside, target.join("pivotal.pem")).unwrap();
    let mut options = make_options(&source, &target);
    options.force = true;
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("symlink"), "{error}");
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside\n");
}

#[cfg(unix)]
#[test]
fn rejects_a_symlinked_secret_source() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    std::fs::create_dir_all(&source).unwrap();
    let real = source.join("real.pem");
    std::fs::write(&real, b"private\n").unwrap();
    symlink(&real, source.join("pivotal.pem")).unwrap();
    let error = prepare_server_secrets(&make_options(&source, &root.path().join("data")))
        .unwrap_err()
        .to_string();
    assert!(error.contains("symlink"), "{error}");
}

#[test]
fn rejects_archive_links_and_unknown_entries() {
    let root = tempfile::tempdir().unwrap();
    let archive = root.path().join("secrets.tar");
    let file = std::fs::File::create(&archive).unwrap();
    let mut builder = Builder::new(file);
    let mut symlink = header(0, 0o777, EntryType::Symlink);
    symlink.set_link_name("/etc/passwd").unwrap();
    symlink.set_cksum();
    builder
        .append_data(&mut symlink, "pivotal.pem", &[][..])
        .unwrap();
    builder.finish().unwrap();

    let mut options = make_options(&archive, &root.path().join("data"));
    options.source_dir = None;
    options.source_archive = Some(archive);
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("link"), "{error}");

    let fifo = root.path().join("fifo.tar");
    let file = std::fs::File::create(&fifo).unwrap();
    let mut builder = Builder::new(file);
    let mut fifo_header = header(0, 0o644, EntryType::Fifo);
    fifo_header.set_cksum();
    builder
        .append_data(&mut fifo_header, "pivotal.pem", &[][..])
        .unwrap();
    builder.finish().unwrap();
    let mut options = make_options(&fifo, &root.path().join("data"));
    options.source_dir = None;
    options.source_archive = Some(fifo);
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("FIFO"), "{error}");

    let hardlink = root.path().join("hardlink.tar");
    let file = std::fs::File::create(&hardlink).unwrap();
    let mut builder = Builder::new(file);
    let mut link_header = header(0, 0o600, EntryType::Link);
    link_header.set_link_name("pivotal.pem").unwrap();
    link_header.set_cksum();
    builder
        .append_data(&mut link_header, "hardlink.pem", &[][..])
        .unwrap();
    builder.finish().unwrap();
    let mut options = make_options(&hardlink, &root.path().join("data"));
    options.source_dir = None;
    options.source_archive = Some(hardlink);
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("link"), "{error}");

    let unknown = root.path().join("unknown.tar");
    plain_archive(&unknown, &[("README.txt", b"not a secret\n")]);
    let mut options = make_options(&unknown, &root.path().join("data"));
    options.source_dir = None;
    options.source_archive = Some(unknown);
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("outside the allowlist"), "{error}");
}

#[test]
fn refuses_both_sources() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("secrets");
    std::fs::create_dir_all(&source).unwrap();
    let mut options = make_options(&source, &root.path().join("data"));
    options.source_archive = Some(root.path().join("also.tar"));
    let error = prepare_server_secrets(&options).unwrap_err().to_string();
    assert!(error.contains("either"), "{error}");
}
