//! Microwave Mail - SMTP transport and bounded dispatcher

pub mod dispatcher;
pub mod smtp;

use std::sync::Arc;

pub use dispatcher::{MailDispatcher, MAIL_QUEUE_CAPACITY};
pub use smtp::SmtpMailer;

use microwave_config::MailConfig;

/// Error type for mail operations
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("Configuration error: {0}")]
    Configuration(String),
    #[error("Address error: {0}")]
    Address(String),
    #[error("Transport error: {0}")]
    Transport(String),
    #[error("Queue full")]
    QueueFull,
    #[error("Queue closed")]
    QueueClosed,
    #[error("Mail disabled")]
    Disabled,
}

/// Mail message to be sent
#[derive(Clone)]
pub struct MailMessage {
    pub from: String,
    pub to: String,
    pub reply_to: Option<String>,
    pub subject: String,
    pub text_body: String,
}

/// Trait for mailers
#[async_trait::async_trait]
pub trait Mailer: Send + Sync {
    fn is_enabled(&self) -> bool;
    async fn send(&self, message: &MailMessage) -> Result<(), MailError>;
}

/// Disabled mailer that always returns MailError::Disabled
pub struct DisabledMailer;

#[async_trait::async_trait]
impl Mailer for DisabledMailer {
    fn is_enabled(&self) -> bool {
        false
    }

    async fn send(&self, _message: &MailMessage) -> Result<(), MailError> {
        Err(MailError::Disabled)
    }
}

/// Build a mailer from configuration.
/// Returns a DisabledMailer when config.is_enabled() is false,
/// otherwise returns a MailDispatcher wrapping an SmtpMailer.
pub fn build_mailer(config: &MailConfig) -> Result<Arc<dyn Mailer>, MailError> {
    if !config.is_enabled() {
        return Ok(Arc::new(DisabledMailer));
    }

    let smtp_mailer = SmtpMailer::from_config(config)?;
    MailDispatcher::new(Arc::new(smtp_mailer), MAIL_QUEUE_CAPACITY)
        .map(|dispatcher| dispatcher as Arc<dyn Mailer>)
}
