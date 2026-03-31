//! Mutable runtime state owned exclusively by the control loop.
//!
//! No mutex — single owner. Other threads see only read-only snapshots.

use crate::config::CratonConfig;
use crate::history::RingBuf;
use crate::lease::LeaseArbiter;
use crate::model::{BackupPhase, BreakerState, ServiceId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const MAX_RESTART_HISTORY: usize = 20;
const MAX_BACKUP_HISTORY: usize = 30;
const MAX_RECOVERY_HISTORY: usize = 100;
const MAX_DISK_SAMPLES: usize = 14;
const MAX_REMEDIATION_LOG: usize = 50;

/// All mutable state for the daemon. Owned by the control loop thread.
/// Fields are consumed incrementally as runtime wiring completes.
#[allow(dead_code)] // fields wired across phases
#[derive(Debug)]
pub struct State {
    pub services: BTreeMap<ServiceId, SvcState>,
    pub backup_phase: BackupPhase,
    pub backup_history: RingBuf<BackupRecord>,
    pub recovery_history: RingBuf<RecoveryRecord>,
    pub disk_samples: RingBuf<DiskSample>,
    pub remediation_log: RingBuf<RemediationRecord>,
    pub leases: LeaseArbiter,
    pub disk_usage_percent: Option<u32>,
    pub disk_free_bytes: Option<u64>,
    pub last_backup_day: Option<u32>,
    pub last_apt_day: Option<u32>,
    pub last_docker_day: Option<u32>,
    pub last_summary_day: Option<u32>,
    pub last_recovery_mono: Option<u64>,
    pub last_probe_cycle_mono: Option<u64>,
    pub last_disk_mono: Option<u64>,
    pub backup_pending_cmd: Option<u64>,
    pub consecutive_backup_failures: u32,
    pub next_cmd_id: u64,
    pub start_mono: u64,
    pub shutting_down: bool,
    pub degraded_reasons: Vec<String>,
}

impl State {
    #[must_use]
    pub fn new(config: &CratonConfig, start_mono: u64) -> Self {
        let mut services = BTreeMap::new();
        for svc in &config.services {
            services.insert(svc.id.clone(), SvcState::new());
        }

        Self {
            services,
            backup_phase: BackupPhase::Idle,
            backup_history: RingBuf::new(MAX_BACKUP_HISTORY),
            recovery_history: RingBuf::new(MAX_RECOVERY_HISTORY),
            disk_samples: RingBuf::new(MAX_DISK_SAMPLES),
            remediation_log: RingBuf::new(MAX_REMEDIATION_LOG),
            leases: LeaseArbiter::new(7200),
            disk_usage_percent: None,
            disk_free_bytes: None,
            last_backup_day: None,
            last_apt_day: None,
            last_docker_day: None,
            last_summary_day: None,
            last_recovery_mono: None,
            last_probe_cycle_mono: None,
            last_disk_mono: None,
            backup_pending_cmd: None,
            consecutive_backup_failures: 0,
            next_cmd_id: 1,
            start_mono,
            shutting_down: false,
            degraded_reasons: Vec::new(),
        }
    }

    /// Allocates a unique command ID for effect tracking.
    #[allow(dead_code)] // Phase 4: effect command tracking
    pub fn alloc_cmd_id(&mut self) -> u64 {
        let id = self.next_cmd_id;
        self.next_cmd_id += 1;
        id
    }

    pub fn mark_degraded<S: Into<String>>(&mut self, reason: S) {
        let reason = reason.into();
        if !self.degraded_reasons.iter().any(|existing| existing == &reason) {
            self.degraded_reasons.push(reason);
        }
    }
}

/// Per-service runtime state.
#[derive(Debug, Clone)]
pub struct SvcState {
    pub status: ServiceStatus,
    pub breaker: BreakerState,
    pub restart_history: RingBuf<RestartRecord>,
    pub last_restart_mono: Option<u64>,
    #[allow(dead_code)] // Phase 4: alert dedup cooldown
    pub last_alert_mono: Option<u64>,
    pub maintenance: Option<Maintenance>,
}

impl SvcState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: ServiceStatus::Unknown,
            breaker: BreakerState::Closed,
            restart_history: RingBuf::new(MAX_RESTART_HISTORY),
            last_restart_mono: None,
            last_alert_mono: None,
            maintenance: None,
        }
    }

    #[must_use]
    pub fn is_in_maintenance(&self, now_mono: u64) -> bool {
        self.maintenance
            .as_ref()
            .is_some_and(|m| now_mono < m.until_mono)
    }

    #[must_use]
    pub fn restarts_in_window(&self, now_mono: u64, window_secs: u64) -> u32 {
        let cutoff = now_mono.saturating_sub(window_secs);
        let count = self
            .restart_history
            .iter()
            .filter(|r| r.mono >= cutoff)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// Service status as determined by the reducer after considering
/// probes, dependencies, breaker state, and maintenance.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ServiceStatus {
    Unknown,
    Healthy {
        since_mono: u64,
    },
    #[allow(dead_code)] // Phase 4: set when consecutive failures tracked
    Unhealthy {
        since_mono: u64,
        error: String,
        consecutive: u32,
    },
    Recovering {
        attempt: u32,
        since_mono: u64,
    },
    Failed {
        since_mono: u64,
        error: String,
    },
    BlockedByDep {
        root: ServiceId,
    },
    #[allow(dead_code)] // Phase 4: set when breaker trips → suppress
    Suppressed {
        until_mono: u64,
    },
}

impl ServiceStatus {
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    #[must_use]
    pub fn is_degraded(&self) -> bool {
        matches!(
            self,
            Self::Unhealthy { .. }
                | Self::Recovering { .. }
                | Self::Failed { .. }
                | Self::BlockedByDep { .. }
                | Self::Suppressed { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Maintenance {
    pub until_mono: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupRecord {
    pub mono: u64,
    pub success: bool,
    pub partial: bool,
    pub error: Option<String>,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecoveryRecord {
    pub mono: u64,
    pub recovered: Vec<ServiceId>,
    pub failed: Vec<ServiceId>,
    pub docker_restarted: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiskSample {
    pub mono: u64,
    pub usage_percent: u32,
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemediationRecord {
    pub mono: u64,
    pub action: String,
    pub target: Option<ServiceId>,
    pub source: String,
    pub result: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestartRecord {
    pub mono: u64,
    pub attempt: u32,
    pub success: bool,
}
