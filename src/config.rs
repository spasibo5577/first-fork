//! TOML configuration with strict validation.
//!
//! Design: one `#[derive(Deserialize)]` struct tree that maps
//! directly to the TOML file. Validation happens after parsing,
//! not during — this gives better error messages.
//!
//! Unknown fields cause a hard error (serde `deny_unknown_fields`).
//! This catches typos in config files at startup, not in production.

use crate::model::{ProbeSpec, ServiceId, ServiceKind, Severity};
use serde::Deserialize;

/// Top-level configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CratonConfig {
    #[serde(default = "default_daemon")]
    pub daemon: DaemonConfig,

    pub ntfy: NtfyConfig,

    pub backup: BackupConfig,

    #[serde(default = "default_disk")]
    pub disk: DiskConfig,

    #[serde(default = "default_updates")]
    pub updates: UpdatesConfig,

    #[serde(default = "default_ai")]
    pub ai: AiConfig,

    /// `[[service]]` array in TOML.
    #[serde(rename = "service")]
    pub services: Vec<ServiceEntry>,
}

impl CratonConfig {
    /// Parses and validates configuration from a TOML string.
    pub fn from_toml(raw: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Validates internal consistency after parsing.
    fn validate(&self) -> Result<(), ConfigError> {
        // At least one service.
        if self.services.is_empty() {
            return Err(ConfigError::Validation(
                "no services defined".to_string(),
            ));
        }

        // Unique service IDs.
        let mut seen = std::collections::HashSet::new();
        for svc in &self.services {
            if !seen.insert(&svc.id) {
                return Err(ConfigError::Validation(format!(
                    "duplicate service id: {}",
                    svc.id
                )));
            }
        }

        // Validate dependency references.
        let known: std::collections::HashSet<&ServiceId> =
            self.services.iter().map(|s| &s.id).collect();
        for svc in &self.services {
            for dep in &svc.depends_on {
                if !known.contains(dep) {
                    return Err(ConfigError::Validation(format!(
                        "service {} depends on {} which is not defined",
                        svc.id, dep
                    )));
                }
            }
        }

        // Disk thresholds.
        if self.disk.warn_percent >= self.disk.critical_percent {
            return Err(ConfigError::Validation(format!(
                "disk warn_percent ({}) must be < critical_percent ({})",
                self.disk.warn_percent, self.disk.critical_percent
            )));
        }

        // Backup repo and password must be set.
        if self.backup.restic_repo.is_empty() {
            return Err(ConfigError::Validation(
                "backup.restic_repo is empty".to_string(),
            ));
        }
        if self.backup.restic_password_file.is_empty() {
            return Err(ConfigError::Validation(
                "backup.restic_password_file is empty".to_string(),
            ));
        }

        // NTFY URL must be set.
        if self.ntfy.url.is_empty() {
            return Err(ConfigError::Validation(
                "ntfy.url is empty".to_string(),
            ));
        }

        Ok(())
    }

    /// Finds a service by ID.
    #[must_use]
    pub fn find_service(&self, id: &ServiceId) -> Option<&ServiceEntry> {
        self.services.iter().find(|s| &s.id == id)
    }

    /// Returns all services that should be stopped during backup.
    #[must_use]
    pub fn backup_stop_services(&self) -> Vec<&ServiceEntry> {
        self.services.iter().filter(|s| s.backup_stop).collect()
    }
}

// ─── Sub-configs ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_true")]
    pub watchdog: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NtfyConfig {
    pub url: String,
    pub topic: String,
    #[serde(default = "default_ntfy_retries")]
    pub retries: Vec<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    #[serde(default = "default_backup_schedule")]
    pub schedule: String,
    pub restic_repo: String,
    pub restic_password_file: String,
    #[serde(default = "default_restic_binary")]
    pub restic_binary: String,
    #[serde(default = "default_retention")]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub verify: bool,
    #[serde(default = "default_verify_subset")]
    pub verify_subset_percent: u32,
    /// Paths to include in the backup.
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    #[serde(default = "default_retention_daily")]
    pub daily: u32,
    #[serde(default = "default_retention_weekly")]
    pub weekly: u32,
    #[serde(default = "default_retention_monthly")]
    pub monthly: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskConfig {
    #[serde(default = "default_disk_interval")]
    pub interval: String,
    #[serde(default = "default_warn_percent")]
    pub warn_percent: u32,
    #[serde(default = "default_critical_percent")]
    pub critical_percent: u32,
    #[serde(default)]
    pub predictive: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesConfig {
    #[serde(default = "default_apt_schedule")]
    pub apt_schedule: String,
    #[serde(default = "default_docker_schedule")]
    pub docker_schedule: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    #[serde(default = "default_ai_mode")]
    pub mode: String,
    #[serde(default)]
    pub picoclaw_url: String,
    #[serde(default = "default_context_path")]
    pub context_path: String,
    #[serde(default = "default_token_path")]
    pub token_path: String,
}

// ─── Service entry ─────────────────────────────────────────────

/// One `[[service]]` block in the TOML config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEntry {
    pub id: ServiceId,
    pub name: String,
    pub unit: String,
    pub kind: ServiceKind,
    pub probe: ProbeSpec,

    #[serde(default)]
    pub depends_on: Vec<ServiceId>,

    #[serde(default)]
    pub resources: Vec<String>,

    #[serde(default = "default_severity")]
    pub severity: Severity,

    #[serde(default = "default_startup_grace")]
    pub startup_grace_secs: u64,

    #[serde(default = "default_restart_cooldown")]
    pub restart_cooldown_secs: u64,

    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,

    #[serde(default = "default_breaker_window")]
    pub breaker_window_secs: u64,

    #[serde(default = "default_breaker_cooldown")]
    pub breaker_cooldown_secs: u64,

    #[serde(default)]
    pub backup_stop: bool,
}

// ─── Defaults ──────────────────────────────────────────────────

fn default_daemon() -> DaemonConfig {
    DaemonConfig {
        listen: default_listen(),
        watchdog: true,
        log_level: default_log_level(),
    }
}

fn default_listen() -> String {
    "127.0.0.1:18800".to_string()
}

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_ntfy_retries() -> Vec<u64> {
    vec![0, 5, 15, 60]
}

fn default_backup_schedule() -> String {
    "odd_days:04:00".to_string()
}

fn default_restic_binary() -> String {
    "/usr/bin/restic".to_string()
}

fn default_retention() -> RetentionConfig {
    RetentionConfig {
        daily: default_retention_daily(),
        weekly: default_retention_weekly(),
        monthly: default_retention_monthly(),
    }
}

fn default_retention_daily() -> u32 {
    7
}
fn default_retention_weekly() -> u32 {
    4
}
fn default_retention_monthly() -> u32 {
    6
}

fn default_verify_subset() -> u32 {
    5
}

fn default_disk() -> DiskConfig {
    DiskConfig {
        interval: default_disk_interval(),
        warn_percent: default_warn_percent(),
        critical_percent: default_critical_percent(),
        predictive: false,
    }
}

fn default_disk_interval() -> String {
    "6h".to_string()
}

fn default_warn_percent() -> u32 {
    85
}

fn default_critical_percent() -> u32 {
    95
}

fn default_updates() -> UpdatesConfig {
    UpdatesConfig {
        apt_schedule: default_apt_schedule(),
        docker_schedule: default_docker_schedule(),
    }
}

fn default_apt_schedule() -> String {
    "daily:09:00".to_string()
}

fn default_docker_schedule() -> String {
    "weekly:sun:10:00".to_string()
}

fn default_ai() -> AiConfig {
    AiConfig {
        mode: default_ai_mode(),
        picoclaw_url: String::new(),
        context_path: default_context_path(),
        token_path: default_token_path(),
    }
}

fn default_ai_mode() -> String {
    "disabled".to_string()
}

fn default_context_path() -> String {
    "/run/craton/llm_context.json".to_string()
}

fn default_token_path() -> String {
    "/var/lib/craton/remediation-token".to_string()
}

fn default_severity() -> Severity {
    Severity::Warning
}

fn default_startup_grace() -> u64 {
    15
}

fn default_restart_cooldown() -> u64 {
    60
}

fn default_max_restarts() -> u32 {
    3
}

fn default_breaker_window() -> u64 {
    3600
}

fn default_breaker_cooldown() -> u64 {
    3600
}

// ─── Errors ────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ConfigError {
    Parse(String),
    Validation(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "config parse error: {msg}"),
            Self::Validation(msg) => write!(f, "config validation error: {msg}"),
        }
    }
}

impl std::error::Error for ConfigError {}

// ─── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const MINIMAL_CONFIG: &str = r#"
[ntfy]
url = "http://127.0.0.1:8080"
topic = "granit"

[backup]
restic_repo = "/opt/restic-repo"
restic_password_file = "/root/.config/restic/passphrase"

[[service]]
id = "unbound"
name = "Unbound"
unit = "unbound.service"
kind = "systemd"

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 5335
query = "google.com"

[[service]]
id = "adguard"
name = "AdGuard Home"
unit = "AdGuardHome.service"
kind = "systemd"
depends_on = ["unbound"]

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 53
query = "google.com"
"#;

    #[test]
    fn parse_minimal_config() {
        let cfg = CratonConfig::from_toml(MINIMAL_CONFIG).expect("should parse");
        assert_eq!(cfg.services.len(), 2);
        assert_eq!(cfg.services[0].id.as_str(), "unbound");
        assert_eq!(cfg.services[1].id.as_str(), "adguard");
        assert_eq!(cfg.services[1].depends_on.len(), 1);
        assert_eq!(cfg.services[1].depends_on[0].as_str(), "unbound");
    }

    #[test]
    fn defaults_applied() {
        let cfg = CratonConfig::from_toml(MINIMAL_CONFIG).expect("should parse");
        assert_eq!(cfg.daemon.listen, "127.0.0.1:18800");
        assert_eq!(cfg.disk.warn_percent, 85);
        assert_eq!(cfg.disk.critical_percent, 95);
        assert_eq!(cfg.backup.retention.daily, 7);
        assert_eq!(cfg.ntfy.retries, vec![0, 5, 15, 60]);
    }

    #[test]
    fn empty_services_rejected() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"
"#;
        let result = CratonConfig::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_dependency_rejected() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[[service]]
id = "a"
name = "A"
unit = "a.service"
kind = "systemd"
depends_on = ["nonexistent"]

[service.probe]
type = "systemd_active"
"#;
        let result = CratonConfig::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_service_id_rejected() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[[service]]
id = "dup"
name = "First"
unit = "a.service"
kind = "systemd"

[service.probe]
type = "systemd_active"

[[service]]
id = "dup"
name = "Second"
unit = "b.service"
kind = "systemd"

[service.probe]
type = "systemd_active"
"#;
        let result = CratonConfig::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn disk_thresholds_validated() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[disk]
warn_percent = 95
critical_percent = 85

[[service]]
id = "x"
name = "X"
unit = "x.service"
kind = "systemd"

[service.probe]
type = "systemd_active"
"#;
        let result = CratonConfig::from_toml(toml);
        assert!(result.is_err());
    }

    #[test]
    fn http_probe_parses() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[[service]]
id = "ntfy"
name = "NTFY"
unit = "ntfy.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "http"
url = "http://127.0.0.1:8080/v1/health"
timeout_secs = 5
"#;
        let cfg = CratonConfig::from_toml(toml).expect("should parse");
        match &cfg.services[0].probe {
            ProbeSpec::Http {
                url, timeout_secs, ..
            } => {
                assert_eq!(url, "http://127.0.0.1:8080/v1/health");
                assert_eq!(*timeout_secs, 5);
            }
            other => panic!("expected Http probe, got {other:?}"),
        }
    }

    #[test]
    fn exec_probe_with_stdout_check() {
        let toml = r#"
[ntfy]
url = "http://localhost"
topic = "test"

[backup]
restic_repo = "/repo"
restic_password_file = "/pass"

[[service]]
id = "tailscale"
name = "Tailscale"
unit = "tailscaled.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "exec"
argv = ["tailscale", "status", "--json"]
timeout_secs = 10

[service.probe.expect_stdout]
kind = "json_field"
pointer = "/BackendState"
expected = "Running"
"#;
        let cfg = CratonConfig::from_toml(toml).expect("should parse");
        match &cfg.services[0].probe {
            ProbeSpec::Exec {
                argv,
                expect_stdout,
                ..
            } => {
                assert_eq!(argv, &["tailscale", "status", "--json"]);
                assert!(expect_stdout.is_some());
            }
            other => panic!("expected Exec probe, got {other:?}"),
        }
    }
}