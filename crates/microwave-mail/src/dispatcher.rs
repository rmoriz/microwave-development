use std::sync::Arc;
use tokio::sync::mpsc;

use crate::{MailError, MailMessage, Mailer};

pub const MAIL_QUEUE_CAPACITY: usize = 64;

pub struct MailDispatcher {
    sender: mpsc::Sender<MailMessage>,
    _worker: tokio::task::JoinHandle<()>,
}

impl MailDispatcher {
    pub fn new(inner: Arc<dyn Mailer>, capacity: usize) -> Result<Arc<Self>, MailError> {
        if capacity == 0 {
            return Err(MailError::Configuration(
                "MailDispatcher capacity must be greater than zero".into(),
            ));
        }

        // Ensure we're in a Tokio runtime
        let _handle = tokio::runtime::Handle::try_current()
            .map_err(|_| MailError::Configuration("mail requires a Tokio runtime".into()))?;

        let (sender, receiver) = mpsc::channel::<MailMessage>(capacity);

        let worker = tokio::spawn(async move {
            Self::worker_loop(inner, receiver).await;
        });

        Ok(Arc::new(Self {
            sender,
            _worker: worker,
        }))
    }

    async fn worker_loop(inner: Arc<dyn Mailer>, mut receiver: mpsc::Receiver<MailMessage>) {
        while let Some(message) = receiver.recv().await {
            // Inner mailers are responsible for logging delivery failures with
            // redacted metadata. The dispatcher only owns queue lifecycle.
            let _ = inner.send(&message).await;
        }
    }
}

#[async_trait::async_trait]
impl Mailer for MailDispatcher {
    fn is_enabled(&self) -> bool {
        true
    }

    async fn send(&self, message: &MailMessage) -> Result<(), MailError> {
        self.sender.try_send(message.clone()).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => MailError::QueueFull,
            mpsc::error::TrySendError::Closed(_) => MailError::QueueClosed,
        })
    }
}
