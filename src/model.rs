//! Core domain types for the entire CRATON system.
//!
//! Every enum here must be exhaustively matched everywhere it is used.
//! Adding a variant is a compile-time breaking change — by design.
//!
//! Naming: types use full English names, no abbreviations.
//! Serialization: `serde` with `rename_all = "snake_case"` for JSON compatibility.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

// ─── Service identity ───────────────────────────────────────────

/// Opaque service identifier. Cheap to clone, cheap to compare.
/// Wrapping a `String` gives us type safety over raw strings:
/// a function accepting `ServiceId` cannot accidentally receive
/// a unit name or a URL.
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
    /// A specific service instance.
    Service(ServiceId),
    /// The Docker daemon itself.
    DockerDaemon,
    /// The restic backup repository.
    BackupRepo,
    /// Disk cleanup operations.
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
    /// Native systemd unit (e.g., unbound, ntfy).
    Systemd,
    /// Docker container wrapped in a systemd unit (e.g., continuwuity).
    /// Key difference: if this fails, Docker daemon health is checked.
    DockerSystemd,
        /// Virtual node representing infrastructure (e.g., `docker_daemon`).
    /// Not a real service — used as a dependency graph root.
    Virtual,
}

// ─── Severity ──────────────────────────────────────────────────

/// Determines recovery aggressiveness and alert priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// One restart attempt, default alert, no incident report.
    Info,
    /// Two restart attempts, high alert, no Docker escalation.
    Warning,
    /// Three restart attempts, urgent alert, Docker escalation, incident report.
    Critical,
}

impl Severity {
    /// Maximum number of restart attempts before giving up.
    #[must_use]
    pub const fn max_restart_attempts(self) -> u32 {
        match self {
            Self::Info => 1,
            Self::Warning => 2,
            Self::Critical => 3,
        }
    }

    /// Whether Docker daemon restart is attempted if individual restart fails.
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
        /// Expected HTTP status code. Default: 200.
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
        /// If empty, uses the service's `unit` field.
        #[serde(default)]
        unit: String,
    },
    Exec {
        argv: Vec<String>,
        #[serde(default = "default_probe_timeout")]
        timeout_secs: u64,
        /// Optional stdout validation.
        #[serde(default)]
        expect_stdout: Option<StdoutCheck>,
    },
}

impl ProbeSpec {
    /// Returns the timeout for this probe as a `Duration`.
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
    /// Stdout must contain this substring.
    Contains { pattern: String },
    /// Stdout must NOT contain this substring.
    NotContains { pattern: String },
    /// Parse stdout as JSON and check a field via JSON pointer.
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

/// Structured error from a probe — not a string, because we want
/// to make decisions based on error class, not parse error text.
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
            Self::ExecFailed { exit_code, stderr } => {
                write!(f, "exit {exit_code}: {stderr}")
            }
            Self::UnexpectedOutput { detail } => write!(f, "unexpected output: {detail}"),
            Self::DependencyUnavailable { root } => {
                write!(f, "dependency unavailable: {root}")
            }
        }
    }
}

// ─── Service state FSM ─────────────────────────────────────────

/// The operational state of a single service as determined by the reducer.
/// This is NOT a health check result — it's the recovery engine's
/// assessment after considering dependencies, breaker state, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    /// Service is healthy. No action needed.
    Healthy,
    /// Service failed health check but hasn't exhausted recovery attempts.
    Degraded {
        consecutive_failures: u32,
    },
    /// Service failed and recovery was attempted but unsuccessful.
    Failed {
        last_error: String,
    },
    /// Recovery is in progress right now.
    Recovering {
        attempt: u32,
    },
    /// Service itself may be fine, but a dependency is down.
    /// Recovery is suppressed to avoid wasted restarts.
    BlockedByDependency {
        root: ServiceId,
    },
    /// Circuit breaker tripped — too many restarts in window.
    Suppressed {
        until_epoch_secs: u64,
    },
    /// Operator or AI explicitly marked as maintenance.
    InMaintenance {
        until_epoch_secs: u64,
        reason: String,
    },
    /// No data yet (initial state before first probe).
    Unknown,
}

// ─── Circuit breaker ───────────────────────────────────────────

/// Per-service circuit breaker state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BreakerState {
    /// Normal operation — recovery is allowed.
    Closed,
    /// Recovery suppressed until the given monotonic instant.
    Open {
        /// Seconds since an arbitrary monotonic epoch.
        until_mono_secs: u64,
        trip_count: u32,
    },
    /// Cooldown expired — one probe attempt allowed to test recovery.
    HalfOpen {
        probe_attempt: u32,
        previous_trip_count: u32,
    },
}

impl BreakerState {
    /// Returns `true` if recovery actions are currently allowed.
    #[must_use]
    pub fn allows_recovery(&self, now_mono_secs: u64) -> bool {
        match self {
            Self::Closed | Self::HalfOpen { .. } => true,
            Self::Open { until_mono_secs, .. } => now_mono_secs >= *until_mono_secs,
        }
    }
}

// ─── Backup FSM ────────────────────────────────────────────────

/// Backup lifecycle phases. Each variant carries the data needed
/// for crash recovery: if the daemon dies and restarts, it reads
/// the persisted `BackupPhase` and knows exactly what to clean up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum BackupPhase {
    /// No backup in progress.
    Idle,

    /// Lock acquired, about to start.
    Locked {
        run_id: String,
    },

    /// Running `restic unlock` to clear stale locks.
    ResticUnlocking {
        run_id: String,
    },

    /// Stopping services that must be offline during backup.
    ServicesStopping {
        run_id: String,
        /// Snapshot of services that were running BEFORE we stopped them.
        /// On crash recovery, we restore exactly these — nothing more.
        pre_backup_state: Vec<ServiceSnapshot>,
    },

    /// `restic backup` is executing.
    ResticRunning {
        run_id: String,
        pre_backup_state: Vec<ServiceSnapshot>,
    },

    /// Starting services back up after backup.
    ServicesStarting {
        run_id: String,
        remaining: Vec<ServiceRestore>,
    },

    /// Verifying that restarted services pass health checks.
    ServicesVerifying {
        run_id: String,
        started: Vec<ServiceId>,
    },

    /// Running `restic forget --prune` for retention policy.
    RetentionRunning {
        run_id: String,
    },

    /// Running `restic check` for integrity verification.
    Verifying {
        run_id: String,
    },
}

impl BackupPhase {
    #[must_use]
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    /// Whether crash recovery should attempt to start stopped services.
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

    /// Whether crash recovery should run `restic unlock`.
    #[must_use]
    pub fn needs_restic_unlock(&self) -> bool {
        matches!(
            self,
            Self::ResticUnlocking { .. } | Self::ResticRunning { .. }
        )
    }

    /// Extract pre-backup service state for crash compensation.
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

/// Every input to the reducer. The control loop receives these
/// from various sources and feeds them into `reduce()`.
#[derive(Debug, Clone)]
pub enum Event {
    /// Scheduler determined these tasks are due.
    Tick {
        due_tasks: Vec<TaskKind>,
    },

    /// Health probes completed for all services.
    ProbeResults(Vec<ProbeResult>),

    /// An effect (external command) completed.
    EffectCompleted {
        cmd_id: u64,
        result: EffectResult,
    },

    /// An HTTP API request that requires mutation.
    HttpCommand(CommandRequest),

    /// OS signal received.
    Signal(SignalKind),

    /// Daemon just started — run crash recovery.
    StartupRecovery {
        persisted_backup: BackupPhase,
    },
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
#[derive(Debug, Clone)]
pub enum EffectResult {
    /// Command succeeded.
    Success {
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    /// Command failed with exit code.
    Failed {
        exit_code: i32,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    /// Command was killed (timeout or signal).
    Killed {
        signal: i32,
        duration_ms: u64,
    },
    /// Exec helper process itself failed.
    HelperError {
        message: String,
    },
}

/// Commands that can arrive from the HTTP API.
#[derive(Debug, Clone)]
pub enum CommandRequest {
    /// Manual task trigger (e.g., POST /trigger/backup).
    Trigger(TaskKind),
    /// AI remediation request.
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
    Shutdown,
    Reload,
}

// ─── Commands (output of reducer) ──────────────────────────────

/// Every output from the reducer. The effect executor receives
/// these and performs real-world side effects.
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
    RunBackupPhase {
        phase: BackupPhase,
    },
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

/// A structured alert that the notifier renders into NTFY format.
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
    /// NTFY wire format value.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupLevel {
    /// apt clean + journal vacuum.
    Standard,
    /// Standard + docker image prune -a (only if safe).
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