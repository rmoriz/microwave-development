use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use microwave_config::{MailAuth, MailConfig, MailSecurity};
use std::time::Duration;

use crate::{MailError, MailMessage, Mailer};

pub struct SmtpMailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    enabled: bool,
}

impl SmtpMailer {
    pub fn from_config(config: &MailConfig) -> Result<Self, MailError> {
        if !config.is_enabled() {
            return Err(MailError::Configuration("Mail is disabled".into()));
        }

        let host = config
            .host
            .as_deref()
            .ok_or_else(|| MailError::Configuration("mail.host is required".into()))?;

        // Determine port based on security if not specified
        let port = config.port.unwrap_or(match config.security {
            MailSecurity::Starttls => 587,
            MailSecurity::Tls => 465,
            MailSecurity::None => 25,
        });

        // Build TLS parameters based on security mode
        let builder = match config.security {
            MailSecurity::Starttls => {
                let tls_params = Self::build_tls_parameters(host, config.tls_ca_path.as_deref())?;
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                    .port(port)
                    .timeout(Some(Duration::from_millis(config.timeout_ms)))
                    .tls(Tls::Required(tls_params))
            }
            MailSecurity::Tls => {
                let tls_params = Self::build_tls_parameters(host, config.tls_ca_path.as_deref())?;
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                    .port(port)
                    .timeout(Some(Duration::from_millis(config.timeout_ms)))
                    .tls(Tls::Wrapper(tls_params))
            }
            MailSecurity::None => {
                // Security check: reject credentials with security=None for non-loopback hosts
                if config.username.is_some() && !Self::is_loopback_host(host) {
                    return Err(MailError::Configuration(
                        "Credentials with security=none are only allowed for loopback hosts".into(),
                    ));
                }
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
                    .port(port)
                    .timeout(Some(Duration::from_millis(config.timeout_ms)))
                    .tls(Tls::None)
            }
        };

        // Set HELLO name if configured
        let builder = if let Some(ref hello_name) = config.hello_name {
            builder.hello_name(ClientId::Domain(hello_name.clone()))
        } else {
            builder
        };

        // Configure authentication
        let has_credentials = config.username.is_some() && config.password.is_some();
        let builder = if has_credentials {
            let username = config.username.as_ref().unwrap();
            let password = config.password.as_ref().unwrap();
            let credentials = Credentials::new(username.clone(), password.clone());
            let builder = builder.credentials(credentials);

            // Set authentication mechanism
            match config.auth {
                MailAuth::Plain => builder.authentication(vec![Mechanism::Plain]),
                MailAuth::Login => builder.authentication(vec![Mechanism::Login]),
                MailAuth::Auto => builder, // Let the transport auto-negotiate
                MailAuth::None => {
                    return Err(MailError::Configuration(
                        "auth=none cannot be combined with credentials".into(),
                    ));
                }
            }
        } else if config.auth == MailAuth::None {
            // Explicitly disable authentication
            builder.authentication(vec![])
        } else {
            builder
        };

        let transport = builder.build();

        Ok(Self {
            transport,
            enabled: true,
        })
    }

    fn build_tls_parameters(host: &str, ca_path: Option<&str>) -> Result<TlsParameters, MailError> {
        let mut tls_builder = TlsParameters::builder(host.to_string());

        if let Some(ca_file_path) = ca_path {
            // Read custom CA certificate
            let ca_pem = std::fs::read(ca_file_path).map_err(|e| {
                MailError::Configuration(format!("Failed to read CA file {}: {}", ca_file_path, e))
            })?;

            let certificate = lettre::transport::smtp::client::Certificate::from_pem(&ca_pem)
                .map_err(|e| {
                    MailError::Configuration(format!("Failed to parse CA certificate: {}", e))
                })?;

            tls_builder = tls_builder.add_root_certificate(certificate);
        }

        tls_builder
            .build()
            .map_err(|e| MailError::Configuration(format!("Failed to build TLS parameters: {}", e)))
    }

    fn is_loopback_host(host: &str) -> bool {
        host == "localhost" || host == "127.0.0.1" || host == "::1"
    }
}

#[async_trait::async_trait]
impl Mailer for SmtpMailer {
    fn is_enabled(&self) -> bool {
        self.enabled
    }

    async fn send(&self, message: &MailMessage) -> Result<(), MailError> {
        use lettre::message::header::ContentType;
        use lettre::message::Mailbox;
        use lettre::message::SinglePart;
        use lettre::Message;

        // Parse from address
        let from: Mailbox = message.from.parse().map_err(|e| {
            MailError::Address(format!("Invalid from address '{}': {}", message.from, e))
        })?;

        // Parse to address
        let to: Mailbox = message.to.parse().map_err(|e| {
            MailError::Address(format!("Invalid to address '{}': {}", message.to, e))
        })?;

        // Build the message
        let mut email_builder = Message::builder()
            .from(from)
            .to(to)
            .subject(message.subject.clone());

        // Add reply-to if present
        if let Some(ref reply_to) = message.reply_to {
            let reply_to_mailbox: Mailbox = reply_to.parse().map_err(|e| {
                MailError::Address(format!("Invalid reply-to address '{}': {}", reply_to, e))
            })?;
            email_builder = email_builder.reply_to(reply_to_mailbox);
        }

        // Build the email with plain text body
        let email = email_builder
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(message.text_body.clone()),
            )
            .map_err(|e| MailError::Transport(format!("Failed to build email: {}", e)))?;

        // Send the email
        self.transport
            .send(email)
            .await
            .map_err(|e| MailError::Transport(format!("SMTP error: {}", e)))?;

        Ok(())
    }
}
