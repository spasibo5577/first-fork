use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub reason: String,
}

impl HealthResponse {
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub services: BTreeMap<String, ServiceStatusDto>,
    pub backup_phase: BackupPhaseDto,
    pub disk_usage_percent: Option<u32>,
    pub shutting_down: bool,
    pub backup_history: Vec<BackupRecordDto>,
    pub recovery_history: Vec<RecoveryRecordDto>,
    pub remediation_history: Vec<RemediationRecordDto>,
    pub snapshot_epoch_secs: u64,
    pub last_recovery_mono: Option<u64>,
    pub start_mono: u64,
    pub outbox_overflow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ServiceStatusDto {
    Unknown,
    Healthy { since_mono: u64 },
    Unhealthy {
        since_mono: u64,
        error: String,
        consecutive: u32,
    },
    Recovering { attempt: u32, since_mono: u64 },
    Failed { since_mono: u64, error: String },
    BlockedByDep { root: String },
    Suppressed { until_mono: u64 },
}

impl ServiceStatusDto {
    #[must_use]
    pub const fn status_name(&self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Healthy { .. } => "healthy",
            Self::Unhealthy { .. } => "unhealthy",
            Self::Recovering { .. } => "recovering",
            Self::Failed { .. } => "failed",
            Self::BlockedByDep { .. } => "blocked_by_dep",
            Self::Suppressed { .. } => "suppressed",
        }
    }

    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !matches!(self, Self::Healthy { .. } | Self::Unknown)
    }

    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Unknown => "unknown".into(),
            Self::Healthy { .. } => "healthy".into(),
            Self::Unhealthy { error, .. } => format!("unhealthy: {error}"),
            Self::Recovering { attempt, .. } => format!("recovering (attempt {attempt})"),
            Self::Failed { error, .. } => format!("failed: {error}"),
            Self::BlockedByDep { root } => format!("blocked by dependency: {root}"),
            Self::Suppressed { .. } => "suppressed by breaker".into(),
        }
    }

    #[must_use]
    pub fn timing(&self) -> Option<u64> {
        match self {
            Self::Healthy { since_mono }
            | Self::Unhealthy { since_mono, .. }
            | Self::Recovering { since_mono, .. }
            | Self::Failed { since_mono, .. } => Some(*since_mono),
            Self::Unknown | Self::BlockedByDep { .. } | Self::Suppressed { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum BackupPhaseDto {
    Idle,
    Locked { run_id: String },
    ResticUnlocking { run_id: String },
    ServicesStopping { run_id: String },
    ResticRunning { run_id: String },
    ServicesStarting { run_id: String },
    ServicesVerifying { run_id: String },
    RetentionRunning { run_id: String },
    Verifying { run_id: String },
}

impl BackupPhaseDto {
    #[must_use]
    pub const fn phase_name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Locked { .. } => "locked",
            Self::ResticUnlocking { .. } => "restic_unlocking",
            Self::ServicesStopping { .. } => "services_stopping",
            Self::ResticRunning { .. } => "restic_running",
            Self::ServicesStarting { .. } => "services_starting",
            Self::ServicesVerifying { .. } => "services_verifying",
            Self::RetentionRunning { .. } => "retention_running",
            Self::Verifying { .. } => "verifying",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryRecordDto {
    pub mono: u64,
    pub recovered: Vec<String>,
    pub failed: Vec<String>,
    pub docker_restarted: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRecordDto {
    pub mono: u64,
    pub success: bool,
    pub partial: bool,
    pub error: Option<String>,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemediationRecordDto {
    pub mono: u64,
    pub action: String,
    pub target: Option<String>,
    pub source: String,
    pub result: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnoseResponse {
    pub service: String,
    pub unit: String,
    pub active: bool,
    pub journal_last_50: String,
    pub systemctl_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandAcceptedResponse {
    pub status: String,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSummary {
    pub health: HealthResponse,
    pub service_count: usize,
    pub degraded_count: usize,
    pub backup_phase: String,
    pub disk_usage_percent: Option<u32>,
    pub shutting_down: bool,
    pub outbox_overflow: bool,
    pub snapshot_epoch_secs: u64,
    pub last_recovery_mono: Option<u64>,
}

impl StatusSummary {
    #[must_use]
    pub fn from_parts(health: HealthResponse, state: &StateSnapshot) -> Self {
        let degraded_count = state
            .services
            .values()
            .filter(|status| status.is_degraded())
            .count();

        Self {
            health,
            service_count: state.services.len(),
            degraded_count,
            backup_phase: state.backup_phase.phase_name().into(),
            disk_usage_percent: state.disk_usage_percent,
            shutting_down: state.shutting_down,
            outbox_overflow: state.outbox_overflow,
            snapshot_epoch_secs: state.snapshot_epoch_secs,
            last_recovery_mono: state.last_recovery_mono,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceSummary {
    pub id: String,
    pub status: String,
    pub summary: String,
}

impl ServiceSummary {
    #[must_use]
    pub fn list_from_snapshot(state: StateSnapshot) -> Vec<Self> {
        state.services
            .into_iter()
            .map(|(id, status)| Self {
                id,
                status: status.status_name().into(),
                summary: status.summary(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceDetail {
    pub id: String,
    pub status: String,
    pub summary: String,
    pub since_mono: Option<u64>,
    pub raw_status: ServiceStatusDto,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub action: String,
    pub status: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

impl ServiceDetail {
    #[must_use]
    pub fn from_snapshot(state: &StateSnapshot, id: &str) -> Option<Self> {
        let status = state.services.get(id)?.clone();
        Some(Self {
            id: id.into(),
            status: status.status_name().into(),
            summary: status.summary(),
            since_mono: status.timing(),
            raw_status: status,
        })
    }
}
