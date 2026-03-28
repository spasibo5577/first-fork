//! Top-level reducer: `(State, Event) → Vec<Command>`.
//!
//! The single point where all state mutations happen.
//! Pure logic — no I/O, no exec, no network.

use crate::config::CratonConfig;
use crate::graph::DepGraph;
use crate::model::{
    Alert, AlertPriority, BackupPhase, Command, CommandRequest, EffectResult, Event, IncidentKind,
    IncidentReport, RemediationAction, ResourceId, ServiceId, SignalKind, TaskKind,
};
use crate::policy::{backup, recovery};
use crate::schedule::WallClock;
use crate::state::{
    BackupRecord, RecoveryRecord, RemediationRecord, RestartRecord, ServiceStatus, State,
};
use std::collections::BTreeMap;

/// Context provided by the runtime to the reducer on each call.
pub struct Ctx {
    pub mono_secs: u64,
    /// Unix epoch seconds (wall clock). Use for timestamps in incidents/records.
    pub epoch_secs: u64,
    pub wall: WallClock,
}

/// The core reducer. Called for every event. Returns commands to execute.
pub fn reduce(
    state: &mut State,
    event: Event,
    config: &CratonConfig,
    graph: &DepGraph,
    ctx: &Ctx,
) -> Vec<Command> {
    if state.shutting_down {
        return vec![];
    }

    match event {
        Event::Tick { due_tasks } => handle_tick(state, &due_tasks, config, ctx),
        Event::ProbeResults(results) => handle_probes(state, &results, config, graph, ctx),
        Event::EffectCompleted { cmd_id, result } => {
            handle_effect_completed(state, cmd_id, &result, config, ctx)
        }
        Event::HttpCommand(req) => handle_http_command(state, req, config, ctx),
        Event::Signal(SignalKind::Shutdown) => handle_shutdown(state),
        Event::Signal(SignalKind::Reload) => vec![],
        Event::StartupRecovery { persisted_backup } => {
            handle_startup_recovery(state, &persisted_backup)
        }
    }
}

// ─── Tick ──────────────────────────────────────────────────────

fn handle_tick(
    state: &mut State,
    due_tasks: &[TaskKind],
    config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    let mut cmds = Vec::new();

    for task in due_tasks {
        match task {
            TaskKind::Recovery => {
                state.last_recovery_mono = Some(ctx.mono_secs);
                let all_ids: Vec<ServiceId> = state.services.keys().cloned().collect();
                cmds.push(Command::RunProbes(all_ids));
            }
            TaskKind::Backup => {
                if backup::should_start(
                    &config.backup.schedule,
                    ctx.wall,
                    state.last_backup_day,
                    &state.backup_phase,
                ) {
                    cmds.extend(start_backup(state, config, ctx));
                }
            }
            TaskKind::DiskMonitor => {
                state.last_disk_mono = Some(ctx.mono_secs);
                cmds.push(Command::CheckDiskUsage);
                cmds.extend(evaluate_disk(state, config));
            }
            TaskKind::AptUpdates => {
                state.last_apt_day = Some(ctx.wall.day);
                cmds.push(Command::CheckAptUpdates);
            }
            TaskKind::DockerUpdates => {
                state.last_docker_day = Some(ctx.wall.day);
                cmds.push(Command::CheckDockerUpdates);
            }
            TaskKind::DailySummary => {
                state.last_summary_day = Some(ctx.wall.day);
                cmds.extend(build_daily_summary(state, config));
            }
        }
    }

    cmds
}

// ─── Probes ────────────────────────────────────────────────────

fn handle_probes(
    state: &mut State,
    results: &[crate::model::ProbeResult],
    config: &CratonConfig,
    graph: &DepGraph,
    ctx: &Ctx,
) -> Vec<Command> {
    let plan = recovery::evaluate(results, &state.services, config, graph, ctx.mono_secs);

    let mut cmds = Vec::new();
    let mut recovered = Vec::new();
    let mut failed = Vec::new();

    apply_decisions(
        state,
        &plan,
        config,
        ctx,
        &mut cmds,
        &mut recovered,
        &mut failed,
    );
    emit_alerts(&plan, &recovered, &failed, &mut cmds);

    state.recovery_history.push(RecoveryRecord {
        mono: ctx.mono_secs,
        recovered,
        failed,
        docker_restarted: plan.docker_restart_needed,
        duration_ms: 0,
    });

    cmds.push(Command::UpdateLlmContext);
    cmds.push(Command::NotifyWatchdog);

    cmds
}

fn apply_decisions(
    state: &mut State,
    plan: &recovery::RecoveryPlan,
    config: &CratonConfig,
    ctx: &Ctx,
    cmds: &mut Vec<Command>,
    recovered: &mut Vec<ServiceId>,
    failed: &mut Vec<ServiceId>,
) {
    for (sid, decision) in &plan.decisions {
        let Some(svc) = state.services.get_mut(sid) else {
            continue;
        };
        let Some(svc_config) = config.find_service(sid) else {
            continue;
        };

        match decision {
            recovery::Decision::Healthy => {
                if !svc.status.is_healthy() {
                    recovered.push(sid.clone());
                }
                svc.status = ServiceStatus::Healthy {
                    since_mono: ctx.mono_secs,
                };
                svc.breaker = crate::breaker::on_healthy_probe(&svc.breaker);
            }
            recovery::Decision::Restart {
                unit,
                attempt,
                severity: _,
            } => {
                // Lease check: don't restart if resource is held.
                if !state
                    .leases
                    .is_free(&ResourceId::Service(sid.clone()), ctx.mono_secs)
                {
                    continue;
                }

                svc.status = ServiceStatus::Recovering {
                    attempt: *attempt,
                    since_mono: ctx.mono_secs,
                };
                svc.last_restart_mono = Some(ctx.mono_secs);
                svc.restart_history.push(RestartRecord {
                    mono: ctx.mono_secs,
                    attempt: *attempt,
                    success: false,
                });

                let restarts =
                    svc.restarts_in_window(ctx.mono_secs, svc_config.breaker_window_secs);
                svc.breaker = crate::breaker::record_restart(
                    &svc.breaker,
                    restarts,
                    svc_config.max_restarts,
                    svc_config.breaker_cooldown_secs,
                    ctx.mono_secs,
                );

                cmds.push(Command::RestartService {
                    id: sid.clone(),
                    unit: unit.clone(),
                    reason: format!("unhealthy, attempt {attempt}"),
                });
            }
            recovery::Decision::Failed { error } => {
                svc.status = ServiceStatus::Failed {
                    since_mono: ctx.mono_secs,
                    error: error.clone(),
                };
                failed.push(sid.clone());

                if svc_config.severity.generates_incident() {
                    cmds.push(Command::WriteIncident(IncidentReport {
                        kind: IncidentKind::ServiceUnrecoverable,
                        service: Some(sid.clone()),
                        timestamp_epoch_secs: ctx.epoch_secs,
                        details: BTreeMap::from([("error".into(), error.clone())]),
                    }));
                }
            }
            recovery::Decision::BlockedByDependency { root } => {
                svc.status = ServiceStatus::BlockedByDep { root: root.clone() };
            }
            recovery::Decision::BreakerOpen => {
                svc.breaker = crate::breaker::maybe_transition(&svc.breaker, ctx.mono_secs);
            }
            recovery::Decision::InMaintenance | recovery::Decision::DockerRootCause => {}
        }
    }

    if plan.docker_restart_needed {
        cmds.push(Command::RestartDockerDaemon {
            reason: "correlated Docker service failures".into(),
        });
    }
}

fn emit_alerts(
    plan: &recovery::RecoveryPlan,
    recovered: &[ServiceId],
    failed: &[ServiceId],
    cmds: &mut Vec<Command>,
) {
    if !failed.is_empty() {
        let (title, body) =
            recovery::coalesce_alert(&plan.root_causes, &plan.blocked, plan.docker_restart_needed);
        cmds.push(Command::SendAlert(Alert {
            title,
            body,
            priority: AlertPriority::Urgent,
            tags: "rotating_light".into(),
        }));
    }

    if !recovered.is_empty() {
        let names: Vec<&str> = recovered.iter().map(ServiceId::as_str).collect();
        cmds.push(Command::SendAlert(Alert {
            title: "🔄 Восстановлены".into(),
            body: format!("Автоматически восстановлены: {}", names.join(", ")),
            priority: AlertPriority::Default,
            tags: "white_check_mark".into(),
        }));
    }
}

// ─── Backup ────────────────────────────────────────────────────

fn start_backup(state: &mut State, config: &CratonConfig, ctx: &Ctx) -> Vec<Command> {
    // Lease check.
    if !state.leases.is_free(&ResourceId::BackupRepo, ctx.mono_secs) {
        return vec![];
    }

    let run_id = format!("backup-{}", ctx.mono_secs);

    state.backup_phase = BackupPhase::Locked {
        run_id: run_id.clone(),
    };
    state.last_backup_day = Some(ctx.wall.day);

    let mut cmds = vec![
        Command::AcquireLease {
            resource: ResourceId::BackupRepo,
            holder: run_id.clone(),
        },
        Command::PersistBackupState(state.backup_phase.clone()),
        Command::ResticUnlock,
    ];

    // Stop services that need to be offline during backup.
    let stop_svcs = config.backup_stop_services();
    let pre_backup_state: Vec<crate::model::ServiceSnapshot> = stop_svcs
        .iter()
        .map(|svc| {
            let is_healthy = state
                .services
                .get(&svc.id)
                .is_some_and(|s| s.status.is_healthy());
            crate::model::ServiceSnapshot {
                id: svc.id.clone(),
                was_running: is_healthy,
                unit: svc.unit.clone(),
            }
        })
        .collect();

    for svc in &stop_svcs {
        cmds.push(Command::AcquireLease {
            resource: ResourceId::Service(svc.id.clone()),
            holder: run_id.clone(),
        });
        cmds.push(Command::StopService {
            id: svc.id.clone(),
            unit: svc.unit.clone(),
            reason: "backup".into(),
        });
    }

    state.backup_phase = BackupPhase::ResticRunning {
        run_id: run_id.clone(),
        pre_backup_state: pre_backup_state.clone(),
    };

    cmds.push(Command::PersistBackupState(state.backup_phase.clone()));
    cmds.push(Command::ResticBackup {
        paths: config.backup.paths.clone(),
    });

    cmds
}

// ─── Effect completed ──────────────────────────────────────────

fn handle_effect_completed(
    state: &mut State,
    cmd_id: u64,
    result: &EffectResult,
    config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    // Check if this is a backup-related completion.
    if state.backup_pending_cmd == Some(cmd_id) {
        state.backup_pending_cmd = None;
        return handle_backup_effect(state, result, config, ctx);
    }

    vec![Command::NotifyWatchdog]
}

fn handle_backup_effect(
    state: &mut State,
    result: &EffectResult,
    config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    let mut cmds = Vec::new();
    let success = matches!(result, EffectResult::Success { .. });

    match &state.backup_phase {
        BackupPhase::ResticRunning {
            run_id,
            pre_backup_state,
        } => {
            if success {
                state.consecutive_backup_failures = 0;

                // Start services back.
                let run_id = run_id.clone();
                let pre = pre_backup_state.clone();

                for snap in &pre {
                    if snap.was_running {
                        cmds.push(Command::StartService {
                            id: snap.id.clone(),
                            unit: snap.unit.clone(),
                        });
                        cmds.push(Command::ReleaseLease {
                            resource: ResourceId::Service(snap.id.clone()),
                        });
                    }
                }
                state.backup_phase = BackupPhase::RetentionRunning {
                    run_id: run_id.clone(),
                };
                cmds.push(Command::PersistBackupState(state.backup_phase.clone()));
                cmds.push(Command::ResticForget {
                    daily: config.backup.retention.daily,
                    weekly: config.backup.retention.weekly,
                    monthly: config.backup.retention.monthly,
                });
            } else {
                cmds.extend(handle_backup_failure(state, result, config, ctx));
            }
        }
        BackupPhase::RetentionRunning { run_id } => {
            let run_id = run_id.clone();

            if success {
                if config.backup.verify {
                    state.backup_phase = BackupPhase::Verifying {
                        run_id: run_id.clone(),
                    };
                    cmds.push(Command::PersistBackupState(state.backup_phase.clone()));
                    cmds.push(Command::ResticCheck {
                        subset_percent: config.backup.verify_subset_percent,
                    });
                } else {
                    cmds.extend(finalize_backup(state, true, None, ctx));
                }
            } else {
                cmds.extend(handle_backup_failure(state, result, config, ctx));
            }
        }
        BackupPhase::Verifying { .. } => {
            let error = if success {
                None
            } else {
                Some(effect_error_message(result))
            };
            cmds.extend(finalize_backup(state, success, error, ctx));
        }
        _ => {}
    }

    cmds
}

fn handle_backup_failure(
    state: &mut State,
    result: &EffectResult,
    _config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    state.consecutive_backup_failures += 1;
    let error = effect_error_message(result);

    let mut cmds = Vec::new();

    // Restore services from pre_backup_state.
    if let Some(pre) = state.backup_phase.pre_backup_services() {
        let pre_owned: Vec<_> = pre.to_vec();
        for snap in &pre_owned {
            if snap.was_running {
                cmds.push(Command::StartService {
                    id: snap.id.clone(),
                    unit: snap.unit.clone(),
                });
                cmds.push(Command::ReleaseLease {
                    resource: ResourceId::Service(snap.id.clone()),
                });
            }
        }
    }

    cmds.extend(finalize_backup(state, false, Some(error), ctx));

    if state.consecutive_backup_failures >= 3 {
        cmds.push(Command::WriteIncident(IncidentReport {
            kind: IncidentKind::BackupFailed,
            service: None,
            timestamp_epoch_secs: ctx.epoch_secs,
            details: BTreeMap::from([(
                "consecutive_failures".into(),
                state.consecutive_backup_failures.to_string(),
            )]),
        }));
    }

    cmds
}

fn finalize_backup(
    state: &mut State,
    success: bool,
    error: Option<String>,
    ctx: &Ctx,
) -> Vec<Command> {
    state.backup_phase = BackupPhase::Idle;

    let alert = if success {
        Alert {
            title: "✅ Backup завершён".into(),
            body: "Резервное копирование выполнено успешно".into(),
            priority: AlertPriority::Default,
            tags: "white_check_mark".into(),
        }
    } else {
        Alert {
            title: "❌ Backup не удался".into(),
            body: format!(
                "Ошибка: {}\nПодряд неудач: {}",
                error.as_deref().unwrap_or("unknown"),
                state.consecutive_backup_failures
            ),
            priority: AlertPriority::High,
            tags: "x".into(),
        }
    };

    state.backup_history.push(BackupRecord {
        mono: ctx.mono_secs,
        success,
        partial: false,
        error,
        duration_secs: 0,
    });

    vec![
        Command::PersistBackupState(BackupPhase::Idle),
        Command::ReleaseLease {
            resource: ResourceId::BackupRepo,
        },
        Command::SendAlert(alert),
        Command::PublishSnapshot,
    ]
}

fn effect_error_message(result: &EffectResult) -> String {
    match result {
        EffectResult::Failed {
            exit_code, stderr, ..
        } => format!("exit {exit_code}: {stderr}"),
        EffectResult::Killed { signal, .. } => format!("killed by signal {signal}"),
        EffectResult::HelperError { message } => message.clone(),
        EffectResult::Success { .. } => "success".into(),
    }
}

// ─── HTTP commands ─────────────────────────────────────────────

fn handle_http_command(
    state: &mut State,
    req: CommandRequest,
    config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    match req {
        CommandRequest::Trigger(task) => handle_tick(state, &[task], config, ctx),
        CommandRequest::Remediate {
            action,
            target,
            source,
            reason,
        } => handle_remediation(
            state,
            &action,
            target.as_ref(),
            &source,
            &reason,
            config,
            ctx,
        ),
    }
}

fn remediation_rate_limited(
    state: &State,
    action: &RemediationAction,
    target: Option<&ServiceId>,
    now_mono: u64,
) -> bool {
    let (window_secs, max_count) = match action {
        RemediationAction::RestartService => (3600u64, 3usize),
        RemediationAction::DockerRestart => (3600, 1),
        RemediationAction::TriggerBackup => (86400, 1),
        RemediationAction::MarkMaintenance => (3600, 5),
        _ => return false,
    };

    let action_str = format!("{action:?}");
    let cutoff = now_mono.saturating_sub(window_secs);

    let count = state
        .remediation_log
        .iter()
        .filter(|r| {
            r.mono >= cutoff
                && r.action == action_str
                && r.target.as_ref().map(ServiceId::as_str) == target.map(ServiceId::as_str)
        })
        .count();

    count >= max_count
}

fn handle_remediation(
    state: &mut State,
    action: &RemediationAction,
    target: Option<&ServiceId>,
    source: &str,
    reason: &str,
    config: &CratonConfig,
    ctx: &Ctx,
) -> Vec<Command> {
    let mut cmds = Vec::new();

    if remediation_rate_limited(state, action, target, ctx.mono_secs) {
        record_remediation(state, action, target, source, "rejected: rate limited", ctx);
        return vec![];
    }

    let result_str = match action {
        RemediationAction::RestartService => {
            let Some(sid) = target else {
                record_remediation(state, action, target, source, "rejected: no target", ctx);
                return vec![];
            };
            let Some(svc_config) = config.find_service(sid) else {
                record_remediation(
                    state,
                    action,
                    target,
                    source,
                    "rejected: unknown service",
                    ctx,
                );
                return vec![];
            };
            cmds.push(Command::RestartService {
                id: sid.clone(),
                unit: svc_config.unit.clone(),
                reason: format!("remediation from {source}: {reason}"),
            });
            "executed"
        }
        RemediationAction::DockerRestart => {
            cmds.push(Command::RestartDockerDaemon {
                reason: format!("remediation from {source}: {reason}"),
            });
            "executed"
        }
        RemediationAction::ResticUnlock => {
            cmds.push(Command::ResticUnlock);
            "executed"
        }
        RemediationAction::TriggerBackup => {
            cmds.extend(handle_tick(state, &[TaskKind::Backup], config, ctx));
            "triggered"
        }
        RemediationAction::ClearBreaker => {
            let Some(sid) = target else {
                record_remediation(state, action, target, source, "rejected: no target", ctx);
                return vec![];
            };
            if let Some(svc) = state.services.get_mut(sid) {
                svc.breaker = crate::breaker::reset();
            }
            "executed"
        }
        RemediationAction::MarkMaintenance => {
            let Some(sid) = target else {
                record_remediation(state, action, target, source, "rejected: no target", ctx);
                return vec![];
            };
            if let Some(svc) = state.services.get_mut(sid) {
                svc.maintenance = Some(crate::state::Maintenance {
                    until_mono: ctx.mono_secs + 3600, // 1 hour default
                    reason: reason.to_string(),
                });
            }
            cmds.push(Command::PersistMaintenance);
            "executed"
        }
        RemediationAction::ClearMaintenance => {
            let Some(sid) = target else {
                record_remediation(state, action, target, source, "rejected: no target", ctx);
                return vec![];
            };
            if let Some(svc) = state.services.get_mut(sid) {
                svc.maintenance = None;
            }
            cmds.push(Command::PersistMaintenance);
            "executed"
        }
        RemediationAction::ClearFlapping => {
            let Some(sid) = target else {
                record_remediation(state, action, target, source, "rejected: no target", ctx);
                return vec![];
            };
            if let Some(svc) = state.services.get_mut(sid) {
                svc.breaker = crate::breaker::reset();
                svc.restart_history.clear();
            }
            "executed"
        }
        RemediationAction::RunDiskCleanup => {
            cmds.push(Command::RunDiskCleanup {
                level: crate::model::CleanupLevel::Standard,
            });
            "executed"
        }
    };

    record_remediation(state, action, target, source, result_str, ctx);
    cmds
}

fn record_remediation(
    state: &mut State,
    action: &RemediationAction,
    target: Option<&ServiceId>,
    source: &str,
    result: &str,
    ctx: &Ctx,
) {
    state.remediation_log.push(RemediationRecord {
        mono: ctx.mono_secs,
        action: format!("{action:?}"),
        target: target.cloned(),
        source: source.to_string(),
        result: result.to_string(),
        error: None,
    });
}

// ─── Disk evaluation ───────────────────────────────────────────

fn evaluate_disk(state: &State, config: &CratonConfig) -> Vec<Command> {
    let Some(usage) = state.disk_usage_percent else {
        return vec![];
    };

    let decision = crate::policy::disk::evaluate(
        usage,
        config.disk.warn_percent,
        config.disk.critical_percent,
    );

    let mut cmds = Vec::new();

    if let Some(level) = decision.cleanup_level() {
        cmds.push(Command::RunDiskCleanup { level });
    }

    match &decision {
        crate::policy::disk::DiskDecision::Warning { usage_percent } => {
            let free = state.disk_free_bytes.unwrap_or(0);
            cmds.push(Command::SendAlert(Alert {
                title: "⚠️ Диск заполняется".into(),
                body: format!(
                    "Использование: {usage_percent}%\nСвободно: {}\nЗапущена стандартная очистка",
                    crate::effect::disk::human_bytes(free)
                ),
                priority: AlertPriority::High,
                tags: "warning".into(),
            }));
        }
        crate::policy::disk::DiskDecision::Critical { usage_percent } => {
            let free = state.disk_free_bytes.unwrap_or(0);
            cmds.push(Command::SendAlert(Alert {
                title: "🔴 Диск критически заполнен".into(),
                body: format!(
                    "Использование: {usage_percent}%\nСвободно: {}\nЗапущена агрессивная очистка",
                    crate::effect::disk::human_bytes(free)
                ),
                priority: AlertPriority::Urgent,
                tags: "rotating_light".into(),
            }));
        }
        crate::policy::disk::DiskDecision::Ok => {}
    }

    cmds
}

// ─── Daily summary ─────────────────────────────────────────────

fn build_daily_summary(state: &State, config: &CratonConfig) -> Vec<Command> {
    use std::fmt::Write;

    let mut body = String::with_capacity(512);

    // Services status.
    let _ = writeln!(body, "📊 Сервисы:");
    for svc in &config.services {
        let status = state.services.get(&svc.id).map_or("❌", |s| {
            if s.status.is_healthy() {
                "✅"
            } else {
                "❌"
            }
        });
        let _ = writeln!(body, "  {status} {}", svc.name);
    }

    // Disk.
    if let Some(usage) = state.disk_usage_percent {
        let free = state.disk_free_bytes.unwrap_or(0);
        let _ = writeln!(
            body,
            "\n💾 Диск: {usage}% занято, {} свободно",
            crate::effect::disk::human_bytes(free)
        );
    }

    // Backup.
    let last_backup = state.backup_history.iter().last();
    match last_backup {
        Some(b) if b.success => {
            let _ = writeln!(body, "\n📦 Последний backup: ✅ успешно");
        }
        Some(b) => {
            let err = b.error.as_deref().unwrap_or("unknown");
            let _ = writeln!(body, "\n📦 Последний backup: ❌ {err}");
        }
        None => {
            let _ = writeln!(body, "\n📦 Backup: нет данных");
        }
    }

    vec![
        Command::SendAlert(Alert {
            title: "📋 Ежедневная сводка".into(),
            body,
            priority: AlertPriority::Min,
            tags: "memo".into(),
        }),
        Command::PublishSnapshot,
    ]
}

// ─── Shutdown ──────────────────────────────────────────────────

fn handle_shutdown(state: &mut State) -> Vec<Command> {
    state.shutting_down = true;
    vec![Command::Shutdown { grace_secs: 90 }]
}

// ─── Startup recovery ──────────────────────────────────────────

fn handle_startup_recovery(state: &mut State, persisted: &BackupPhase) -> Vec<Command> {
    let actions = backup::crash_compensation(persisted);
    let mut cmds = Vec::new();

    for action in actions {
        match action {
            backup::CompensationAction::ResticUnlock => {
                cmds.push(Command::ResticUnlock);
            }
            backup::CompensationAction::StartService { id, unit } => {
                cmds.push(Command::StartService { id, unit });
            }
            backup::CompensationAction::StartServiceByName { name } => {
                cmds.push(Command::StartService {
                    id: ServiceId(name.clone()),
                    unit: format!("{name}.service"),
                });
            }
            backup::CompensationAction::ResetToIdle => {
                state.backup_phase = BackupPhase::Idle;
                cmds.push(Command::PersistBackupState(BackupPhase::Idle));
            }
        }
    }

    if !cmds.is_empty() {
        cmds.push(Command::SendAlert(Alert {
            title: "⚠️ Crash recovery выполнен".into(),
            body: format!("Backup был прерван в фазе: {persisted:?}"),
            priority: AlertPriority::High,
            tags: "warning".into(),
        }));
    }

    cmds
}

// ─── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::CratonConfig;
    use crate::model::{ProbeError, ProbeResult};

    fn test_config() -> CratonConfig {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[[service]]
id = "unbound"
name = "Unbound"
unit = "unbound.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "systemd_active"

[[service]]
id = "ntfy"
name = "NTFY"
unit = "ntfy.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "http"
url = "http://localhost:8080/v1/health"
"#;
        CratonConfig::from_toml(toml).unwrap()
    }

    fn test_ctx(mono_secs: u64) -> Ctx {
        Ctx {
            mono_secs,
            epoch_secs: mono_secs + 1_700_000_000,
            wall: WallClock {
                year: 2025,
                month: 1,
                day: 15,
                hour: 9,
                minute: 0,
                second: 0,
                weekday: 2,
            },
        }
    }

    #[test]
    fn tick_recovery_emits_probes() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::Tick {
                due_tasks: vec![TaskKind::Recovery],
            },
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds.iter().any(|c| matches!(c, Command::RunProbes(_))));
    }

    #[test]
    fn probe_healthy_updates_state() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let probes = vec![
            ProbeResult::Healthy {
                service: ServiceId("unbound".into()),
                latency_ms: 5,
            },
            ProbeResult::Healthy {
                service: ServiceId("ntfy".into()),
                latency_ms: 3,
            },
        ];

        let _cmds = reduce(
            &mut state,
            Event::ProbeResults(probes),
            &config,
            &graph,
            &ctx,
        );

        assert!(state.services[&ServiceId("unbound".into())]
            .status
            .is_healthy());
        assert!(state.services[&ServiceId("ntfy".into())]
            .status
            .is_healthy());
    }

    #[test]
    fn probe_unhealthy_emits_restart() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let probes = vec![
            ProbeResult::Healthy {
                service: ServiceId("unbound".into()),
                latency_ms: 5,
            },
            ProbeResult::Unhealthy {
                service: ServiceId("ntfy".into()),
                error: ProbeError::ConnectionRefused,
                latency_ms: 5,
            },
        ];

        let cmds = reduce(
            &mut state,
            Event::ProbeResults(probes),
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::RestartService { id, .. } if id.as_str() == "ntfy")));
    }

    #[test]
    fn shutdown_sets_flag() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::Signal(SignalKind::Shutdown),
            &config,
            &graph,
            &ctx,
        );

        assert!(state.shutting_down);
        assert!(cmds.iter().any(|c| matches!(c, Command::Shutdown { .. })));
    }

    #[test]
    fn startup_recovery_from_restic_running() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(10);

        let persisted = BackupPhase::ResticRunning {
            run_id: "r1".into(),
            pre_backup_state: vec![crate::model::ServiceSnapshot {
                id: ServiceId("continuwuity".into()),
                was_running: true,
                unit: "continuwuity.service".into(),
            }],
        };

        let cmds = reduce(
            &mut state,
            Event::StartupRecovery {
                persisted_backup: persisted,
            },
            &config,
            &graph,
            &ctx,
        );

        assert!(state.backup_phase.is_idle());
        assert!(cmds.iter().any(|c| matches!(c, Command::ResticUnlock)));
        assert!(cmds.iter().any(
            |c| matches!(c, Command::StartService { id, .. } if id.as_str() == "continuwuity")
        ));
        assert!(cmds.iter().any(|c| matches!(c, Command::SendAlert(_))));
    }

    #[test]
    fn after_shutdown_events_are_ignored() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        state.shutting_down = true;
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::Tick {
                due_tasks: vec![TaskKind::Recovery],
            },
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds.is_empty());
    }

    #[test]
    fn http_trigger_emits_commands() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::HttpCommand(CommandRequest::Trigger(TaskKind::Recovery)),
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds.iter().any(|c| matches!(c, Command::RunProbes(_))));
    }

    #[test]
    fn remediation_restart_service() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::HttpCommand(CommandRequest::Remediate {
                action: RemediationAction::RestartService,
                target: Some(ServiceId("ntfy".into())),
                source: "picoclaw".into(),
                reason: "test".into(),
            }),
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::RestartService { id, .. } if id.as_str() == "ntfy")));
        assert!(!state.remediation_log.is_empty());
    }

    #[test]
    fn remediation_clear_breaker() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);

        // Trip the breaker.
        state
            .services
            .get_mut(&ServiceId("ntfy".into()))
            .unwrap()
            .breaker = crate::model::BreakerState::Open {
            until_mono_secs: 9999,
            trip_count: 2,
        };

        let ctx = test_ctx(300);
        let _cmds = reduce(
            &mut state,
            Event::HttpCommand(CommandRequest::Remediate {
                action: RemediationAction::ClearBreaker,
                target: Some(ServiceId("ntfy".into())),
                source: "operator".into(),
                reason: "manual reset".into(),
            }),
            &config,
            &graph,
            &ctx,
        );

        assert!(matches!(
            state.services[&ServiceId("ntfy".into())].breaker,
            crate::model::BreakerState::Closed
        ));
    }

    #[test]
    fn daily_summary_produces_alert() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);
        let ctx = test_ctx(300);

        let cmds = reduce(
            &mut state,
            Event::Tick {
                due_tasks: vec![TaskKind::DailySummary],
            },
            &config,
            &graph,
            &ctx,
        );

        assert!(cmds
            .iter()
            .any(|c| matches!(c, Command::SendAlert(a) if a.title.contains("сводка"))));
    }

    #[test]
    fn lease_blocks_restart_during_backup() {
        let config = test_config();
        let graph = DepGraph::build(&config.services).unwrap();
        let mut state = State::new(&config, 0);

        // Simulate lease held on ntfy by backup.
        state.leases.acquire(
            ResourceId::Service(ServiceId("ntfy".into())),
            "backup-100",
            100,
        );

        let ctx = test_ctx(200);
        let probes = vec![ProbeResult::Unhealthy {
            service: ServiceId("ntfy".into()),
            error: ProbeError::ConnectionRefused,
            latency_ms: 5,
        }];

        let cmds = reduce(
            &mut state,
            Event::ProbeResults(probes),
            &config,
            &graph,
            &ctx,
        );

        // Should NOT restart ntfy because lease is held.
        assert!(!cmds
            .iter()
            .any(|c| matches!(c, Command::RestartService { id, .. } if id.as_str() == "ntfy")));
    }
}
