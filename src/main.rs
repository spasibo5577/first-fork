//! Craton Infrastructure Daemon — autonomous server management.
//!
//! Single binary, single control loop, no external runtime dependencies.
//!
//! Startup phases:
//!   1. Load config
//!   2. Build dependency graph
//!   3. Create runtime directories
//!   4. Initialize subsystems (signal, notifier, HTTP, token)
//!   5. Run control loop (blocks until shutdown)

mod breaker;
mod config;
mod effect;
mod graph;
mod history;
mod http;
mod lease;
mod log;
mod model;
mod notify;
mod persist;
mod policy;
mod reduce;
mod runtime;
mod schedule;
mod signal;
mod state;

use model::{Event, SignalKind};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::Duration;

/// Loads an existing remediation token or generates a new one on first run.
///
/// Token is a hex-encoded SHA-256 digest seeded from PID + time.
/// Written atomically to `token_path` so it survives daemon restarts.
fn load_or_create_token(token_path: &str) -> String {
    let path = std::path::Path::new(token_path);

    // Try to read existing token.
    if let Ok(Some(data)) = persist::read_optional(path) {
        if let Ok(s) = String::from_utf8(data) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }

    // Generate new token: SHA-256(pid || now_nanos || salt).
    let pid = std::process::id();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    hasher.update(pid.to_le_bytes());
    hasher.update(now_ns.to_le_bytes());
    hasher.update(b"cratond-remediation-token-v1");
    let token = format!("{:x}", hasher.finalize());

    if let Err(e) = persist::atomic_write(path, token.as_bytes()) {
        crate::log::warn("startup", &format!("failed to persist remediation token: {e}"));
    } else {
        crate::log::info("startup", "remediation token persisted");
    }

    token
}

fn main() {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/craton/config.toml".to_string());

    match run(&config_path) {
        Ok(()) => {
            crate::log::info("startup", "daemon stopped");
        }
        Err(e) => {
            crate::log::error("startup", &format!("fatal error: {e}"));
            std::process::exit(1);
        }
    }
}

fn run(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ── Phase 1: Load config ──
    let raw = std::fs::read_to_string(config_path)
        .map_err(|e| format!("reading config {config_path}: {e}"))?;

    let cfg = config::CratonConfig::from_toml(&raw)?;

    crate::log::info(
        "startup",
        &format!("config loaded: {} services", cfg.services.len()),
    );

    // ── Phase 2: Build dependency graph ──
    let dep_graph = graph::DepGraph::build(&cfg.services)?;
    crate::log::info(
        "startup",
        &format!(
            "dependency graph built: {} nodes, {} ordered",
            dep_graph.all_services().len(),
            dep_graph.topological_order().len()
        ),
    );

    // ── Phase 3: Create directories ──
    for dir in &["/var/lib/craton", "/run/craton"] {
        let path = std::path::Path::new(dir);
        if !path.exists() {
            if let Err(e) = std::fs::create_dir_all(path) {
                crate::log::warn("startup", &format!("cannot create {dir}: {e}"));
            }
        }
    }

    // ── Phase 4: Initialize subsystems ──
    let sd = effect::systemd::SdNotify::from_env();

    // Event channel: all event sources send here, control loop receives.
    let (event_tx, event_rx) = mpsc::channel::<Event>();

    // Signal handler.
    {
        let tx = event_tx.clone();
        let (sig_tx, sig_rx) = mpsc::channel::<SignalKind>();

        std::thread::Builder::new()
            .name("signal-adapter".into())
            .spawn(move || {
                for kind in sig_rx {
                    if tx.send(Event::Signal(kind)).is_err() {
                        break;
                    }
                }
            })
            .ok();

        signal::spawn_signal_thread(sig_tx)?;
    }

    // Notifier.
    let outbox_overflow = Arc::new(AtomicBool::new(false));
    let notify_config = notify::NotifyConfig {
        ntfy_url: format!("{}/{}", cfg.ntfy.url, cfg.ntfy.topic),
        retries: cfg
            .ntfy
            .retries
            .iter()
            .map(|&s| Duration::from_secs(s))
            .collect(),
        dedup_ttl: Duration::from_secs(1800), // 30 min
        queue_size: 64,
        outbox_path: "/var/lib/craton/alert-outbox.jsonl".into(),
        overflow_flag: outbox_overflow.clone(),
    };
    let (notif_sender, notif_consumer) = notify::create(notify_config);

    std::thread::Builder::new()
        .name("notifier".into())
        .spawn(move || {
            notif_consumer.run();
        })
        .map_err(|e| format!("spawning notifier: {e}"))?;

    // HTTP API.
    let snapshot = http::empty_snapshot();
    let remediation_token = load_or_create_token(&cfg.ai.token_path);
    http::spawn_http_thread(
        &cfg.daemon.listen,
        snapshot.clone(),
        event_tx.clone(),
        remediation_token,
    )?;

    // ── Phase 5: Run control loop (blocks until shutdown) ──
    runtime::run_control_loop(
        runtime::RuntimeDeps {
            config: &cfg,
            graph: &dep_graph,
            snapshot,
            notifier: notif_sender,
            sd_notify: &sd,
            outbox_overflow,
        },
        event_rx,
        event_tx,
    );

    Ok(())
}
