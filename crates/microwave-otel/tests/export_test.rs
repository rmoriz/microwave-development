use microwave_config::OtelConfig;
use microwave_metrics::Metrics;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

fn stub() -> (String, std::sync::mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 4096];
        let read = stream.read(&mut buffer).unwrap();
        let head = String::from_utf8_lossy(&buffer[..read]).to_string();
        let line = head.lines().next().unwrap_or_default().to_string();
        let _ = tx.send(line);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/x-protobuf\r\ncontent-length: 0\r\n\r\n")
            .unwrap();
    });
    (format!("http://{address}"), rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn reaches_a_stub_collector() {
    let (endpoint, received) = stub();
    let metrics = Arc::new(Metrics::new());
    let config = OtelConfig {
        enabled: true,
        endpoint: Some(endpoint),
        ..Default::default()
    };
    let guard = microwave_otel::init(&config, metrics.clone(), "http").unwrap();
    metrics.record_http("/_status", "GET", "acme", 200, Duration::from_millis(3));
    guard.force_flush().await.unwrap();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(5)).unwrap(),
        "POST /v1/metrics HTTP/1.1"
    );
    assert_eq!(metrics.export_error(), None);
}
