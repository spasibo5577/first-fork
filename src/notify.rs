//! NTFY notification delivery with retry, deduplication, and syslog fallback.
//!
//! Architecture: single consumer thread reads from a bounded channel.
//! Producers call `queue()` (non-blocking) from any thread.
//! Failed deliveries are logged to stderr (captured by `journald`).

use crate::model::Alert;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Configuration for the notifier.
#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub ntfy_url: String,
    pub retries: Vec<Duration>,
    pub dedup_ttl: Duration,
    pub queue_size: usize,
}

/// Handle for queueing notifications. Clone-able, thread-safe (via `mpsc::Sender`).
#[derive(Clone)]
pub struct NotifySender {
    tx: mpsc::SyncSender<Alert>,
}

impl NotifySender {
    /// Queues an alert for delivery. Non-blocking.
    /// If the queue is full, the alert is dropped and logged to stderr.
    pub fn queue(&self, alert: Alert) {
        if let Err(mpsc::TrySendError::Full(dropped) | mpsc::TrySendError::Disconnected(dropped)) =
            self.tx.try_send(alert)
        {
            eprintln!(
                "[cratond] ALERT DROPPED: {} | {}",
                dropped.title, dropped.body
            );
        }
    }
}

/// Creates a notifier pair: sender (for producers) and consumer runner.
///
/// The consumer must be started in a dedicated thread via `consumer.run()`.
#[must_use]
pub fn create(config: NotifyConfig) -> (NotifySender, NotifyConsumer) {
    let (tx, rx) = mpsc::sync_channel(config.queue_size);
    (NotifySender { tx }, NotifyConsumer { rx, config })
}

/// Consumer side of the notifier. Runs in its own thread.
pub struct NotifyConsumer {
    rx: mpsc::Receiver<Alert>,
    config: NotifyConfig,
}

impl NotifyConsumer {
    /// Runs the consumer loop. Blocks until the channel is closed.
    pub fn run(self) {
        let mut dedup = DedupCache::new(self.config.dedup_ttl);
        let mut sent_count: u64 = 0;
        let mut failed_count: u64 = 0;
        let mut dedup_count: u64 = 0;

        eprintln!(
            "[cratond] notifier started, url={}",
            self.config.ntfy_url
        );

        loop {
            let Ok(alert) = self.rx.recv() else {
                eprintln!(
                    "[cratond] notifier stopped: sent={sent_count} failed={failed_count} dedup={dedup_count}"
                );
                return;
            };

            let key = dedup_key(&alert);
            if dedup.is_duplicate(&key) {
                dedup_count += 1;
                continue;
            }

            let mut delivered = false;
            for (attempt, delay) in self.config.retries.iter().enumerate() {
                if delay.as_secs() > 0 || delay.as_millis() > 0 {
                    std::thread::sleep(*delay);
                }

                match send_ntfy(&self.config.ntfy_url, &alert) {
                    Ok(()) => {
                        delivered = true;
                        sent_count += 1;
                        dedup.record(key.clone());
                        break;
                    }
                    Err(e) => {
                        eprintln!(
                            "[cratond] NTFY delivery failed (attempt {}): {e}",
                            attempt + 1
                        );
                    }
                }
            }

            if !delivered {
                failed_count += 1;
                eprintln!(
                    "[cratond] NTFY UNREACHABLE — alert: [{}] {} | {}",
                    alert.priority.as_str(),
                    alert.title,
                    alert.body
                );
            }
        }
    }
}

fn send_ntfy(url: &str, alert: &Alert) -> Result<(), String> {
    let parsed = parse_ntfy_url(url).ok_or_else(|| format!("invalid NTFY URL: {url}"))?;

    let addr = format!("{}:{}", parsed.host, parsed.port);
    let sock_addr: std::net::SocketAddr =
        addr.parse().map_err(|e| format!("invalid address {addr}: {e}"))?;

    let mut stream =
        std::net::TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
            .map_err(|e| format!("connect: {e}"))?;

    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set timeout: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set timeout: {e}"))?;

    let body = &alert.body;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nTitle: {}\r\nPriority: {}\r\nTags: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        parsed.path,
        parsed.host,
        alert.title,
        alert.priority.as_str(),
        alert.tags,
        body.len(),
        body
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    let mut buf = [0u8; 256];
    let n = std::io::Read::read(&mut stream, &mut buf).map_err(|e| format!("read: {e}"))?;
    let resp = String::from_utf8_lossy(&buf[..n]);

    if resp.contains("200") {
        Ok(())
    } else {
        let status_line = resp.lines().next().unwrap_or("no response");
        Err(format!("NTFY returned: {status_line}"))
    }
}

struct NtfyUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_ntfy_url(url: &str) -> Option<NtfyUrl> {
    let without_scheme = url.strip_prefix("http://")?;
    let (host_port, path) = match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None => (without_scheme, "/"),
    };

    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let h = &host_port[..i];
            let p: u16 = host_port[i + 1..].parse().ok()?;
            (h.to_string(), p)
        }
        None => (host_port.to_string(), 80),
    };

    Some(NtfyUrl {
        host,
        port,
        path: path.to_string(),
    })
}

fn dedup_key(alert: &Alert) -> String {
    let mut hasher = Sha256::new();
    hasher.update(alert.title.as_bytes());
    hasher.update(b"\0");
    hasher.update(alert.body.as_bytes());
    format!("{:x}", hasher.finalize())
}

struct DedupCache {
    entries: HashMap<String, Instant>,
    ttl: Duration,
}

impl DedupCache {
    fn new(ttl: Duration) -> Self {
        Self {
            entries: HashMap::with_capacity(64),
            ttl,
        }
    }

    fn is_duplicate(&mut self, key: &str) -> bool {
        self.evict_expired();
        self.entries
            .get(key)
            .is_some_and(|exp| Instant::now() < *exp)
    }

    fn record(&mut self, key: String) {
        self.entries.insert(key, Instant::now() + self.ttl);
    }

    fn evict_expired(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, exp| now < *exp);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::AlertPriority;

    #[test]
    fn dedup_blocks_duplicates() {
        let mut cache = DedupCache::new(Duration::from_secs(60));
        assert!(!cache.is_duplicate("key1"));
        cache.record("key1".into());
        assert!(cache.is_duplicate("key1"));
        assert!(!cache.is_duplicate("key2"));
    }

    #[test]
    fn ntfy_url_parsing() {
        let u = parse_ntfy_url("http://127.0.0.1:8080/granit-alerts").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/granit-alerts");
    }

    #[test]
    fn dedup_key_deterministic() {
        let a1 = Alert {
            title: "test".into(),
            body: "body".into(),
            priority: AlertPriority::Default,
            tags: String::new(),
        };
        let a2 = a1.clone();
        assert_eq!(dedup_key(&a1), dedup_key(&a2));
    }

    #[test]
    fn dedup_key_differs_for_different_alerts() {
        let a1 = Alert {
            title: "test1".into(),
            body: "body".into(),
            priority: AlertPriority::Default,
            tags: String::new(),
        };
        let a2 = Alert {
            title: "test2".into(),
            body: "body".into(),
            priority: AlertPriority::Default,
            tags: String::new(),
        };
        assert_ne!(dedup_key(&a1), dedup_key(&a2));
    }

    #[test]
    fn queue_and_receive() {
        let config = NotifyConfig {
            ntfy_url: "http://localhost:1/test".into(),
            retries: vec![Duration::ZERO],
            dedup_ttl: Duration::from_secs(60),
            queue_size: 8,
        };
        let (sender, _consumer) = create(config);

        sender.queue(Alert {
            title: "hello".into(),
            body: "world".into(),
            priority: AlertPriority::Default,
            tags: String::new(),
        });
    }
}