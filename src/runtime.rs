//! Control loop — the central event processor.
//!
//! Owns all mutable state. Receives events from:
//! - Scheduler (tick timer)
//! - Signal handler
//! - HTTP API (triggers, remediation)
//! - Effect completions (probe results, command results)
//!
//! Calls the reducer for each event, then dispatches commands.

use crate::config::CratonConfig;
use crate::effect;
use crate::graph::DepGraph;
use crate::http::SharedSnapshot;
use crate::model::{
    Alert, AlertPriority, BackupPhase, Command, Event, ProbeResult, ServiceId, TaskKind,
};
use crate::notify::NotifySender;
use crate::reduce::{self, Ctx};
use crate::schedule::{self, Schedule, WallClock};
use crate::state::State;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Runs the main control loop. Blocks until shutdown.
pub fn run_control_loop(
    config: &CratonConfig,
    graph: &DepGraph,
    event_rx: mpsc::Receiver<Event>,
    _event_tx: mpsc::Sender<Event>,
    snapshot: SharedSnapshot,
    notifier: NotifySender,
    sd_notify: &crate::effect::systemd::SdNotify,
) {
    let mono_start = monotonic_secs();
    let mut state = State::new(config, mono_start);

    // Crash recovery from persisted backup state.
    let persisted_phase = load_persisted_backup_phase(config);
    if !persisted_phase.is_idle() {
        eprintln!("[cratond] crash recovery needed: backup phase = {persisted_phase:?}");
        let event = Event::StartupRecovery {
            persisted_backup: persisted_phase,
        };
        let cmds = reduce(
            &mut state, event, config, graph,
            &make_ctx(),
        );
        execute_commands(&cmds, config, &notifier, &snapshot, &mut state, sd_notify);
    }

    // Signal ready.
    sd_notify.ready();
    sd_notify.status("running");
    eprintln!("[cratond] control loop started");

    // Notify startup.
    notifier.queue(Alert {
        title: "🟢 Кратон запущен".into(),
        body: format!(
            "Сервисов: {}, backup: {}",
            config.services.len(),
            config.backup.restic_repo
        ),
        priority: AlertPriority::Default,
        tags: "white_check_mark".into(),
    });

    // Schedule state: last run timestamps for interval tasks.
    let mut last_recovery = Instant::now();
    let mut last_disk = Instant::now();
    let recovery_interval = Duration::from_secs(300); // 5 min
    let disk_interval = Duration::from_secs(6 * 3600); // 6h

    // Startup: run first recovery after 2 minutes.
    let startup_delay = Duration::from_secs(120);
    let start_instant = Instant::now();

    loop {
        // Check scheduled tasks.
        let mut due_tasks = Vec::new();

        let elapsed = start_instant.elapsed();
        if elapsed >= startup_delay {
            if last_recovery.elapsed() >= recovery_interval {
                due_tasks.push(TaskKind::Recovery);
                last_recovery = Instant::now();
            }

            if last_disk.elapsed() >= disk_interval {
                due_tasks.push(TaskKind::DiskMonitor);
                last_disk = Instant::now();
            }

            // Time-of-day schedules.
            let wall = WallClock::now();
            check_daily_schedules(&mut due_tasks, &mut state, &wall, config);
        }

        // Emit tick if there are due tasks.
        if !due_tasks.is_empty() {
            let event = Event::Tick { due_tasks };
            let ctx = make_ctx();
            let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
            execute_commands(&cmds, config, &notifier, &snapshot, &mut state, sd_notify);
        }

        // Process events from channel (with timeout for scheduler ticks).
        match event_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                let ctx = make_ctx();
                let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
                execute_commands(&cmds, config, &notifier, &snapshot, &mut state, sd_notify);

                if state.shutting_down {
                    eprintln!("[cratond] shutdown initiated");
                    sd_notify.stopping();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Normal — just loop back to check schedules.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[cratond] event channel disconnected — shutting down");
                break;
            }
        }
    }

    // Graceful shutdown.
    eprintln!("[cratond] control loop exiting");
    sd_notify.status("shutting down");

    // Publish final snapshot.
    publish_snapshot(&state, &snapshot);
}


fn handle_run_probes(
    service_ids: &[ServiceId],
    config: &CratonConfig,
    state: &mut State,
    notifier: &NotifySender,
    snapshot: &SharedSnapshot,
    sd_notify: &crate::effect::systemd::SdNotify,
) {
    let results = run_probes(service_ids, config);
    let graph = crate::graph::DepGraph::build(&config.services)
        .unwrap_or_else(|e| panic!("graph build failed: {e}"));
    let ctx = make_ctx();
    let sub_cmds = reduce::reduce(state, Event::ProbeResults(results), config, &graph, &ctx);
    execute_commands(&sub_cmds, config, notifier, snapshot, state, sd_notify);
}

/// Executes commands emitted by the reducer.
fn execute_commands(
    cmds: &[Command],
    config: &CratonConfig,
    notifier: &NotifySender,
    snapshot: &SharedSnapshot,
    state: &mut State,
    sd_notify: &crate::effect::systemd::SdNotify,
) {
    for cmd in cmds {
        match cmd {
            Command::RunProbes(service_ids) => {
                handle_run_probes(service_ids, config, state, notifier, snapshot, sd_notify);
            }
            Command::RestartService { id, unit, reason } => {
                eprintln!("[cratond] restarting {id} ({unit}): {reason}");
                let _ = effect::exec::run_dry_aware(
                    &["systemctl", "restart", unit],
                    Duration::from_secs(30),
                    false, // TODO: pass dry_run from config
                );
            }
            Command::StopService { id, unit, reason } => {
                eprintln!("[cratond] stopping {id} ({unit}): {reason}");
                let _ = effect::exec::run_dry_aware(
                    &["systemctl", "stop", unit],
                    Duration::from_secs(30),
                    false,
                );
            }
            Command::StartService { id, unit } => {
                eprintln!("[cratond] starting {id} ({unit})");
                let _ = effect::exec::run_dry_aware(
                    &["systemctl", "start", unit],
                    Duration::from_secs(30),
                    false,
                );
            }
            Command::RestartDockerDaemon { reason } => {
                eprintln!("[cratond] restarting Docker daemon: {reason}");
                let _ = effect::exec::run(
                    &["systemctl", "kill", "-s", "SIGKILL", "docker.service"],
                    Duration::from_secs(15),
                );
                std::thread::sleep(Duration::from_secs(3));
                let _ = effect::exec::run(
                    &["systemctl", "start", "docker.service"],
                    Duration::from_secs(30),
                );
            }
            Command::SendAlert(alert) => {
                notifier.queue(alert.clone());
            }
            Command::PersistBackupState(phase) => {
                let data = serde_json::to_vec_pretty(phase)
                    .unwrap_or_else(|_| b"{}".to_vec());
                if let Err(e) = crate::persist::atomic_write(
                    std::path::Path::new("/var/lib/craton/backup-state.json"),
                    &data,
                ) {
                    eprintln!("[cratond] failed to persist backup state: {e}");
                }
            }
            Command::PublishSnapshot => {
                publish_snapshot(state, snapshot);
            }
            Command::NotifyWatchdog => {
                sd_notify.watchdog();
            }
            Command::Shutdown { grace_secs } => {
                eprintln!("[cratond] shutdown command received, grace={grace_secs}s");
                state.shutting_down = true;
            }
            Command::ResticUnlock => {
                eprintln!("[cratond] running restic unlock");
                let _ = effect::exec::run(
                    &[
                        &config.backup.restic_binary,
                        "unlock",
                        "--repo", &config.backup.restic_repo,
                        "--password-file", &config.backup.restic_password_file,
                    ],
                    Duration::from_secs(120),
                );
            }
            Command::UpdateLlmContext => {
                let snap = build_snapshot_json(state);
                if let Err(e) = crate::persist::atomic_write(
                    std::path::Path::new(&config.ai.context_path),
                    snap.as_bytes(),
                ) {
                    // Non-fatal — AI context is best-effort.
                    eprintln!("[cratond] failed to write LLM context: {e}");
                }
            }
            Command::WriteIncident(report) => {
                eprintln!(
                    "[cratond] incident: {:?} service={:?}",
                    report.kind, report.service
                );
                // Phase 4: write markdown file.
            }
            // Commands not yet implemented in Phase 3.
            Command::CheckDiskUsage
            | Command::RunDiskCleanup { .. }
            | Command::CheckAptUpdates
            | Command::CheckDockerUpdates
            | Command::ResticBackup { .. }
            | Command::ResticForget { .. }
            | Command::ResticCheck { .. }
            | Command::RunBackupPhase { .. }
            | Command::PersistMaintenance
            | Command::TriggerPicoClaw { .. }
            | Command::AcquireLease { .. }
            | Command::ReleaseLease { .. } => {
                // Skeleton — will be implemented in Phase 4.
            }
        }
    }
}

/// Runs health probes for the given services.
fn run_probes(service_ids: &[ServiceId], config: &CratonConfig) -> Vec<ProbeResult> {
    // Run probes in parallel using scoped threads.
    let results: Vec<ProbeResult> = std::thread::scope(|s| {
        let handles: Vec<_> = service_ids
            .iter()
            .filter_map(|sid| {
                let svc = config.find_service(sid)?;
                Some(s.spawn(move || {
                    effect::probe::run_probe(&svc.id, &svc.probe, &svc.unit)
                }))
            })
            .collect();

        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    });

    results
}

fn publish_snapshot(state: &State, snapshot: &SharedSnapshot) {
    let json = build_snapshot_json(state);
    if let Ok(mut s) = snapshot.lock() {
        *s = json;
    }
}

fn build_snapshot_json(state: &State) -> String {
    // Build a serializable snapshot.
    let mut services = serde_json::Map::new();
    for (id, svc) in &state.services {
        let status_json = serde_json::to_value(&svc.status).unwrap_or_default();
        services.insert(id.as_str().to_string(), status_json);
    }

    let snap = serde_json::json!({
        "services": services,
        "backup_phase": serde_json::to_value(&state.backup_phase).unwrap_or_default(),
        "disk_usage_percent": state.disk_usage_percent,
        "shutting_down": state.shutting_down,
        "backup_history": state.backup_history.to_vec(),
        "recovery_history": state.recovery_history.to_vec(),
    });

    snap.to_string()
}

fn load_persisted_backup_phase(config: &CratonConfig) -> BackupPhase {
    let path = std::path::Path::new("/var/lib/craton/backup-state.json");
    match crate::persist::read_optional(path) {
        Ok(Some(data)) => {
            serde_json::from_slice(&data).unwrap_or(BackupPhase::Idle)
        }
        Ok(None) => {
            // Try legacy Go path.
            let legacy = std::path::Path::new("/var/lib/granit/backup-state.json");
            match crate::persist::read_optional(legacy) {
                Ok(Some(data)) => {
                    eprintln!("[cratond] migrating backup state from Go monolith");
                    serde_json::from_slice(&data).unwrap_or(BackupPhase::Idle)
                }
                _ => BackupPhase::Idle,
            }
        }
        Err(e) => {
            eprintln!("[cratond] failed to read backup state: {e}");
            BackupPhase::Idle
        }
    }
}

fn check_daily_schedules(
    due: &mut Vec<TaskKind>,
    state: &mut State,
    wall: &WallClock,
    _config: &CratonConfig,
) {
    // Backup: odd days at 04:00.
    if schedule::is_due(
        &Schedule::OddDays {
            hour: 4,
            minute: 0,
        },
        *wall,
        state.last_backup_day,
    ) {
        due.push(TaskKind::Backup);
    }

    // APT updates: daily at 09:00.
    if schedule::is_due(
        &Schedule::Daily {
            hour: 9,
            minute: 0,
        },
        *wall,
        state.last_apt_day,
    ) {
        due.push(TaskKind::AptUpdates);
    }

    // Docker updates: weekly Sunday at 10:00.
    if schedule::is_due(
        &Schedule::Weekly {
            weekday: 6,
            hour: 10,
            minute: 0,
        },
        *wall,
        state.last_docker_day,
    ) {
        due.push(TaskKind::DockerUpdates);
    }

    // Daily summary: 09:05.
    if schedule::is_due(
        &Schedule::Daily {
            hour: 9,
            minute: 5,
        },
        *wall,
        state.last_summary_day,
    ) {
        due.push(TaskKind::DailySummary);
    }
}

fn make_ctx() -> Ctx {
    Ctx {
        mono_secs: monotonic_secs(),
        wall: WallClock::now(),
    }
}

fn monotonic_secs() -> u64 {
    #[cfg(unix)]
    {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        #[allow(clippy::cast_sign_loss)]
        let secs = ts.tv_sec as u64;
        secs
    }

    #[cfg(not(unix))]
    {
        // On Windows, use Instant as approximate monotonic.
        use std::sync::OnceLock;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_secs()
    }
}

fn reduce(
    state: &mut State,
    event: Event,
    config: &CratonConfig,
    graph: &DepGraph,
    ctx: &Ctx,
) -> Vec<Command> {
    reduce::reduce(state, event, config, graph, ctx)
}