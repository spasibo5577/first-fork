//! Top-level reducer: `(State, Event) → Vec<Command>`.
//!
//! The single point where all state mutations happen.
//! Pure logic — no I/O, no exec, no network.

use crate::config::CratonConfig;
use crate::graph::DepGraph;
use crate::model::{
    Alert, AlertPriority, BackupPhase, Command, CommandRequest, Event,
    IncidentKind, IncidentReport, ResourceId, ServiceId, SignalKind, TaskKind,
};
use crate::policy::{backup, recovery};
use crate::schedule::WallClock;
use crate::state::{RecoveryRecord, RestartRecord, ServiceStatus, State};
use std::collections::BTreeMap;

/// Context provided by the runtime to the reducer on each call.
pub struct Ctx {
    pub mono_secs: u64,
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
                cmds.push(Command::PublishSnapshot);
            }
        }
    }

    cmds
}

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

    apply_decisions(state, &plan, config, ctx, &mut cmds, &mut recovered, &mut failed);
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
                        timestamp_epoch_secs: ctx.mono_secs,
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

fn start_backup(state: &mut State, config: &CratonConfig, ctx: &Ctx) -> Vec<Command> {
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

    for svc in config.backup_stop_services() {
        cmds.push(Command::AcquireLease {
            resource: ResourceId::Service(svc.id.clone()),
            holder: run_id.clone(),
        });
    }

    state.backup_phase = BackupPhase::ResticUnlocking {
        run_id,
    };
    cmds.push(Command::PersistBackupState(state.backup_phase.clone()));

    cmds
}

fn handle_effect_completed(
    _state: &mut State,
    _cmd_id: u64,
    _result: &crate::model::EffectResult,
    _config: &CratonConfig,
    _ctx: &Ctx,
) -> Vec<Command> {
    vec![Command::NotifyWatchdog]
}

fn handle_http_command(
    _state: &mut State,
    _req: CommandRequest,
    _config: &CratonConfig,
    _ctx: &Ctx,
) -> Vec<Command> {
    vec![]
}

fn handle_shutdown(state: &mut State) -> Vec<Command> {
    state.shutting_down = true;
    vec![Command::Shutdown { grace_secs: 90 }]
}

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

    fn test_ctx(mono: u64) -> Ctx {
        Ctx {
            mono_secs: mono,
            wall: WallClock {
                year: 2025,
                month: 1,
                day: 15,
                hour: 4,
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
}