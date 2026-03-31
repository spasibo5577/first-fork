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
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub struct RuntimeDeps<'a> {
    pub config: &'a CratonConfig,
    pub graph: &'a DepGraph,
    pub snapshot: SharedSnapshot,
    pub notifier: NotifySender,
    pub sd_notify: &'a crate::effect::systemd::SdNotify,
    pub outbox_overflow: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupKind {
    FirstStart,
    DaemonRestart,
    HostBoot,
    Unknown,
}

/// Runs the main control loop. Blocks until shutdown.
#[allow(clippy::needless_pass_by_value)] // Arc/Sender/NotifySender are moved into this thread
#[allow(clippy::too_many_lines)] // Central loop intentionally stays explicit until a larger runtime refactor is warranted.
pub fn run_control_loop(
    deps: RuntimeDeps<'_>,
    event_rx: mpsc::Receiver<Event>,
    event_tx: mpsc::Sender<Event>,
) {
    let RuntimeDeps {
        config,
        graph,
        snapshot,
        notifier,
        sd_notify,
        outbox_overflow,
    } = deps;
    let mono_start = monotonic_secs();
    let mut state = State::new(config, mono_start);
    let startup_kind = detect_startup_kind(
        Path::new("/proc/sys/kernel/random/boot_id"),
        Path::new("/var/lib/craton/last-boot-id"),
    );

    // Load persisted maintenance entries (drop expired ones).
    load_persisted_maintenance(&mut state, mono_start);

    // Crash recovery from persisted backup state.
    let persisted_phase = load_persisted_backup_phase();
    if !persisted_phase.is_idle() {
        crate::log::warn(
            "runtime",
            &format!("crash recovery needed: backup phase = {persisted_phase:?}"),
        );
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
    emit_startup_signal(startup_kind, config.services.len(), &notifier);

    // Schedule state.
    let mut last_recovery = Instant::now();
    let mut last_disk = Instant::now();
    let mut last_watchdog = Instant::now();
    let recovery_interval = Duration::from_secs(300);
    let disk_interval = Duration::from_secs(6 * 3600);
    let watchdog_interval = effect::systemd::watchdog_interval().unwrap_or(Duration::from_secs(10));
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

        // Publish snapshot every loop iteration so heartbeat stays fresh.
        publish_snapshot(&state, &snapshot, &outbox_overflow);

        match event_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(event) => {
                let ctx = make_ctx();
                let cmds = reduce::reduce(&mut state, event, config, graph, &ctx);
                execute_commands(
                    &cmds, config, graph, &notifier, &snapshot, &mut state, sd_notify, &event_tx,
                );

                if state.shutting_down {
                    crate::log::info("runtime", "shutdown initiated");
                    sd_notify.stopping();
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                crate::log::warn("runtime", "event channel disconnected, shutting down");
                break;
            }
        }

        if should_send_watchdog(state.shutting_down, last_watchdog.elapsed(), watchdog_interval) {
            sd_notify.watchdog();
            last_watchdog = Instant::now();
        }
    }

    crate::log::info("runtime", "control loop exiting");
    sd_notify.status("shutting down");
    publish_snapshot(&state, &snapshot, &outbox_overflow);
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
                // Full publish happens every loop iteration; this is a no-op placeholder.
            }
            Command::NotifyWatchdog => {
                sd_notify.watchdog();
            }
            Command::Shutdown { grace_secs } => {
                crate::log::info("runtime", &format!("shutdown command received, grace={grace_secs}s"));
                state.shutting_down = true;
            }
            Command::ResticUnlock => {
                exec_restic_unlock(config);
            }
            Command::UpdateLlmContext => {
                exec_update_llm_context(state, config);
            }
            Command::WriteIncident(report) => {
                crate::log::warn(
                    "incident",
                    &format!("incident queued: {:?} service={:?}", report.kind, report.service),
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
                    let _ = event_tx.send(Event::DiskSample(sample));
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
    let snap = build_snapshot_json(state, &Arc::new(AtomicBool::new(false)));
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

fn publish_snapshot(state: &State, snapshot: &SharedSnapshot, outbox_overflow: &Arc<AtomicBool>) {
    let json = build_snapshot_json(state, outbox_overflow);
    if let Ok(mut s) = snapshot.lock() {
        *s = json;
    }
}

fn build_snapshot_json(state: &State, outbox_overflow: &Arc<AtomicBool>) -> String {
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
        "remediation_history": state.remediation_log.to_vec(),
        "snapshot_epoch_secs": epoch_secs_now(),
        "last_recovery_mono": state.last_recovery_mono,
        "start_mono": state.start_mono,
        "outbox_overflow": outbox_overflow.load(Ordering::Relaxed),
    })
    .to_string()
}

// ─── Persistence helpers ───────────────────────────────────────

fn load_persisted_maintenance(state: &mut State, now_mono: u64) {
    let path = std::path::Path::new("/var/lib/craton/maintenance.json");
    let data = match crate::persist::read_optional(path) {
        Ok(Some(d)) => d,
        Ok(None) => return,
        Err(e) => {
            eprintln!("[cratond] failed to read maintenance.json: {e}");
            return;
        }
    };

    let entries: std::collections::BTreeMap<String, crate::state::Maintenance> =
        match serde_json::from_slice(&data) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[cratond] failed to parse maintenance.json: {e}");
                return;
            }
        };

    let mut loaded = 0usize;
    let mut expired = 0usize;
    for (id_str, maint) in entries {
        // Drop entries that already expired.
        if maint.until_mono <= now_mono {
            expired += 1;
            continue;
        }
        let sid = crate::model::ServiceId(id_str);
        if let Some(svc) = state.services.get_mut(&sid) {
            svc.maintenance = Some(maint);
            loaded += 1;
        }
    }
    eprintln!("[cratond] maintenance loaded: {loaded} active, {expired} expired");
}

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

fn detect_startup_kind(current_boot_path: &Path, persisted_boot_path: &Path) -> StartupKind {
    let current_boot_id = match read_trimmed_file(current_boot_path) {
        Ok(Some(value)) => value,
        Ok(None) => {
            crate::log::warn("startup", "boot id file is empty");
            return StartupKind::Unknown;
        }
        Err(e) => {
            crate::log::warn("startup", &format!("failed to read boot id: {e}"));
            return StartupKind::Unknown;
        }
    };

    let previous_boot_id = match crate::persist::read_optional(persisted_boot_path) {
        Ok(Some(data)) => String::from_utf8(data)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        Ok(None) => None,
        Err(e) => {
            crate::log::warn("startup", &format!("failed to read last boot id: {e}"));
            None
        }
    };

    let kind = classify_startup_kind(Some(current_boot_id.as_str()), previous_boot_id.as_deref());

    if let Err(e) = crate::persist::atomic_write(persisted_boot_path, current_boot_id.as_bytes()) {
        crate::log::warn("startup", &format!("failed to persist last boot id: {e}"));
    } else {
        crate::log::info(
            "startup",
            &format!("boot session recorded ({})", kind.label()),
        );
    }

    kind
}

fn emit_startup_signal(startup_kind: StartupKind, service_count: usize, notifier: &NotifySender) {
    crate::log::info(
        "runtime",
        &format!("control loop started ({})", startup_kind.label()),
    );

    notifier.queue(Alert {
        title: startup_kind.alert_title().into(),
        body: format!(
            "{}\nСервисов под наблюдением: {}",
            startup_kind.alert_body(),
            service_count
        ),
        priority: AlertPriority::Default,
        tags: "white_check_mark".into(),
    });
}

fn read_trimmed_file(path: &Path) -> Result<Option<String>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

fn classify_startup_kind(current_boot_id: Option<&str>, previous_boot_id: Option<&str>) -> StartupKind {
    match (current_boot_id, previous_boot_id) {
        (None, _) => StartupKind::Unknown,
        (Some(_), None) => StartupKind::FirstStart,
        (Some(current), Some(previous)) if current == previous => StartupKind::DaemonRestart,
        (Some(_), Some(_)) => StartupKind::HostBoot,
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
        epoch_secs: epoch_secs_now(),
        wall: WallClock::now(),
    }
}

fn should_send_watchdog(
    shutting_down: bool,
    elapsed: Duration,
    watchdog_interval: Duration,
) -> bool {
    !shutting_down && elapsed >= watchdog_interval
}

impl StartupKind {
    const fn label(self) -> &'static str {
        match self {
            Self::FirstStart => "first_start",
            Self::DaemonRestart => "daemon_restart",
            Self::HostBoot => "host_boot",
            Self::Unknown => "unknown",
        }
    }

    const fn alert_title(self) -> &'static str {
        match self {
            Self::FirstStart | Self::Unknown => "🟢 Кратон запущен",
            Self::DaemonRestart => "🟢 Кратон перезапущен",
            Self::HostBoot => "🟢 Кратон запущен после загрузки хоста",
        }
    }

    const fn alert_body(self) -> &'static str {
        match self {
            Self::FirstStart => "Первый запуск или отсутствует сохранённый boot id.",
            Self::DaemonRestart => "Перезапуск демона в рамках текущей загрузки хоста.",
            Self::HostBoot => "Обнаружена новая загрузка хоста.",
            Self::Unknown => "Не удалось надёжно определить тип запуска.",
        }
    }
}

fn epoch_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn watchdog_sends_when_interval_elapsed() {
        assert!(should_send_watchdog(
            false,
            Duration::from_secs(10),
            Duration::from_secs(10)
        ));
        assert!(should_send_watchdog(
            false,
            Duration::from_secs(11),
            Duration::from_secs(10)
        ));
    }

    #[test]
    fn watchdog_does_not_send_before_interval_or_during_shutdown() {
        assert!(!should_send_watchdog(
            false,
            Duration::from_secs(9),
            Duration::from_secs(10)
        ));
        assert!(!should_send_watchdog(
            true,
            Duration::from_secs(10),
            Duration::from_secs(10)
        ));
    }

    #[test]
    fn classify_startup_kind_distinguishes_first_restart_and_new_boot() {
        assert_eq!(classify_startup_kind(Some("boot-a"), None), StartupKind::FirstStart);
        assert_eq!(
            classify_startup_kind(Some("boot-a"), Some("boot-a")),
            StartupKind::DaemonRestart
        );
        assert_eq!(
            classify_startup_kind(Some("boot-b"), Some("boot-a")),
            StartupKind::HostBoot
        );
    }

    #[test]
    fn detect_startup_kind_persists_current_boot_id() {
        let dir = std::env::temp_dir().join(format!(
            "craton_runtime_boot_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create test dir: {e}"));

        let current_path = dir.join("boot_id");
        let persisted_path = dir.join("last_boot_id");
        fs::write(&current_path, "boot-1\n").unwrap_or_else(|e| panic!("write current boot: {e}"));

        let first = detect_startup_kind(&current_path, &persisted_path);
        assert_eq!(first, StartupKind::FirstStart);
        assert_eq!(
            fs::read_to_string(&persisted_path)
                .unwrap_or_else(|e| panic!("read persisted boot id: {e}")),
            "boot-1"
        );

        let second = detect_startup_kind(&current_path, &persisted_path);
        assert_eq!(second, StartupKind::DaemonRestart);

        fs::write(&current_path, "boot-2\n").unwrap_or_else(|e| panic!("rewrite current boot: {e}"));
        let third = detect_startup_kind(&current_path, &persisted_path);
        assert_eq!(third, StartupKind::HostBoot);

        let _ = fs::remove_dir_all(&dir);
    }
}
