//! Core domain types for the CRATON system.
//!
//! Every enum here must be exhaustively matched.
//! Adding a variant is a compile-time breaking change — by design.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

// ─── Service identity ───────────────────────────────────────────

/// Opaque service identifier. Cheap to clone, cheap to compare.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(pub String);

impl ServiceId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ─── Resource identity (for leases) ────────────────────────────

/// Identifies a resource that can be leased for exclusive access.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ResourceId {
    Service(ServiceId),
    DockerDaemon,
    BackupRepo,
    DiskCleanup,
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Service(id) => write!(f, "service:{id}"),
            Self::DockerDaemon => f.write_str("docker-daemon"),
            Self::BackupRepo => f.write_str("backup-repo"),
            Self::DiskCleanup => f.write_str("disk-cleanup"),
        }
    }
}

// ─── Service kind ──────────────────────────────────────────────

/// How the service is managed at the OS level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    /// Native systemd unit.
    Systemd,
    /// Docker container wrapped in a systemd unit.
    DockerSystemd,
    /// Virtual node for dependency graph (e.g. `docker_daemon`).
    Virtual,
}

// ─── Severity ──────────────────────────────────────────────────

/// Determines recovery aggressiveness and alert priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    /// Maximum restart attempts before giving up.
    #[must_use]
    pub const fn max_restart_attempts(self) -> u32 {
        match self {
            Self::Info => 1,
            Self::Warning => 2,
            Self::Critical => 3,
        }
    }

    /// Whether Docker daemon restart is allowed on escalation.
    #[allow(dead_code)] // wired in Phase 4: recovery escalation gating
    #[must_use]
    pub const fn allows_docker_escalation(self) -> bool {
        matches!(self, Self::Critical)
    }

    /// Whether to generate an incident markdown file.
    #[must_use]
    pub const fn generates_incident(self) -> bool {
        matches!(self, Self::Critical)
    }
}

// ─── Probe specification ───────────────────────────────────────

/// How to verify that a service is alive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProbeSpec {
    Http {
        url: String,
        #[serde(default = "default_probe_timeout")]
        timeout_secs: u64,
        #[serde(default = "default_http_status")]
        expect_status: u16,
    },
    Dns {
        server: String,
        port: u16,
        query: String,
        #[serde(default = "default_probe_timeout")]
        timeout_secs: u64,
    },
    SystemdActive {
        #[serde(default)]
        unit: String,
    },
    Exec {
        argv: Vec<String>,
        #[serde(default = "default_probe_timeout")]
        timeout_secs: u64,
        #[serde(default)]
        expect_stdout: Option<StdoutCheck>,
    },
}

impl ProbeSpec {
    /// Returns the timeout for this probe as a `Duration`.
    #[allow(dead_code)] // wired in Phase 4: probe scheduling with unified timeout
    #[must_use]
    pub fn timeout(&self) -> Duration {
        let secs = match self {
            Self::Http { timeout_secs, .. }
            | Self::Dns { timeout_secs, .. }
            | Self::Exec { timeout_secs, .. } => *timeout_secs,
            Self::SystemdActive { .. } => 5,
        };
        Duration::from_secs(secs)
    }
}

/// How to validate stdout of an exec probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StdoutCheck {
    Contains { pattern: String },
    NotContains { pattern: String },
    JsonField { pointer: String, expected: String },
}

fn default_probe_timeout() -> u64 {
    5
}
fn default_http_status() -> u16 {
    200
}

// ─── Probe result ──────────────────────────────────────────────

/// Outcome of a single health probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProbeResult {
    Healthy {
        service: ServiceId,
        latency_ms: u64,
    },
    Unhealthy {
        service: ServiceId,
        error: ProbeError,
        latency_ms: u64,
    },
}

impl ProbeResult {
    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        match self {
            Self::Healthy { service, .. } | Self::Unhealthy { service, .. } => service,
        }
    }

    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }
}

/// Structured probe error — decisions are based on error class, not text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbeError {
    ConnectionRefused,
    Timeout,
    HttpStatus { code: u16 },
    DnsFailure { message: String },
    ExecFailed { exit_code: i32, stderr: String },
    UnexpectedOutput { detail: String },
    DependencyUnavailable { root: ServiceId },
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectionRefused => f.write_str("connection refused"),
            Self::Timeout => f.write_str("timeout"),
            Self::HttpStatus { code } => write!(f, "HTTP {code}"),
            Self::DnsFailure { message } => write!(f, "DNS: {message}"),
            Self::ExecFailed { exit_code, stderr } => write!(f, "exit {exit_code}: {stderr}"),
            Self::UnexpectedOutput { detail } => write!(f, "unexpected output: {detail}"),
            Self::DependencyUnavailable { root } => write!(f, "dependency unavailable: {root}"),
        }
    }
}

// ─── Circuit breaker ───────────────────────────────────────────

/// Per-service circuit breaker state.
///
/// State transitions are managed by `breaker::record_restart`,
/// `breaker::on_healthy_probe`, and `breaker::maybe_transition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open {
        until_mono_secs: u64,
        trip_count: u32,
    },
    HalfOpen {
        probe_attempt: u32,
        previous_trip_count: u32,
    },
}

// ─── Backup FSM ────────────────────────────────────────────────

/// Backup lifecycle phases. Each variant carries crash-recovery data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum BackupPhase {
    Idle,
    Locked {
        run_id: String,
    },
    ResticUnlocking {
        run_id: String,
    },
    ServicesStopping {
        run_id: String,
        /// Services that were running BEFORE backup. On crash, restore exactly these.
        pre_backup_state: Vec<ServiceSnapshot>,
    },
    ResticRunning {
        run_id: String,
        pre_backup_state: Vec<ServiceSnapshot>,
    },
    ServicesStarting {
        run_id: String,
        remaining: Vec<ServiceRestore>,
    },
    ServicesVerifying {
        run_id: String,
        started: Vec<ServiceId>,
    },
    RetentionRunning {
        run_id: String,
    },
    Verifying {
        run_id: String,
    },
}

impl BackupPhase {
    #[must_use]
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    #[must_use]
    pub fn needs_service_recovery(&self) -> bool {
        matches!(
            self,
            Self::ServicesStopping { .. }
                | Self::ResticRunning { .. }
                | Self::ServicesStarting { .. }
                | Self::ServicesVerifying { .. }
        )
    }

    #[must_use]
    pub fn needs_restic_unlock(&self) -> bool {
        matches!(
            self,
            Self::ResticUnlocking { .. } | Self::ResticRunning { .. }
        )
    }

    #[must_use]
    pub fn pre_backup_services(&self) -> Option<&[ServiceSnapshot]> {
        match self {
            Self::ServicesStopping {
                pre_backup_state, ..
            }
            | Self::ResticRunning {
                pre_backup_state, ..
            } => Some(pre_backup_state),
            _ => None,
        }
    }
}

/// Records whether a service was running before backup started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSnapshot {
    pub id: ServiceId,
    pub was_running: bool,
    pub unit: String,
}

/// Tracks progress of restarting a service after backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRestore {
    pub id: ServiceId,
    pub unit: String,
    pub attempts: u32,
    pub docker_restarted: bool,
}

// ─── Events ────────────────────────────────────────────────────

/// Every input to the reducer.
#[derive(Debug, Clone)]
pub enum Event {
    /// Scheduler determined these tasks are due.
    Tick { due_tasks: Vec<TaskKind> },

    /// Health probes completed.
    ProbeResults(Vec<ProbeResult>),

    /// Fresh disk usage sample collected by runtime and fed back into reducer.
    DiskSample(crate::state::DiskSample),

    /// An effect (external command) completed.
    /// Constructed by effect executor, fed back into reducer.
    #[allow(dead_code)] // Phase 4: effect loop closure
    EffectCompleted { cmd_id: u64, result: EffectResult },

    /// HTTP API request requiring state mutation.
    HttpCommand(CommandRequest),

    /// OS signal received.
    Signal(SignalKind),

    /// Daemon just started — run crash recovery.
    StartupRecovery { persisted_backup: BackupPhase },
}

/// Kinds of scheduled tasks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Recovery,
    Backup,
    DiskMonitor,
    AptUpdates,
    DockerUpdates,
    DailySummary,
}

/// Result of executing an external command.
/// Constructed by the effect executor and fed back via `Event::EffectCompleted`.
#[allow(dead_code)] // Phase 4: effect worker constructs these from ExecResult
#[derive(Debug, Clone)]
pub enum EffectResult {
    Success {
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    Failed {
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    Killed {
        signal: i32,
        duration_ms: u64,
    },
    HelperError {
        message: String,
    },
}

/// Commands arriving from the HTTP API.
/// Constructed by HTTP handler, consumed by `reduce::handle_http_command`.
#[allow(dead_code)] // Phase 4: HTTP remediation endpoint + reducer handler
#[derive(Debug, Clone)]
pub enum CommandRequest {
    Trigger(TaskKind),
    Remediate {
        action: RemediationAction,
        target: Option<ServiceId>,
        source: String,
        reason: String,
    },
}

/// OS signals the daemon handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    #[allow(dead_code)] // constructed in signal.rs under #[cfg(unix)]
    Shutdown,
    #[allow(dead_code)] // constructed in signal.rs under #[cfg(unix)]
    Reload,
}

// ─── Commands (reducer output) ─────────────────────────────────

/// Every output from the reducer. Effect executor performs these.
/// Not all variants are emitted yet — the enum defines the complete protocol.
#[allow(dead_code)] // Protocol enum: variants wired incrementally across phases
#[derive(Debug, Clone)]
pub enum Command {
    // ── Service management ──
    RunProbes(Vec<ServiceId>),
    RestartService {
        id: ServiceId,
        unit: String,
        reason: String,
    },
    StopService {
        id: ServiceId,
        unit: String,
        reason: String,
    },
    StartService {
        id: ServiceId,
        unit: String,
    },
    RestartDockerDaemon {
        reason: String,
    },

    // ── Backup ──
    ResticUnlock,
    ResticBackup {
        paths: Vec<String>,
    },
    ResticForget {
        daily: u32,
        weekly: u32,
        monthly: u32,
    },
    ResticCheck {
        subset_percent: u32,
    },

    // ── Notifications ──
    SendAlert(Alert),

    // ── Persistence ──
    PersistBackupState(BackupPhase),
    PersistMaintenance,

    // ── State publishing ──
    PublishSnapshot,

    // ── Disk ──
    RunDiskCleanup {
        level: CleanupLevel,
    },
    CheckDiskUsage,

    // ── Updates ──
    CheckAptUpdates,
    CheckDockerUpdates,

    // ── Incident reports ──
    WriteIncident(IncidentReport),

    // ── AI bridge ──
    UpdateLlmContext,
    TriggerPicoClaw {
        event_type: String,
        details: BTreeMap<String, String>,
    },

    // ── Daemon lifecycle ──
    NotifyWatchdog,
    Shutdown {
        grace_secs: u64,
    },

    // ── Leases ──
    AcquireLease {
        resource: ResourceId,
        holder: String,
    },
    ReleaseLease {
        resource: ResourceId,
    },
}

// ─── Alert ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub title: String,
    pub body: String,
    pub priority: AlertPriority,
    pub tags: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertPriority {
    Min,
    Low,
    Default,
    High,
    Urgent,
}

impl AlertPriority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Low => "low",
            Self::Default => "default",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

// ─── Cleanup level ─────────────────────────────────────────────

/// Disk cleanup aggressiveness.
#[allow(dead_code)] // Phase 4: constructed by disk policy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupLevel {
    /// `apt clean` + journal vacuum.
    Standard,
    /// Standard + `docker image prune -a` (only if safe).
    Aggressive,
}

// ─── Incident report ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentReport {
    pub kind: IncidentKind,
    pub service: Option<ServiceId>,
    pub timestamp_epoch_secs: u64,
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    ServiceUnrecoverable,
    BackupFailed,
    DiskCritical,
    DockerDaemonFailure,
}

// ─── Remediation ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemediationAction {
    RestartService,
    ResticUnlock,
    DockerRestart,
    MarkMaintenance,
    ClearMaintenance,
    ClearFlapping,
    RunDiskCleanup,
    TriggerBackup,
    ClearBreaker,
}
