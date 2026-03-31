//! NTFY notification delivery with retry, deduplication, and durable outbox.
//!
//! Architecture: single consumer thread reads from a bounded channel.
//! Producers call `queue()` (non-blocking) from any thread.
//!
//! # Durable outbox
//!
//! All alerts are written to `/var/lib/craton/alert-outbox.jsonl` before
//! delivery. On startup, undelivered entries from previous runs are replayed.
//! On overflow (>256 entries) oldest delivered entries are evicted first,
//! then oldest undelivered (with explicit loss logging).
//!
//! The overflow flag (`Arc<AtomicBool>`) is shared with the control loop so
//! it can be published in the snapshot and checked by `GET /health`.

use crate::model::Alert;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const OUTBOX_MAX: usize = 256;

// ─── Config / public API ──────────────────────────────────────

/// Configuration for the notifier.
#[derive(Debug, Clone)]
pub struct NotifyConfig {
    pub ntfy_url: String,
    pub retries: Vec<Duration>,
    pub dedup_ttl: Duration,
    pub queue_size: usize,
    /// Path to the durable alert outbox (JSONL file).
    pub outbox_path: String,
    /// Set to `true` when an undelivered alert is evicted due to overflow.
    /// Shared with the control loop for snapshot publication.
    pub overflow_flag: Arc<AtomicBool>,
    /// Shared delivery-health state for operator observability.
    pub runtime_state: NotifyRuntimeState,
}

/// Shared runtime health for the notification channel.
#[derive(Debug, Clone)]
pub struct NotifyRuntimeState {
    degraded: Arc<AtomicBool>,
    consecutive_failures: Arc<AtomicU64>,
    last_success_epoch_secs: Arc<AtomicU64>,
    last_failure_epoch_secs: Arc<AtomicU64>,
}

impl NotifyRuntimeState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            degraded: Arc::new(AtomicBool::new(false)),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
            last_success_epoch_secs: Arc::new(AtomicU64::new(0)),
            last_failure_epoch_secs: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn record_delivery_success(&self) {
        let previous_failures = self.consecutive_failures.swap(0, Ordering::Relaxed);
        let was_degraded = self.degraded.swap(false, Ordering::Relaxed);
        self.last_success_epoch_secs
            .store(epoch_secs_now(), Ordering::Relaxed);

        if was_degraded {
            crate::log::info(
                "notify",
                &format!(
                    "notification channel recovered after {previous_failures} consecutive failure(s)"
                ),
            );
        }
    }

    pub fn record_delivery_failure(&self) {
        let current = self
            .consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let already_degraded = self.degraded.swap(true, Ordering::Relaxed);
        self.last_failure_epoch_secs
            .store(epoch_secs_now(), Ordering::Relaxed);

        if !already_degraded {
            crate::log::error(
                "notify",
                &format!(
                    "notification channel degraded: first failed delivery in current streak (count={current})"
                ),
            );
        }
    }

    #[must_use]
    pub fn degraded(&self) -> bool {
        self.degraded.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn consecutive_failures(&self) -> u64 {
        self.consecutive_failures.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn last_success_epoch_secs(&self) -> u64 {
        self.last_success_epoch_secs.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn last_failure_epoch_secs(&self) -> u64 {
        self.last_failure_epoch_secs.load(Ordering::Relaxed)
    }
}

/// Handle for queueing notifications. Clone-able, thread-safe.
#[derive(Clone)]
pub struct NotifySender {
    tx: mpsc::SyncSender<Alert>,
}

impl NotifySender {
    /// Queues an alert for delivery. Non-blocking.
    /// If the queue is full the alert is dropped and logged to stderr.
    pub fn queue(&self, alert: Alert) {
        if let Err(mpsc::TrySendError::Full(dropped) | mpsc::TrySendError::Disconnected(dropped)) =
            self.tx.try_send(alert)
        {
            crate::log::warn(
                "notify",
                &format!("alert dropped (queue full): {} | {}", dropped.title, dropped.body),
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

// ─── Consumer ─────────────────────────────────────────────────

/// Consumer side of the notifier. Runs in its own thread.
pub struct NotifyConsumer {
    rx: mpsc::Receiver<Alert>,
    config: NotifyConfig,
}

impl NotifyConsumer {
    /// Runs the consumer loop. Blocks until the channel is closed.
    #[allow(clippy::too_many_lines)]
    pub fn run(self) {
        let outbox_path = PathBuf::from(&self.config.outbox_path);
        let mut outbox = Outbox::load(&outbox_path);
        let mut dedup = DedupCache::new(self.config.dedup_ttl);
        let mut sent_count: u64 = 0;
        let mut failed_count: u64 = 0;
        let mut dedup_count: u64 = 0;

        crate::log::info("notify", "notifier started");

        // ── Startup replay ────────────────────────────────────
        let undelivered: Vec<OutboxEntry> = outbox
            .entries
            .iter()
            .filter(|e| !e.delivered)
            .cloned()
            .collect();

        if !undelivered.is_empty() {
            crate::log::info(
                "notify",
                &format!("replaying {} undelivered alert(s) from outbox", undelivered.len()),
            );
        }

        for entry in &undelivered {
            if dedup.is_duplicate(&entry.id) {
                dedup_count += 1;
                continue;
            }
            let mut delivered = false;
            for delay in &self.config.retries {
                if !delay.is_zero() {
                    std::thread::sleep(*delay);
                }
                if send_ntfy(&self.config.ntfy_url, &entry.alert).is_ok() {
                    delivered = true;
                    dedup.record(entry.id.clone());
                    self.config.runtime_state.record_delivery_success();
                    break;
                }
            }
            if delivered {
                outbox.mark_delivered(&entry.id);
                sent_count += 1;
            } else {
                failed_count += 1;
                self.config.runtime_state.record_delivery_failure();
                crate::log::warn(
                    "notify",
                    &format!("replay delivery failed for '{}', will retry next start", entry.alert.title),
                );
            }
        }
        if !undelivered.is_empty() {
            outbox.persist(&outbox_path);
        }

        // ── Main delivery loop ────────────────────────────────
        loop {
            let Ok(alert) = self.rx.recv() else {
                eprintln!(
                    "[cratond] notifier stopped: sent={sent_count} \
                     failed={failed_count} dedup={dedup_count}"
                );
                return;
            };

            let key = dedup_key(&alert);
            if dedup.is_duplicate(&key) {
                dedup_count += 1;
                continue;
            }

            // Persist before delivery (crash-safe outbox).
            outbox.append(OutboxEntry {
                id: key.clone(),
                alert: alert.clone(),
                created_at: epoch_secs_now(),
                delivered: false,
            });

            if outbox.compact_if_needed(OUTBOX_MAX) {
                self.config.overflow_flag.store(true, Ordering::Relaxed);
            }
            outbox.persist(&outbox_path);

            // Attempt delivery with retries.
            let mut delivered = false;
            for (attempt, delay) in self.config.retries.iter().enumerate() {
                if !delay.is_zero() {
                    std::thread::sleep(*delay);
                }
                match send_ntfy(&self.config.ntfy_url, &alert) {
                    Ok(()) => {
                        delivered = true;
                        sent_count += 1;
                        dedup.record(key.clone());
                        self.config.runtime_state.record_delivery_success();
                        break;
                    }
                    Err(e) => {
                        if attempt + 1 == self.config.retries.len() {
                            crate::log::warn(
                                "notify",
                                &format!(
                                    "NTFY delivery exhausted retries ({} attempt(s)): {e}",
                                    attempt + 1
                                ),
                            );
                        }
                    }
                }
            }

            if delivered {
                outbox.mark_delivered(&key);
                outbox.persist(&outbox_path);
            } else {
                failed_count += 1;
                self.config.runtime_state.record_delivery_failure();
                crate::log::error(
                    "notify",
                    &format!(
                        "NTFY unreachable: [{}] {} | {}",
                        alert.priority.as_str(),
                        alert.title,
                        alert.body
                    ),
                );
            }
        }
    }
}

// ─── Outbox ───────────────────────────────────────────────────

/// In-memory outbox backed by a persistent JSONL file.
struct Outbox {
    entries: Vec<OutboxEntry>,
}

impl Outbox {
    /// Loads outbox from disk; returns empty outbox on any error.
    fn load(path: &Path) -> Self {
        let data = match crate::persist::read_optional(path) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Self {
                    entries: Vec::new(),
                }
            }
            Err(e) => {
                eprintln!("[cratond] failed to read outbox {}: {e}", path.display());
                return Self {
                    entries: Vec::new(),
                };
            }
        };

        // Parse JSONL lines, skip corrupt ones
        let mut parsed: Vec<OutboxEntry> = String::from_utf8_lossy(&data)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|line| match serde_json::from_str::<OutboxEntry>(line) {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("[cratond] outbox: skipping corrupt entry: {e}");
                    None
                }
            })
            .collect();

        // Collapse entries by id, keeping only the latest record (by created_at).
        // This prevents duplicate IDs with conflicting delivered flags.
        let mut latest: HashMap<String, OutboxEntry> = HashMap::new();
        for e in parsed.drain(..) {
            match latest.get(&e.id) {
                Some(existing) if existing.created_at >= e.created_at => {
                    // keep existing
                }
                _ => {
                    latest.insert(e.id.clone(), e);
                }
            }
        }

        // Rebuild entries vector sorted by created_at ascending for deterministic behavior.
        let mut entries: Vec<OutboxEntry> = latest.into_values().collect();
        entries.sort_by_key(|e| e.created_at);

        Self { entries }
    }

    fn append(&mut self, entry: OutboxEntry) {
        self.entries.push(entry);
    }

    fn mark_delivered(&mut self, id: &str) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.id == id) {
            e.delivered = true;
        }
    }

    /// Evicts entries until `entries.len() <= max`.
    /// Strategy: oldest delivered first, then oldest undelivered.
    /// Returns `true` if any undelivered entries were evicted (overflow).
    fn compact_if_needed(&mut self, max: usize) -> bool {
        if self.entries.len() <= max {
            return false;
        }

        // Sort ascending by creation time so we always remove the oldest.
        self.entries.sort_by_key(|e| e.created_at);

        let mut undelivered_lost = false;
        while self.entries.len() > max {
            if let Some(pos) = self.entries.iter().position(|e| e.delivered) {
                self.entries.remove(pos);
            } else {
                // No delivered entries left — must drop an undelivered one.
                let lost = self.entries.remove(0);
                eprintln!(
                    "[cratond] OUTBOX OVERFLOW: evicting undelivered alert '{}' (created_at={})",
                    lost.alert.title, lost.created_at
                );
                undelivered_lost = true;
            }
        }

        undelivered_lost
    }

    /// Atomically persists the full outbox to disk.
    fn persist(&self, path: &Path) {
        let mut buf = String::with_capacity(self.entries.len() * 128);
        for entry in &self.entries {
            match serde_json::to_string(entry) {
                Ok(line) => {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                Err(e) => eprintln!("[cratond] outbox: failed to serialize entry: {e}"),
            }
        }
        if let Err(e) = crate::persist::atomic_write(path, buf.as_bytes()) {
            eprintln!("[cratond] failed to persist outbox: {e}");
        }
    }

    /// Total number of entries (delivered + undelivered).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Number of undelivered entries.
    #[cfg(test)]
    fn undelivered_count(&self) -> usize {
        self.entries.iter().filter(|e| !e.delivered).count()
    }
}

// ─── OutboxEntry ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OutboxEntry {
    id: String,
    alert: Alert,
    created_at: u64,
    delivered: bool,
}

// ─── NTFY delivery ────────────────────────────────────────────

fn send_ntfy(url: &str, alert: &Alert) -> Result<(), String> {
    let parsed = parse_ntfy_url(url).ok_or_else(|| format!("invalid NTFY URL: {url}"))?;

    let addr = format!("{}:{}", parsed.host, parsed.port);
    let sock_addr: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| format!("invalid address {addr}: {e}"))?;

    let mut stream = std::net::TcpStream::connect_timeout(&sock_addr, Duration::from_secs(5))
        .map_err(|e| format!("connect: {e}"))?;

    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set timeout: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set timeout: {e}"))?;

    let body = &alert.body;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nTitle: {}\r\nPriority: {}\r\nTags: {}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
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

// ─── Dedup cache ──────────────────────────────────────────────

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

// ─── Helpers ──────────────────────────────────────────────────

fn epoch_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── Tests ────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::model::AlertPriority;

    fn make_alert(title: &str) -> Alert {
        Alert {
            title: title.to_string(),
            body: "body".to_string(),
            priority: AlertPriority::Default,
            tags: String::new(),
        }
    }

    fn make_entry(title: &str, created_at: u64, delivered: bool) -> OutboxEntry {
        let alert = make_alert(title);
        let id = dedup_key(&alert);
        OutboxEntry {
            id,
            alert,
            created_at,
            delivered,
        }
    }

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
        let a1 = make_alert("test");
        let a2 = a1.clone();
        assert_eq!(dedup_key(&a1), dedup_key(&a2));
    }

    #[test]
    fn dedup_key_differs_for_different_alerts() {
        let a1 = make_alert("test1");
        let a2 = make_alert("test2");
        assert_ne!(dedup_key(&a1), dedup_key(&a2));
    }

    #[test]
    fn queue_and_receive() {
        let config = NotifyConfig {
            ntfy_url: "http://localhost:1/test".into(),
            retries: vec![Duration::ZERO],
            dedup_ttl: Duration::from_secs(60),
            queue_size: 8,
            outbox_path: "/tmp/test-outbox.jsonl".into(),
            overflow_flag: Arc::new(AtomicBool::new(false)),
            runtime_state: NotifyRuntimeState::new(),
        };
        let (sender, _consumer) = create(config);
        sender.queue(make_alert("hello"));
    }

    #[test]
    fn runtime_state_marks_degraded_and_recovers() {
        let state = NotifyRuntimeState::new();
        assert!(!state.degraded());
        assert_eq!(state.consecutive_failures(), 0);

        state.record_delivery_failure();
        assert!(state.degraded());
        assert_eq!(state.consecutive_failures(), 1);
        assert!(state.last_failure_epoch_secs() > 0);

        state.record_delivery_failure();
        assert_eq!(state.consecutive_failures(), 2);

        state.record_delivery_success();
        assert!(!state.degraded());
        assert_eq!(state.consecutive_failures(), 0);
        assert!(state.last_success_epoch_secs() > 0);
    }

    #[test]
    fn outbox_compact_removes_delivered_first() {
        let mut outbox = Outbox {
            entries: Vec::new(),
        };

        // 3 delivered (older) + 1 undelivered
        for i in 0..3u64 {
            outbox.append(make_entry(&format!("delivered-{i}"), i, true));
        }
        outbox.append(make_entry("undelivered", 10, false));

        // max=3 → should remove 1 delivered (oldest)
        let overflow = outbox.compact_if_needed(3);
        assert!(!overflow, "no undelivered should be lost");
        assert_eq!(outbox.len(), 3);
        assert_eq!(outbox.undelivered_count(), 1, "undelivered must survive");
    }

    #[test]
    fn outbox_compact_drops_undelivered_when_no_delivered() {
        let mut outbox = Outbox {
            entries: Vec::new(),
        };
        for i in 0..5u64 {
            outbox.append(make_entry(&format!("pending-{i}"), i, false));
        }

        let overflow = outbox.compact_if_needed(3);
        assert!(overflow, "must signal overflow");
        assert_eq!(outbox.len(), 3);
    }

    #[test]
    fn outbox_mark_delivered() {
        let mut outbox = Outbox {
            entries: Vec::new(),
        };
        let entry = make_entry("test", 1, false);
        let id = entry.id.clone();
        outbox.append(entry);

        assert_eq!(outbox.undelivered_count(), 1);
        outbox.mark_delivered(&id);
        assert_eq!(outbox.undelivered_count(), 0);
    }

    #[test]
    fn outbox_persist_and_load() {
        let dir = std::env::temp_dir().join("craton_test_outbox");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        let path = dir.join("alert-outbox.jsonl");

        let mut outbox = Outbox {
            entries: Vec::new(),
        };
        outbox.append(make_entry("alpha", 100, false));
        outbox.append(make_entry("beta", 200, true));
        outbox.persist(&path);

        let loaded = Outbox::load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.undelivered_count(), 1);
        assert_eq!(loaded.entries[0].alert.title, "alpha");
        assert_eq!(loaded.entries[1].alert.title, "beta");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outbox_load_nonexistent_returns_empty() {
        let outbox = Outbox::load(std::path::Path::new("/nonexistent/path/outbox.jsonl"));
        assert_eq!(outbox.len(), 0);
    }

    #[test]
    fn overflow_flag_set_on_undelivered_eviction() {
        let flag = Arc::new(AtomicBool::new(false));
        let mut outbox = Outbox {
            entries: Vec::new(),
        };
        for i in 0..10u64 {
            outbox.append(make_entry(&format!("p{i}"), i, false));
        }
        let overflowed = outbox.compact_if_needed(5);
        if overflowed {
            flag.store(true, Ordering::Relaxed);
        }
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn outbox_collapse_duplicate_ids_keeps_latest() {
        // Regression test for issue: duplicate IDs with conflicting delivered flags.
        // Create a JSONL file with duplicate IDs and different delivered states.
        let dir = std::env::temp_dir().join("craton_test_collapse");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        let path = dir.join("alert-outbox.jsonl");

        // Create JSONL with duplicate IDs simulating the bug:
        // Same ID appears twice with different created_at and delivered flags.
        let entry1 = make_entry("startup", 100, true);
        let entry2 = make_entry("startup", 200, false);
        assert_eq!(entry1.id, entry2.id, "same title → same ID");

        let entry3 = make_entry("alert", 300, false);
        let entry4 = make_entry("alert", 400, true);
        assert_eq!(entry3.id, entry4.id, "same title → same ID");

        // Manually write JSONL with duplicates (simulating past bug behavior)
        let jsonl_content = format!(
            "{}\n{}\n{}\n{}",
            serde_json::to_string(&entry1).unwrap(),
            serde_json::to_string(&entry2).unwrap(),
            serde_json::to_string(&entry3).unwrap(),
            serde_json::to_string(&entry4).unwrap(),
        );
        std::fs::write(&path, &jsonl_content).expect("write JSONL");

        // Load and verify collapse behavior
        let loaded = Outbox::load(&path);

        // Should have 2 entries (one per unique ID), not 4
        assert_eq!(loaded.len(), 2, "should collapse to unique IDs");

        // Verify latest record is kept for each ID
        // entry2 (created_at=200, delivered=false) should be latest for entry1.id
        // entry4 (created_at=400, delivered=true) should be latest for entry3.id
        let startup_entry = loaded.entries.iter().find(|e| e.id == entry1.id).unwrap();
        assert_eq!(startup_entry.created_at, 200, "should keep latest created_at");
        assert!(!startup_entry.delivered, "startup should be undelivered (latest)");

        let alert_entry = loaded.entries.iter().find(|e| e.id == entry3.id).unwrap();
        assert_eq!(alert_entry.created_at, 400, "should keep latest created_at");
        assert!(alert_entry.delivered, "alert should be delivered (latest)");

        // Verify replay count is correct: only 1 undelivered entry (startup)
        let undelivered_count = loaded
            .entries
            .iter()
            .filter(|e| !e.delivered)
            .count();
        assert_eq!(undelivered_count, 1, "only startup should be undelivered for replay");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
