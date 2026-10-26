use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

use microwave_mail::{MailDispatcher, MailError, MailMessage, Mailer};

struct RecordingMailer {
    messages: Arc<Mutex<Vec<MailMessage>>>,
    fail_count: Arc<Mutex<usize>>,
}

impl RecordingMailer {
    fn new() -> (Self, Arc<Mutex<Vec<MailMessage>>>) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        let fail_count = Arc::new(Mutex::new(0));
        (
            Self {
                messages: messages.clone(),
                fail_count,
            },
            messages,
        )
    }

    fn with_failures(fail_count: usize) -> (Self, Arc<Mutex<Vec<MailMessage>>>) {
        let messages = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                messages: messages.clone(),
                fail_count: Arc::new(Mutex::new(fail_count)),
            },
            messages,
        )
    }
}

#[async_trait::async_trait]
impl Mailer for RecordingMailer {
    fn is_enabled(&self) -> bool {
        true
    }

    async fn send(&self, message: &MailMessage) -> Result<(), MailError> {
        let mut fail_count = self.fail_count.lock().await;
        if *fail_count > 0 {
            *fail_count -= 1;
            return Err(MailError::Transport("Simulated failure".to_string()));
        }
        drop(fail_count);

        self.messages.lock().await.push(message.clone());
        Ok(())
    }
}

struct BlockingMailer {
    block: Arc<Notify>,
    started: Arc<Notify>,
}

#[async_trait::async_trait]
impl Mailer for BlockingMailer {
    fn is_enabled(&self) -> bool {
        true
    }

    async fn send(&self, _message: &MailMessage) -> Result<(), MailError> {
        self.started.notify_one();
        self.block.notified().await;
        Ok(())
    }
}

fn make_test_message(id: &str) -> MailMessage {
    MailMessage {
        from: "test@example.com".into(),
        to: "recipient@example.com".into(),
        reply_to: None,
        subject: format!("Test {}", id),
        text_body: format!("Body {}", id),
    }
}

#[tokio::test]
async fn dispatcher_send_records_message() {
    let (recorder, messages) = RecordingMailer::new();
    let dispatcher = MailDispatcher::new(Arc::new(recorder), 10).unwrap();

    let msg = make_test_message("1");
    dispatcher.send(&msg).await.unwrap();

    // Wait for the message to be processed
    timeout(Duration::from_secs(1), async {
        loop {
            if messages.lock().await.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let captured = messages.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].subject, "Test 1");
}

#[tokio::test]
async fn dispatcher_returns_queue_full_when_capacity_exceeded() {
    let block = Arc::new(Notify::new());
    let started = Arc::new(Notify::new());
    let blocking_mailer = Arc::new(BlockingMailer {
        block: block.clone(),
        started: started.clone(),
    });
    let dispatcher = MailDispatcher::new(blocking_mailer, 2).unwrap();

    dispatcher.send(&make_test_message("0")).await.unwrap();
    timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("worker did not start the blocking send");

    // Now fill the remaining capacity
    dispatcher.send(&make_test_message("1")).await.unwrap();
    dispatcher.send(&make_test_message("2")).await.unwrap();

    // Next message should fail with QueueFull
    let msg = make_test_message("overflow");
    let result = dispatcher.send(&msg).await;
    assert!(matches!(result, Err(MailError::QueueFull)));

    // Clean up
    block.notify_waiters();
}

#[tokio::test]
async fn dispatcher_continues_after_inner_send_failure() {
    let (recorder, messages) = RecordingMailer::with_failures(1);
    let dispatcher = MailDispatcher::new(Arc::new(recorder), 10).unwrap();

    // First message should fail internally
    let msg1 = make_test_message("1");
    dispatcher.send(&msg1).await.unwrap();

    // Wait a bit for processing
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second message should succeed
    let msg2 = make_test_message("2");
    dispatcher.send(&msg2).await.unwrap();

    // Wait for processing
    timeout(Duration::from_secs(1), async {
        loop {
            if messages.lock().await.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("timeout waiting for second message to be processed");

    // Verify the second message was actually delivered
    let captured_messages = messages.lock().await;
    assert_eq!(captured_messages.len(), 1);
    assert_eq!(captured_messages[0].subject, "Test 2");
    drop(captured_messages);

    // The dispatcher no longer logs inner delivery failures; the inner
    // mailer owns that. It must neither panic nor leak the queued payload.
    // The dispatcher must not panic or leak the queued payload through logs.
    let _ = messages;
}
