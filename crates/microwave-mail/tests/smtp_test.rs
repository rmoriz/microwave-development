use base64::{engine::general_purpose, Engine as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use microwave_config::{MailAuth, MailConfig, MailSecurity};
use microwave_mail::{MailMessage, Mailer, SmtpMailer};

struct CapturedEmail {
    from: String,
    to: String,
    body: String,
}

async fn start_mock_smtp_server(
    expected_username: &str,
    expected_password: &str,
    should_fail_auth: bool,
) -> (SocketAddr, Arc<Mutex<Vec<CapturedEmail>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let emails = Arc::new(Mutex::new(Vec::new()));
    let emails_clone = emails.clone();
    let expected_user = expected_username.to_string();
    let expected_pass = expected_password.to_string();

    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let (reader, mut writer) = socket.split();
            let mut reader = BufReader::new(reader);

            // Send greeting
            writer
                .write_all(b"220 mock.smtp.server ESMTP\r\n")
                .await
                .unwrap();

            let mut in_data_mode = false;
            let mut current_email = CapturedEmail {
                from: String::new(),
                to: String::new(),
                body: String::new(),
            };
            let mut auth_stage = 0;

            let mut line = String::new();
            while reader.read_line(&mut line).await.unwrap() > 0 {
                let line_trimmed = line.trim();

                if in_data_mode {
                    if line_trimmed == "." {
                        in_data_mode = false;
                        writer.write_all(b"250 OK\r\n").await.unwrap();
                        emails_clone.lock().await.push(current_email);
                        current_email = CapturedEmail {
                            from: String::new(),
                            to: String::new(),
                            body: String::new(),
                        };
                    } else {
                        current_email.body.push_str(&line);
                    }
                } else if line_trimmed.starts_with("EHLO") || line_trimmed.starts_with("HELO") {
                    writer
                        .write_all(b"250-mock.smtp.server\r\n250-AUTH PLAIN LOGIN\r\n250 OK\r\n")
                        .await
                        .unwrap();
                } else if line_trimmed.starts_with("AUTH PLAIN") {
                    if should_fail_auth {
                        writer
                            .write_all(b"535 Authentication failed\r\n")
                            .await
                            .unwrap();
                    } else {
                        // Decode base64 (format: \0username\0password)
                        let encoded = &line_trimmed[11..];
                        if let Ok(decoded) = general_purpose::STANDARD.decode(encoded) {
                            let decoded_str = String::from_utf8_lossy(&decoded);
                            let parts: Vec<&str> = decoded_str.split('\0').collect();
                            if parts.len() == 3
                                && parts[1] == expected_user
                                && parts[2] == expected_pass
                            {
                                writer
                                    .write_all(b"235 Authentication successful\r\n")
                                    .await
                                    .unwrap();
                            } else {
                                writer
                                    .write_all(b"535 Authentication failed\r\n")
                                    .await
                                    .unwrap();
                            }
                        } else {
                            writer
                                .write_all(b"535 Authentication failed\r\n")
                                .await
                                .unwrap();
                        }
                    }
                } else if line_trimmed == "AUTH LOGIN" {
                    auth_stage = 1;
                    writer.write_all(b"334 VXNlcm5hbWU6\r\n").await.unwrap();
                } else if auth_stage == 1 {
                    // Username
                    if let Ok(decoded) = general_purpose::STANDARD.decode(line_trimmed) {
                        let username = String::from_utf8_lossy(&decoded);
                        if username == expected_user {
                            auth_stage = 2;
                            writer.write_all(b"334 UGFzc3dvcmQ6\r\n").await.unwrap();
                        } else {
                            writer
                                .write_all(b"535 Authentication failed\r\n")
                                .await
                                .unwrap();
                            auth_stage = 0;
                        }
                    }
                } else if auth_stage == 2 {
                    // Password
                    if should_fail_auth {
                        writer
                            .write_all(b"535 Authentication failed\r\n")
                            .await
                            .unwrap();
                    } else if let Ok(decoded) = general_purpose::STANDARD.decode(line_trimmed) {
                        let password = String::from_utf8_lossy(&decoded);
                        if password == expected_pass {
                            writer
                                .write_all(b"235 Authentication successful\r\n")
                                .await
                                .unwrap();
                        } else {
                            writer
                                .write_all(b"535 Authentication failed\r\n")
                                .await
                                .unwrap();
                        }
                    }
                    auth_stage = 0;
                } else if let Some(rest) = line_trimmed.strip_prefix("MAIL FROM:") {
                    current_email.from = rest.trim().to_string();
                    writer.write_all(b"250 OK\r\n").await.unwrap();
                } else if let Some(rest) = line_trimmed.strip_prefix("RCPT TO:") {
                    current_email.to = rest.trim().to_string();
                    writer.write_all(b"250 OK\r\n").await.unwrap();
                } else if line_trimmed == "DATA" {
                    in_data_mode = true;
                    writer
                        .write_all(b"354 Start mail input; end with <CRLF>.<CRLF>\r\n")
                        .await
                        .unwrap();
                } else if line_trimmed == "QUIT" {
                    writer.write_all(b"221 Bye\r\n").await.unwrap();
                    break;
                }

                line.clear();
            }
        }
    });

    (addr, emails)
}

#[tokio::test]
async fn sends_with_auth_plain_and_captures_the_reset_message() {
    let (addr, emails) = start_mock_smtp_server("testuser", "testpass", false).await;

    let config = MailConfig {
        enabled: true,
        host: Some("127.0.0.1".to_string()),
        port: Some(addr.port()),
        security: MailSecurity::None,
        username: Some("testuser".to_string()),
        password: Some("testpass".to_string()),
        auth: MailAuth::Plain,
        from: Some("Chef <chef@example.com>".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let mailer = SmtpMailer::from_config(&config).unwrap();
    let message = MailMessage {
        from: "Chef <chef@example.com>".into(),
        to: "alice@example.com".into(),
        reply_to: None,
        subject: "Reset your Chef Infra Server password".into(),
        text_body: "https://chef.example.com/id/password-reset?token=abc".into(),
    };

    mailer.send(&message).await.unwrap();

    // Give the server a moment to process
    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured = emails.lock().await;
    assert_eq!(captured.len(), 1);
    assert!(captured[0].from.contains("chef@example.com"));
    assert!(captured[0].to.contains("alice@example.com"));
    assert!(captured[0]
        .body
        .contains("Reset your Chef Infra Server password"));
    assert!(captured[0]
        .body
        .contains("https://chef.example.com/id/password-reset?token=abc"));
}

#[tokio::test]
async fn sends_with_auth_login() {
    let (addr, emails) = start_mock_smtp_server("loginuser", "loginpass", false).await;

    let config = MailConfig {
        enabled: true,
        host: Some("127.0.0.1".to_string()),
        port: Some(addr.port()),
        security: MailSecurity::None,
        username: Some("loginuser".to_string()),
        password: Some("loginpass".to_string()),
        auth: MailAuth::Login,
        from: Some("Chef <chef@example.com>".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let mailer = SmtpMailer::from_config(&config).unwrap();
    let message = MailMessage {
        from: "Chef <chef@example.com>".into(),
        to: "bob@example.com".into(),
        reply_to: None,
        subject: "Test with AUTH LOGIN".into(),
        text_body: "Test body".into(),
    };

    mailer.send(&message).await.unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured = emails.lock().await;
    assert_eq!(captured.len(), 1);
    assert!(captured[0].to.contains("bob@example.com"));
}

#[tokio::test]
async fn wrong_credentials_return_a_transport_error() {
    let (addr, _) = start_mock_smtp_server("rightuser", "rightpass", true).await;

    let config = MailConfig {
        enabled: true,
        host: Some("127.0.0.1".to_string()),
        port: Some(addr.port()),
        security: MailSecurity::None,
        username: Some("wronguser".to_string()),
        password: Some("wrongpass".to_string()),
        auth: MailAuth::Plain,
        from: Some("Chef <chef@example.com>".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let mailer = SmtpMailer::from_config(&config).unwrap();
    let message = MailMessage {
        from: "Chef <chef@example.com>".into(),
        to: "charlie@example.com".into(),
        reply_to: None,
        subject: "Test".into(),
        text_body: "Test".into(),
    };

    let result = mailer.send(&message).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn builder_only_starttls() {
    let config = MailConfig {
        enabled: true,
        host: Some("smtp.example.com".to_string()),
        port: Some(587),
        security: MailSecurity::Starttls,
        username: None,
        password: None,
        auth: MailAuth::None,
        from: Some("test@example.com".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let result = SmtpMailer::from_config(&config);
    assert!(result.is_ok());
}

#[tokio::test]
async fn builder_only_tls() {
    let config = MailConfig {
        enabled: true,
        host: Some("smtp.example.com".to_string()),
        port: Some(465),
        security: MailSecurity::Tls,
        username: None,
        password: None,
        auth: MailAuth::None,
        from: Some("test@example.com".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let result = SmtpMailer::from_config(&config);
    assert!(result.is_ok());
}

#[tokio::test]
async fn builder_only_none() {
    let config = MailConfig {
        enabled: true,
        host: Some("127.0.0.1".to_string()),
        port: Some(25),
        security: MailSecurity::None,
        username: None,
        password: None,
        auth: MailAuth::None,
        from: Some("test@example.com".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: None,
    };

    let result = SmtpMailer::from_config(&config);
    assert!(result.is_ok());
}

#[tokio::test]
async fn private_ca_test() {
    use rcgen::{CertificateParams, KeyPair};
    use std::fs;
    use tempfile::NamedTempFile;

    // Generate a self-signed certificate
    let key_pair = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Test CA");
    let cert = params.self_signed(&key_pair).unwrap();
    let cert_pem = cert.pem();

    let temp_file = NamedTempFile::new().unwrap();
    fs::write(temp_file.path(), cert_pem).unwrap();

    let config = MailConfig {
        enabled: true,
        host: Some("localhost".to_string()),
        port: Some(465),
        security: MailSecurity::Tls,
        username: None,
        password: None,
        auth: MailAuth::None,
        from: Some("test@example.com".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: Some(temp_file.path().to_string_lossy().to_string()),
    };

    let result = SmtpMailer::from_config(&config);
    assert!(result.is_ok());
}

#[tokio::test]
async fn missing_ca_path_returns_configuration_error() {
    let config = MailConfig {
        enabled: true,
        host: Some("localhost".to_string()),
        port: Some(465),
        security: MailSecurity::Tls,
        username: None,
        password: None,
        auth: MailAuth::None,
        from: Some("test@example.com".to_string()),
        reply_to: None,
        timeout_ms: 5000,
        hello_name: None,
        tls_ca_path: Some("/nonexistent/path/to/ca.pem".to_string()),
    };

    let result = SmtpMailer::from_config(&config);
    assert!(result.is_err());
    if let Err(e) = result {
        assert!(matches!(e, microwave_mail::MailError::Configuration(_)));
    }
}
