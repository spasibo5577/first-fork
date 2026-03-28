//! Control loop — the central event processor.
//!
//! Owns all mutable state. Receives events from scheduler, signal handler,
//! HTTP API, and effect completions. Calls the reducer for each event,
//! then dispatches commands.

use crate::config::CratonConfig;
use crate::effect;
use crate::graph::DepGraph;
use crate::http::SharedSnapshot;
use crate::model::{
    Alert, AlertPriority, BackupPhase, CleanupLevel, Command, EffectResult, Event, ProbeResult,
    ServiceId, TaskKind,
};
use crate::notify::NotifySender;
use crate::reduce::{self, Ctx};
use crate::schedule::{self, Schedule, WallClock};
use crate::state::State;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Runs the main control loop. Blocks until shutdown.
#[allow(clippy::needless_pass_by_value)] // Arc/Sender/NotifySender are moved into this thread
pub fn run_control_loop(
    config: &CratonConfig,
    graph: &DepGraph,
    event_rx: mpsc::Receiver<Event>,
    event_tx: mpsc::Sender<Event>,
    snapshot: SharedSnapshot,
    notifier: NotifySender,
    sd_notify: &crate::effect::systemd::SdNotify,
) {
    let mono_start = monotonic_secs();
    let mut state = State::new(config, mono_start);

    // Crash recovery from persisted backup state.
    let persisted_phase = load_persisted_backup_phase();
    if !persisted_phase.is_idle() {
        eprintln!("[cratond] crash recovery needed: backup phase = {persisted_phase:?}");
        let event = Event::StartupRecovery {
            persisted_backup: persisted_phase,
        };
        let ctx = make_ctx();
        let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
        execute_commands(
            &cmds, config, graph, &notifier, &snapshot, &mut state, sd_notify, &event_tx,
        );
    }

    // Signal ready.
    sd_notify.ready();
    sd_notify.status("running");
    eprintln!("[cratond] control loop started");

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

    // Schedule state.
    let mut last_recovery = Instant::now();
    let mut last_disk = Instant::now();
    let recovery_interval = Duration::from_secs(300);
    let disk_interval = Duration::from_secs(6 * 3600);
    let startup_delay = Duration::from_secs(120);
    let start_instant = Instant::now();

    loop {
        let mut due_tasks = Vec::new();

        if start_instant.elapsed() >= startup_delay {
            if last_recovery.elapsed() >= recovery_interval {
                due_tasks.push(TaskKind::Recovery);
                last_recovery = Instant::now();
            }

            if last_disk.elapsed() >= disk_interval {
                due_tasks.push(TaskKind::DiskMonitor);
                last_disk = Instant::now();
            }

            let wall = WallClock::now();
            check_daily_schedules(&mut due_tasks, &mut state, &wall);
        }

        if !due_tasks.is_empty() {
            let event = Event::Tick { due_tasks };
            let ctx = make_ctx();
            let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
            execute_commands(
                &cmds, config, graph, &notifier, &snapshot, &mut state, sd_notify, &event_tx,
            );
        }

        match event_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                let ctx = make_ctx();
                let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
                execute_commands(
                    &cmds, config, graph, &notifier, &snapshot, &mut state, sd_notify, &event_tx,
                );

                if state.shutting_down {
                    eprintln!("[cratond] shutdown initiated");
                    sd_notify.stopping();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[cratond] event channel disconnected — shutting down");
                break;
            }
        }
    }

    eprintln!("[cratond] control loop exiting");
    sd_notify.status("shutting down");
    publish_snapshot(&state, &snapshot);
}

// ─── Command execution ────────────────────────────────────────

/// Dispatches commands emitted by the reducer.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn execute_commands(
    cmds: &[Command],
    config: &CratonConfig,
    graph: &DepGraph,
    notifier: &NotifySender,
    snapshot: &SharedSnapshot,
    state: &mut State,
    sd_notify: &crate::effect::systemd::SdNotify,
    event_tx: &mpsc::Sender<Event>,
) {
    for cmd in cmds {
        match cmd {
            Command::RunProbes(service_ids) => {
                exec_run_probes(
                    service_ids,
                    config,
                    graph,
                    state,
                    notifier,
                    snapshot,
                    sd_notify,
                    event_tx,
                );
            }
            Command::RestartService { id, unit, reason } => {
                exec_service_action("restart", id, unit, reason);
            }
            Command::StopService { id, unit, reason } => {
                exec_service_action("stop", id, unit, reason);
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
                exec_docker_restart(reason);
            }
            Command::SendAlert(alert) => {
                notifier.queue(alert.clone());
            }
            Command::PersistBackupState(phase) => {
                exec_persist_backup(phase);
            }
            Command::PublishSnapshot => {
                publish_snapshot(state, snapshot);
            }
            Command::NotifyWatchdog => {
                sd_notify.watchdog();
            }
            Command::Shutdown { grace_secs } => {
                eprintln!("[cratond] shutdown command, grace={grace_secs}s");
                state.shutting_down = true;
            }
            Command::ResticUnlock => {
                exec_restic_unlock(config);
            }
            Command::UpdateLlmContext => {
                exec_update_llm_context(state, config);
            }
            Command::WriteIncident(report) => {
                eprintln!(
                    "[cratond] incident: {:?} service={:?}",
                    report.kind, report.service
                );
                let report = report.clone();
                std::thread::Builder::new()
                    .name("incident-writer".into())
                    .spawn(move || effect::incident::write_report(&report))
                    .ok();
            }
            Command::CheckDiskUsage => {
                let mono = monotonic_secs();
                if let Some(sample) = effect::disk::get_usage("/", mono) {
                    state.disk_usage_percent = Some(sample.usage_percent);
                    state.disk_free_bytes = Some(sample.free_bytes);
                    state.disk_samples.push(sample);
                }
            }
            Command::RunDiskCleanup { level } => {
                let level = *level;
                std::thread::Builder::new()
                    .name("disk-cleanup".into())
                    .spawn(move || match level {
                        CleanupLevel::Standard => effect::disk::cleanup_standard(),
                        CleanupLevel::Aggressive => effect::disk::cleanup_aggressive(),
                    })
                    .ok();
            }
            Command::CheckAptUpdates => {
                std::thread::Builder::new()
                    .name("apt-check".into())
                    .spawn(|| {
                        if let Some(r) = effect::updates::check_apt() {
                            eprintln!(
                                "[cratond] APT: {} upgradeable, {} security",
                                r.upgradeable, r.security
                            );
                        }
                    })
                    .ok();
            }
            Command::CheckDockerUpdates => {
                eprintln!("[cratond] docker update check — no image list configured");
            }
            Command::ResticBackup { paths } => {
                let cmd_id = state.alloc_cmd_id();
                state.backup_pending_cmd = Some(cmd_id);
                let tx = event_tx.clone();
                let binary = config.backup.restic_binary.clone();
                let repo = config.backup.restic_repo.clone();
                let pass_file = config.backup.restic_password_file.clone();
                let paths = paths.clone();
                std::thread::Builder::new()
                    .name("restic-backup".into())
                    .spawn(move || {
                        let mut argv_owned: Vec<String> = vec![
                            binary,
                            "backup".into(),
                            "--repo".into(),
                            repo,
                            "--password-file".into(),
                            pass_file,
                        ];
                        for p in paths {
                            argv_owned.push(p);
                        }
                        let argv: Vec<&str> = argv_owned.iter().map(String::as_str).collect();
                        let r = effect::exec::run(&argv, Duration::from_secs(3600));
                        let _ = tx.send(Event::EffectCompleted {
                            cmd_id,
                            result: exec_to_effect_result(r),
                        });
                    })
                    .ok();
            }
            Command::ResticForget {
                daily,
                weekly,
                monthly,
            } => {
                let cmd_id = state.alloc_cmd_id();
                state.backup_pending_cmd = Some(cmd_id);
                let tx = event_tx.clone();
                let binary = config.backup.restic_binary.clone();
                let repo = config.backup.restic_repo.clone();
                let pass_file = config.backup.restic_password_file.clone();
                let daily = *daily;
                let weekly = *weekly;
                let monthly = *monthly;
                std::thread::Builder::new()
                    .name("restic-forget".into())
                    .spawn(move || {
                        let daily_s = daily.to_string();
                        let weekly_s = weekly.to_string();
                        let monthly_s = monthly.to_string();
                        let argv: Vec<&str> = vec![
                            binary.as_str(),
                            "forget",
                            "--prune",
                            "--repo",
                            repo.as_str(),
                            "--password-file",
                            pass_file.as_str(),
                            "--keep-daily",
                            daily_s.as_str(),
                            "--keep-weekly",
                            weekly_s.as_str(),
                            "--keep-monthly",
                            monthly_s.as_str(),
                        ];
                        let r = effect::exec::run(&argv, Duration::from_secs(600));
                        let _ = tx.send(Event::EffectCompleted {
                            cmd_id,
                            result: exec_to_effect_result(r),
                        });
                    })
                    .ok();
            }
            Command::ResticCheck { subset_percent } => {
                let cmd_id = state.alloc_cmd_id();
                state.backup_pending_cmd = Some(cmd_id);
                let tx = event_tx.clone();
                let binary = config.backup.restic_binary.clone();
                let repo = config.backup.restic_repo.clone();
                let pass_file = config.backup.restic_password_file.clone();
                let pct = *subset_percent;
                std::thread::Builder::new()
                    .name("restic-check".into())
                    .spawn(move || {
                        let pct_s = format!("{pct}%");
                        let argv: Vec<&str> = vec![
                            binary.as_str(),
                            "check",
                            "--repo",
                            repo.as_str(),
                            "--password-file",
                            pass_file.as_str(),
                            "--read-data-subset",
                            pct_s.as_str(),
                        ];
                        let r = effect::exec::run(&argv, Duration::from_secs(1800));
                        let _ = tx.send(Event::EffectCompleted {
                            cmd_id,
                            result: exec_to_effect_result(r),
                        });
                    })
                    .ok();
            }
            Command::RunBackupPhase { .. } => {
                // FSM advancement handled via EffectCompleted events.
            }
            Command::PersistMaintenance => {
                exec_persist_maintenance(state);
            }
            Command::TriggerPicoClaw {
                event_type,
                details,
            } => {
                if !config.ai.picoclaw_url.is_empty() {
                    effect::aibridge::trigger(&config.ai.picoclaw_url, event_type, details);
                }
            }
            Command::AcquireLease { resource, holder } => {
                let mono = monotonic_secs();
                let res = state.leases.acquire(resource.clone(), holder, mono);
                eprintln!("[cratond] lease {resource:?} → {holder}: {res:?}");
            }
            Command::ReleaseLease { resource } => {
                state.leases.force_release(resource);
            }
        }
    }
}

// ─── Individual command executors ──────────────────────────────

#[allow(clippy::too_many_arguments)]
fn exec_run_probes(
    service_ids: &[ServiceId],
    config: &CratonConfig,
    graph: &DepGraph,
    state: &mut State,
    notifier: &NotifySender,
    snapshot: &SharedSnapshot,
    sd_notify: &crate::effect::systemd::SdNotify,
    event_tx: &mpsc::Sender<Event>,
) {
    let results = run_probes(service_ids, config);
    let ctx = make_ctx();
    let sub_cmds = reduce::reduce(state, Event::ProbeResults(results), config, graph, &ctx);
    execute_commands(
        &sub_cmds, config, graph, notifier, snapshot, state, sd_notify, event_tx,
    );
}

fn exec_service_action(action: &str, id: &ServiceId, unit: &str, reason: &str) {
    eprintln!("[cratond] {action} {id} ({unit}): {reason}");
    let _ =
        effect::exec::run_dry_aware(&["systemctl", action, unit], Duration::from_secs(30), false);
}

fn exec_docker_restart(reason: &str) {
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

fn exec_persist_backup(phase: &BackupPhase) {
    let data = serde_json::to_vec_pretty(phase).unwrap_or_else(|_| b"{}".to_vec());
    if let Err(e) = crate::persist::atomic_write(
        std::path::Path::new("/var/lib/craton/backup-state.json"),
        &data,
    ) {
        eprintln!("[cratond] failed to persist backup state: {e}");
    }
}

fn exec_restic_unlock(config: &CratonConfig) {
    eprintln!("[cratond] running restic unlock");
    let _ = effect::exec::run(
        &[
            &config.backup.restic_binary,
            "unlock",
            "--repo",
            &config.backup.restic_repo,
            "--password-file",
            &config.backup.restic_password_file,
        ],
        Duration::from_secs(120),
    );
}

fn exec_update_llm_context(state: &State, config: &CratonConfig) {
    let snap = build_snapshot_json(state);
    if let Err(e) = crate::persist::atomic_write(
        std::path::Path::new(&config.ai.context_path),
        snap.as_bytes(),
    ) {
        eprintln!("[cratond] failed to write LLM context: {e}");
    }
}

// ─── Probes ────────────────────────────────────────────────────

fn run_probes(service_ids: &[ServiceId], config: &CratonConfig) -> Vec<ProbeResult> {
    std::thread::scope(|s| {
        let handles: Vec<_> = service_ids
            .iter()
            .filter_map(|sid| {
                let svc = config.find_service(sid)?;
                Some(s.spawn(move || effect::probe::run_probe(&svc.id, &svc.probe, &svc.unit)))
            })
            .collect();

        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    })
}

// ─── Snapshot ──────────────────────────────────────────────────

fn publish_snapshot(state: &State, snapshot: &SharedSnapshot) {
    let json = build_snapshot_json(state);
    if let Ok(mut s) = snapshot.lock() {
        *s = json;
    }
}

fn build_snapshot_json(state: &State) -> String {
    let mut services = serde_json::Map::new();
    for (id, svc) in &state.services {
        let status_json = serde_json::to_value(&svc.status).unwrap_or_default();
        services.insert(id.as_str().to_string(), status_json);
    }

    serde_json::json!({
        "services": services,
        "backup_phase": serde_json::to_value(&state.backup_phase).unwrap_or_default(),
        "disk_usage_percent": state.disk_usage_percent,
        "shutting_down": state.shutting_down,
        "backup_history": state.backup_history.to_vec(),
        "recovery_history": state.recovery_history.to_vec(),
    })
    .to_string()
}

// ─── Persistence helpers ───────────────────────────────────────

fn load_persisted_backup_phase() -> BackupPhase {
    let path = std::path::Path::new("/var/lib/craton/backup-state.json");
    match crate::persist::read_optional(path) {
        Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or(BackupPhase::Idle),
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

// ─── Effect helpers ────────────────────────────────────────────

fn exec_to_effect_result(
    r: Result<effect::exec::ExecResult, effect::exec::ExecError>,
) -> EffectResult {
    match r {
        Ok(r) if r.killed => EffectResult::Killed {
            signal: r.exit_code,
            #[allow(clippy::cast_possible_truncation)]
            duration_ms: r.duration.as_millis() as u64,
        },
        Ok(r) if r.exit_code == 0 => EffectResult::Success {
            stdout: r.stdout_text(),
            stderr: r.stderr_text(),
            #[allow(clippy::cast_possible_truncation)]
            duration_ms: r.duration.as_millis() as u64,
        },
        Ok(r) => EffectResult::Failed {
            exit_code: r.exit_code,
            stdout: r.stdout_text(),
            stderr: r.stderr_text(),
            #[allow(clippy::cast_possible_truncation)]
            duration_ms: r.duration.as_millis() as u64,
        },
        Err(e) => EffectResult::HelperError {
            message: e.to_string(),
        },
    }
}

fn exec_persist_maintenance(state: &State) {
    // Collect non-expired maintenance entries keyed by service id.
    let entries: std::collections::BTreeMap<String, &crate::state::Maintenance> = state
        .services
        .iter()
        .filter_map(|(id, s)| s.maintenance.as_ref().map(|m| (id.as_str().to_string(), m)))
        .collect();

    match serde_json::to_vec_pretty(&entries) {
        Ok(data) => {
            if let Err(e) = crate::persist::atomic_write(
                std::path::Path::new("/var/lib/craton/maintenance.json"),
                &data,
            ) {
                eprintln!("[cratond] failed to persist maintenance: {e}");
            }
        }
        Err(e) => eprintln!("[cratond] failed to serialize maintenance: {e}"),
    }
}

// ─── Schedule helpers ──────────────────────────────────────────

fn check_daily_schedules(due: &mut Vec<TaskKind>, state: &mut State, wall: &WallClock) {
    if schedule::is_due(
        &Schedule::OddDays { hour: 4, minute: 0 },
        *wall,
        state.last_backup_day,
    ) {
        due.push(TaskKind::Backup);
    }

    if schedule::is_due(
        &Schedule::Daily { hour: 9, minute: 0 },
        *wall,
        state.last_apt_day,
    ) {
        due.push(TaskKind::AptUpdates);
    }

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

    if schedule::is_due(
        &Schedule::Daily { hour: 9, minute: 5 },
        *wall,
        state.last_summary_day,
    ) {
        due.push(TaskKind::DailySummary);
    }
}

// ─── Time helpers ──────────────────────────────────────────────

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
        use std::sync::OnceLock;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_secs()
    }
}
