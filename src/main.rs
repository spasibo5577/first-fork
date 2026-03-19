//! Craton Infrastructure Daemon — autonomous server management.
//!
//! Single binary, single control loop, no external runtime dependencies.

mod breaker;
mod config;
mod effect;
mod graph;
mod history;
mod http;
mod lease;
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
use std::sync::mpsc;
use std::time::Duration;

fn main() {
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/etc/craton/config.toml".to_string());

    match run(&config_path) {
        Ok(()) => {
            eprintln!("[cratond] stopped");
        }
        Err(e) => {
            eprintln!("FATAL: {e}");
            std::process::exit(1);
        }
    }
}

fn run(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    // ── Phase 1: Load config ──
    let raw = std::fs::read_to_string(config_path)
        .map_err(|e| format!("reading config {config_path}: {e}"))?;

    let cfg = config::CratonConfig::from_toml(&raw)?;

    eprintln!(
        "[cratond] config loaded — {} services, backup: {}",
        cfg.services.len(),
        cfg.backup.restic_repo,
    );

    // ── Phase 2: Build dependency graph ──
    let dep_graph = graph::DepGraph::build(&cfg.services)?;
    eprintln!(
        "[cratond] dependency graph: {} nodes, order: {:?}",
        dep_graph.all_services().len(),
        dep_graph.topological_order()
    );

    // ── Phase 3: Create directories ──
    for dir in &["/var/lib/craton", "/run/craton"] {
        let path = std::path::Path::new(dir);
        if !path.exists() {
            if let Err(e) = std::fs::create_dir_all(path) {
                eprintln!("[cratond] warning: cannot create {dir}: {e}");
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
    http::spawn_http_thread(&cfg.daemon.listen, snapshot.clone(), event_tx.clone())?;

    // ── Phase 5: Run control loop (blocks until shutdown) ──
    runtime::run_control_loop(
        &cfg,
        &dep_graph,
        event_rx,
        event_tx,
        snapshot,
        notif_sender,
        &sd,
    );

    Ok(())
}