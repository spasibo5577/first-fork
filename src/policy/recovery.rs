//! Recovery policy — pure decision logic.
//!
//! Given probe results and current state, decides what to do
//! for each service. No I/O, no side effects, fully testable.

use crate::config::{CratonConfig, ServiceEntry};
use crate::graph::DepGraph;
use crate::model::{ProbeResult, ServiceId, ServiceKind, Severity};
use crate::state::{ServiceStatus, SvcState};
use std::collections::{BTreeMap, BTreeSet};

/// What the reducer should do for a single service after evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Healthy,
    Restart {
        unit: String,
        attempt: u32,
        severity: Severity,
    },
    Failed {
        error: String,
    },
    BlockedByDependency {
        root: ServiceId,
    },
    BreakerOpen,
    InMaintenance,
    DockerRootCause,
}

/// Full result of evaluating all services in one recovery cycle.
#[derive(Debug)]
pub struct RecoveryPlan {
    pub decisions: BTreeMap<ServiceId, Decision>,
    pub docker_restart_needed: bool,
    pub root_causes: Vec<ServiceId>,
    pub blocked: BTreeMap<ServiceId, ServiceId>,
}

/// Evaluates all probe results and produces a recovery plan.
#[must_use]
pub fn evaluate(
    results: &[ProbeResult],
    services_state: &BTreeMap<ServiceId, SvcState>,
    config: &CratonConfig,
    graph: &DepGraph,
    now_mono: u64,
) -> RecoveryPlan {
    let mut decisions = BTreeMap::new();

    let mut unhealthy_set = BTreeSet::new();
    let mut probe_map: BTreeMap<&ServiceId, &ProbeResult> = BTreeMap::new();
    for result in results {
        probe_map.insert(result.service_id(), result);
        if !result.is_healthy() {
            unhealthy_set.insert(result.service_id().clone());
        }
    }

    let (root_causes, blocked) = graph.classify_failures(&unhealthy_set);

    let docker_services_down: Vec<&ServiceId> = root_causes
        .iter()
        .filter(|id| {
            config
                .find_service(id)
                .is_some_and(|s| s.kind == ServiceKind::DockerSystemd)
        })
        .collect();

    let docker_root_cause = docker_services_down.len() >= 2
        || root_causes.iter().any(|id| {
            config
                .find_service(id)
                .is_some_and(|s| s.kind == ServiceKind::Virtual && id.as_str() == "docker_daemon")
        });

    for result in results {
        let sid = result.service_id();

        let Some(svc_config) = config.find_service(sid) else {
            continue;
        };
        let Some(svc_state) = services_state.get(sid) else {
            continue;
        };

        let decision = evaluate_single(
            sid,
            result,
            svc_state,
            svc_config,
            &blocked,
            docker_root_cause,
            now_mono,
        );

        decisions.insert(sid.clone(), decision);
    }

    RecoveryPlan {
        decisions,
        docker_restart_needed: docker_root_cause && !unhealthy_set.is_empty(),
        root_causes,
        blocked,
    }
}

fn evaluate_single(
    sid: &ServiceId,
    probe: &ProbeResult,
    svc_state: &SvcState,
    svc_config: &ServiceEntry,
    blocked: &BTreeMap<ServiceId, ServiceId>,
    docker_root_cause: bool,
    now_mono: u64,
) -> Decision {
    if probe.is_healthy() {
        return Decision::Healthy;
    }

    if svc_state.is_in_maintenance(now_mono) {
        return Decision::InMaintenance;
    }

    if let Some(root) = blocked.get(sid) {
        return Decision::BlockedByDependency {
            root: root.clone(),
        };
    }

    if docker_root_cause && svc_config.kind == ServiceKind::DockerSystemd {
        return Decision::DockerRootCause;
    }

    if !crate::breaker::allows_recovery(&svc_state.breaker, now_mono) {
        return Decision::BreakerOpen;
    }

    if let Some(last) = svc_state.last_restart_mono {
        if now_mono.saturating_sub(last) < svc_config.restart_cooldown_secs {
            return Decision::BreakerOpen;
        }
    }

    let current_attempt = match &svc_state.status {
        ServiceStatus::Recovering { attempt, .. } => attempt + 1,
        ServiceStatus::Unhealthy { consecutive, .. } => *consecutive + 1,
        ServiceStatus::Unknown
        | ServiceStatus::Healthy { .. }
        | ServiceStatus::Failed { .. }
        | ServiceStatus::BlockedByDep { .. }
        | ServiceStatus::Suppressed { .. } => 1,
    };

    let max = svc_config.severity.max_restart_attempts();

    if current_attempt > max {
        let error = match probe {
            ProbeResult::Unhealthy { error, .. } => error.to_string(),
            ProbeResult::Healthy { .. } => "unknown".to_string(),
        };
        return Decision::Failed { error };
    }

    Decision::Restart {
        unit: svc_config.unit.clone(),
        attempt: current_attempt,
        severity: svc_config.severity,
    }
}

/// Builds a coalesced alert body for multiple failed services.
#[must_use]
pub fn coalesce_alert(
    root_causes: &[ServiceId],
    blocked: &BTreeMap<ServiceId, ServiceId>,
    docker_root: bool,
) -> (String, String) {
    if docker_root {
        let blocked_names: Vec<&str> = blocked.keys().map(ServiceId::as_str).collect();
        let title = "🔴 Сбой Docker daemon".to_string();
        let body = format!(
            "Корень: Docker daemon\nЗатронуты: {}\nЗапущено восстановление Docker",
            if blocked_names.is_empty() {
                "нет".to_string()
            } else {
                blocked_names.join(", ")
            }
        );
        return (title, body);
    }

    if root_causes.len() == 1 {
        let title = format!("🔴 {} не работает", root_causes[0]);
        let body = if blocked.is_empty() {
            "Автоматическое восстановление не удалось".to_string()
        } else {
            let deps: Vec<&str> = blocked.keys().map(ServiceId::as_str).collect();
            format!("Зависимые сервисы затронуты: {}", deps.join(", "))
        };
        return (title, body);
    }

    let names: Vec<&str> = root_causes.iter().map(ServiceId::as_str).collect();
    let title = format!("🔴 {} сервисов не работают", root_causes.len());
    let body = format!("Не удалось восстановить: {}", names.join(", "));
    (title, body)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::*;
    use crate::model::{BreakerState, ProbeError, ProbeSpec};

    fn make_config(services: Vec<ServiceEntry>) -> CratonConfig {
        CratonConfig {
            daemon: DaemonConfig {
                listen: "127.0.0.1:18800".into(),
                watchdog: true,
                log_level: "info".into(),
            },
            ntfy: NtfyConfig {
                url: "http://localhost".into(),
                topic: "test".into(),
                retries: vec![0, 5],
            },
            backup: BackupConfig {
                schedule: "odd_days:04:00".into(),
                restic_repo: "/repo".into(),
                restic_password_file: "/pass".into(),
                restic_binary: "/usr/bin/restic".into(),
                retention: RetentionConfig {
                    daily: 7,
                    weekly: 4,
                    monthly: 6,
                },
                verify: false,
                verify_subset_percent: 5,
                paths: vec![],
            },
            disk: DiskConfig {
                interval: "6h".into(),
                warn_percent: 85,
                critical_percent: 95,
                predictive: false,
            },
            updates: UpdatesConfig {
                apt_schedule: "daily:09:00".into(),
                docker_schedule: "weekly:sun:10:00".into(),
            },
            ai: AiConfig {
                mode: "disabled".into(),
                picoclaw_url: String::new(),
                context_path: String::new(),
                token_path: String::new(),
            },
            services,
        }
    }

    fn svc_entry(id: &str, kind: ServiceKind, severity: Severity, deps: &[&str]) -> ServiceEntry {
        ServiceEntry {
            id: ServiceId(id.into()),
            name: id.into(),
            unit: format!("{id}.service"),
            kind,
            probe: ProbeSpec::SystemdActive {
                unit: String::new(),
            },
            depends_on: deps.iter().map(|d| ServiceId(d.to_string())).collect(),
            resources: vec![],
            severity,
            startup_grace_secs: 10,
            restart_cooldown_secs: 60,
            max_restarts: 3,
            breaker_window_secs: 3600,
            breaker_cooldown_secs: 3600,
            backup_stop: false,
        }
    }

    fn healthy_probe(id: &str) -> ProbeResult {
        ProbeResult::Healthy {
            service: ServiceId(id.into()),
            latency_ms: 5,
        }
    }

    fn unhealthy_probe(id: &str) -> ProbeResult {
        ProbeResult::Unhealthy {
            service: ServiceId(id.into()),
            error: ProbeError::ConnectionRefused,
            latency_ms: 5,
        }
    }

    #[test]
    fn healthy_service_gets_healthy_decision() {
        let entries = vec![svc_entry("ntfy", ServiceKind::Systemd, Severity::Critical, &[])];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        svc_state.insert(ServiceId("ntfy".into()), SvcState::new());

        let plan = evaluate(&[healthy_probe("ntfy")], &svc_state, &config, &graph, 100);
        assert_eq!(plan.decisions[&ServiceId("ntfy".into())], Decision::Healthy);
    }

    #[test]
    fn unhealthy_service_gets_restart() {
        let entries = vec![svc_entry("ntfy", ServiceKind::Systemd, Severity::Critical, &[])];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        svc_state.insert(ServiceId("ntfy".into()), SvcState::new());

        let plan = evaluate(
            &[unhealthy_probe("ntfy")],
            &svc_state,
            &config,
            &graph,
            100,
        );
        match &plan.decisions[&ServiceId("ntfy".into())] {
            Decision::Restart {
                attempt: 1,
                severity: Severity::Critical,
                ..
            } => {}
            other => panic!("expected Restart, got {other:?}"),
        }
    }

    #[test]
    fn dependency_down_blocks_child() {
        let entries = vec![
            svc_entry("unbound", ServiceKind::Systemd, Severity::Critical, &[]),
            svc_entry(
                "adguard",
                ServiceKind::Systemd,
                Severity::Critical,
                &["unbound"],
            ),
        ];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        svc_state.insert(ServiceId("unbound".into()), SvcState::new());
        svc_state.insert(ServiceId("adguard".into()), SvcState::new());

        let probes = vec![unhealthy_probe("unbound"), unhealthy_probe("adguard")];
        let plan = evaluate(&probes, &svc_state, &config, &graph, 100);

        assert!(matches!(
            plan.decisions[&ServiceId("adguard".into())],
            Decision::BlockedByDependency { .. }
        ));
        assert!(matches!(
            plan.decisions[&ServiceId("unbound".into())],
            Decision::Restart { .. }
        ));
    }

    #[test]
    fn maintenance_skips_recovery() {
        let entries = vec![svc_entry("ntfy", ServiceKind::Systemd, Severity::Critical, &[])];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        let mut ss = SvcState::new();
        ss.maintenance = Some(crate::state::Maintenance {
            until_mono: 999,
            reason: "testing".into(),
        });
        svc_state.insert(ServiceId("ntfy".into()), ss);

        let plan = evaluate(
            &[unhealthy_probe("ntfy")],
            &svc_state,
            &config,
            &graph,
            100,
        );
        assert_eq!(
            plan.decisions[&ServiceId("ntfy".into())],
            Decision::InMaintenance
        );
    }

    #[test]
    fn breaker_open_suppresses() {
        let entries = vec![svc_entry("ntfy", ServiceKind::Systemd, Severity::Critical, &[])];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        let mut ss = SvcState::new();
        ss.breaker = BreakerState::Open {
            until_mono_secs: 999,
            trip_count: 1,
        };
        svc_state.insert(ServiceId("ntfy".into()), ss);

        let plan = evaluate(
            &[unhealthy_probe("ntfy")],
            &svc_state,
            &config,
            &graph,
            100,
        );
        assert_eq!(
            plan.decisions[&ServiceId("ntfy".into())],
            Decision::BreakerOpen
        );
    }

    #[test]
    fn correlated_docker_failure() {
        let entries = vec![
            svc_entry("docker_daemon", ServiceKind::Virtual, Severity::Critical, &[]),
            svc_entry(
                "continuwuity",
                ServiceKind::DockerSystemd,
                Severity::Critical,
                &["docker_daemon"],
            ),
            svc_entry(
                "gatus",
                ServiceKind::DockerSystemd,
                Severity::Warning,
                &["docker_daemon"],
            ),
        ];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        for e in &entries {
            svc_state.insert(e.id.clone(), SvcState::new());
        }

        let probes = vec![
            unhealthy_probe("docker_daemon"),
            unhealthy_probe("continuwuity"),
            unhealthy_probe("gatus"),
        ];
        let plan = evaluate(&probes, &svc_state, &config, &graph, 100);

        assert!(plan.docker_restart_needed);
        assert!(matches!(
            plan.decisions[&ServiceId("continuwuity".into())],
            Decision::BlockedByDependency { .. }
        ));
    }

    #[test]
    fn info_severity_gets_one_attempt() {
        let entries = vec![svc_entry("regbot", ServiceKind::Systemd, Severity::Info, &[])];
        let config = make_config(entries.clone());
        let graph = DepGraph::build(&entries).unwrap();
        let mut svc_state = BTreeMap::new();
        let mut ss = SvcState::new();
        ss.status = ServiceStatus::Unhealthy {
            since_mono: 50,
            error: "down".into(),
            consecutive: 1,
        };
        svc_state.insert(ServiceId("regbot".into()), ss);

        let plan = evaluate(
            &[unhealthy_probe("regbot")],
            &svc_state,
            &config,
            &graph,
            100,
        );
        assert!(matches!(
            plan.decisions[&ServiceId("regbot".into())],
            Decision::Failed { .. }
        ));
    }
}