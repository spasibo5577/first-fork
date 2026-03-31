use crate::cratonctl::error::CratonctlError;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_CONFIG_DIR: &str = "/etc/craton";
pub const DEFAULT_STATE_DIR: &str = "/var/lib/craton";
pub const DEFAULT_RUNTIME_DIR: &str = "/run/craton";
pub const DEFAULT_UNIT_PATH: &str = "/etc/systemd/system/cratond.service";
pub const TOKEN_FILE_NAME: &str = "remediation-token";
const CONFIG_FILE_NAME: &str = "config.toml";
const CONFIG_TEMPLATE: &str = r#"# Craton initial configuration template.
# Review this file before starting cratond for the first time.

[daemon]
# Local HTTP API bind address.
listen = "127.0.0.1:18800"

# systemd watchdog integration.
watchdog = true

# Future structured log level.
log_level = "info"

{{NTFY_SECTION}}

[backup]
# Backup schedule: "odd_days:HH:MM" or "daily:HH:MM".
schedule = "odd_days:04:00"

# TODO: point this to your restic repository.
restic_repo = "/var/backups/restic"

# TODO: point this to your restic password file.
restic_password_file = "/root/.config/restic/passphrase"

restic_binary = "/usr/bin/restic"
verify = true
verify_subset_percent = 5
paths = [
    "/etc",
    "{{STATE_DIR}}",
    "/root/.config",
]

[backup.retention]
daily = 7
weekly = 4
monthly = 6

[disk]
interval = "6h"
warn_percent = 85
critical_percent = 95
predictive = false

[updates]
apt_schedule = "daily:09:00"
docker_schedule = "weekly:sun:10:00"

[ai]
mode = "disabled"
picoclaw_url = ""
context_path = "/run/craton/llm_context.json"
token_path = "{{TOKEN_PATH}}"

[[service]]
id = "unbound"
name = "Unbound DNS"
unit = "unbound.service"
kind = "systemd"
severity = "critical"
startup_grace_secs = 15
restart_cooldown_secs = 60
max_restarts = 3
breaker_window_secs = 3600
breaker_cooldown_secs = 3600
backup_stop = false

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 5335
query = "google.com"
timeout_secs = 5

[[service]]
id = "adguard"
name = "AdGuard Home"
unit = "AdGuardHome.service"
kind = "systemd"
severity = "critical"
depends_on = ["unbound"]
backup_stop = false

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 53
query = "google.com"
timeout_secs = 5
"#;
const NTFY_SECTION_ENABLED: &str = r#"[ntfy]
# ntfy topic URL is split into a base URL and topic.
url = "{{NTFY_BASE_URL}}"
topic = "{{NTFY_TOPIC}}"
retries = [0, 5, 15, 60]"#;
const NTFY_SECTION_COMMENTED: &str = r#"# [ntfy]
# Optional notification channel.
# Uncomment and set your ntfy topic URL before relying on alerts.
# url = "http://127.0.0.1:8080"
# topic = "craton-alerts"
# retries = [0, 5, 15, 60]"#;
const UNIT_TEMPLATE: &str = include_str!("../../deploy/cratond.service");
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_RELOAD_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitArgs {
    pub non_interactive: bool,
    pub config_dir: Option<String>,
    pub state_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitReport {
    pub config: ComponentReport,
    pub state_dir: ComponentReport,
    pub runtime_dir: ComponentReport,
    pub token: ComponentReport,
    pub unit: ComponentReport,
    pub daemon_reload: ComponentReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentReport {
    pub path: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct InitPaths {
    config_dir: PathBuf,
    config_path: PathBuf,
    state_dir: PathBuf,
    runtime_dir: PathBuf,
    token_path: PathBuf,
    unit_path: PathBuf,
}

pub fn run(args: &InitArgs) -> Result<InitReport, CratonctlError> {
    ensure_root()?;
    ensure_systemctl_available()?;

    let paths = resolve_paths(args)?;
    let config_exists = paths.config_path.exists();
    let token_exists = paths.token_path.exists();
    let unit_exists = paths.unit_path.exists();

    let state_dir = ensure_directory(&paths.state_dir, 0o700)?;
    let runtime_dir = ensure_directory(&paths.runtime_dir, 0o755)?;
    let _config_dir = ensure_directory(&paths.config_dir, 0o755)?;

    let effective_non_interactive = args.non_interactive || !io::stdin().is_terminal();
    let config = if config_exists {
        ComponentReport {
            path: display_path(&paths.config_path),
            status: "skipped".into(),
            detail: "already exists".into(),
        }
    } else {
        let ntfy = if effective_non_interactive {
            None
        } else {
            prompt_ntfy_topic_url(&paths.config_path)? 
        };
        let content = render_config_template(ntfy.as_ref(), &paths);
        atomic_write_text(&paths.config_path, &content, 0o600)?;
        ComponentReport {
            path: display_path(&paths.config_path),
            status: "created".into(),
            detail: if ntfy.is_some() {
                "written with ntfy settings".into()
            } else if effective_non_interactive {
                "written with commented ntfy section".into()
            } else {
                "written with ntfy section commented out".into()
            },
        }
    };

    let token = if token_exists {
        ensure_existing_file_mode(&paths.token_path, 0o600, "token file")?
    } else {
        let token = generate_token_hex()?;
        atomic_write_text(&paths.token_path, &token, 0o600)?;
        ComponentReport {
            path: display_path(&paths.token_path),
            status: "created".into(),
            detail: "generated 32-byte remediation token".into(),
        }
    };

    let (unit, daemon_reload) = if unit_exists {
        (
            ComponentReport {
                path: display_path(&paths.unit_path),
                status: "skipped".into(),
                detail: "already exists".into(),
            },
            ComponentReport {
                path: "systemctl daemon-reload".into(),
                status: "skipped".into(),
                detail: "unit unchanged".into(),
            },
        )
    } else {
        let unit_contents = render_unit_template(&paths);
        atomic_write_text(&paths.unit_path, &unit_contents, 0o644)?;
        run_command_with_timeout("systemctl", &["daemon-reload"], DAEMON_RELOAD_TIMEOUT)?;
        (
            ComponentReport {
                path: display_path(&paths.unit_path),
                status: "created".into(),
                detail: "installed systemd unit".into(),
            },
            ComponentReport {
                path: "systemctl daemon-reload".into(),
                status: "created".into(),
                detail: "reloaded systemd units".into(),
            },
        )
    };

    Ok(InitReport {
        config,
        state_dir,
        runtime_dir,
        token,
        unit,
        daemon_reload,
    })
}

pub fn render_human(report: &InitReport) -> String {
    let mut lines = vec![
        format!("{} Config:     {}", symbol(&report.config.status), report.config.path),
        format!(
            "{} State dir:  {}{}",
            symbol(&report.state_dir.status),
            report.state_dir.path,
            detail_suffix(&report.state_dir)
        ),
        format!(
            "{} Runtime:    {}{}",
            symbol(&report.runtime_dir.status),
            report.runtime_dir.path,
            detail_suffix(&report.runtime_dir)
        ),
        format!(
            "{} Token:      {}{}",
            symbol(&report.token.status),
            report.token.path,
            detail_suffix(&report.token)
        ),
        format!("{} Unit:       {}", symbol(&report.unit.status), report.unit.path),
    ];

    if report.daemon_reload.status == "created" {
        lines.push(format!(
            "{} systemctl daemon-reload",
            symbol(&report.daemon_reload.status)
        ));
    }

    lines.push(String::new());
    lines.push("Next steps:".into());
    lines.push(format!("  1. Review config:    nano {}", report.config.path));
    lines.push("  2. Start daemon:     systemctl start cratond".into());
    lines.push("  3. Enable on boot:   systemctl enable cratond".into());
    lines.push("  4. Verify:           cratonctl health".into());
    lines.join("\n")
}

pub fn render_quiet(report: &InitReport) -> String {
    [
        format!("config={}", report.config.status),
        format!("state_dir={}", report.state_dir.status),
        format!("runtime_dir={}", report.runtime_dir.status),
        format!("token={}", report.token.status),
        format!("unit={}", report.unit.status),
    ]
    .join(" ")
}

fn resolve_paths(args: &InitArgs) -> Result<InitPaths, CratonctlError> {
    if let Some(config_dir) = &args.config_dir {
        validate_path_override("config-dir", config_dir)?;
    }
    if let Some(state_dir) = &args.state_dir {
        validate_path_override("state-dir", state_dir)?;
    }

    let config_dir = PathBuf::from(args.config_dir.as_deref().unwrap_or(DEFAULT_CONFIG_DIR));
    let state_dir = PathBuf::from(args.state_dir.as_deref().unwrap_or(DEFAULT_STATE_DIR));
    Ok(InitPaths {
        config_path: config_dir.join(CONFIG_FILE_NAME),
        config_dir,
        state_dir: state_dir.clone(),
        runtime_dir: PathBuf::from(DEFAULT_RUNTIME_DIR),
        token_path: state_dir.join(TOKEN_FILE_NAME),
        unit_path: PathBuf::from(DEFAULT_UNIT_PATH),
    })
}

fn ensure_root() -> Result<(), CratonctlError> {
    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(CratonctlError::Config(
                "init must be run as root".into(),
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(CratonctlError::Config(
            "init is only supported on unix-like systems".into(),
        ))
    }
}

fn ensure_systemctl_available() -> Result<(), CratonctlError> {
    run_command_with_timeout("systemctl", &["--version"], SYSTEMCTL_TIMEOUT)
}

fn validate_path_override(flag: &str, value: &str) -> Result<(), CratonctlError> {
    if value.chars().any(|ch| matches!(ch, '\n' | '\r' | '\0')) {
        return Err(CratonctlError::Config(format!(
            "--{flag} must not contain newlines or NUL bytes"
        )));
    }

    if !Path::new(value).is_absolute() {
        return Err(CratonctlError::Config(format!(
            "--{flag} must be an absolute path"
        )));
    }

    Ok(())
}

fn ensure_directory(path: &Path, mode: u32) -> Result<ComponentReport, CratonctlError> {
    if path.exists() {
        let metadata = fs::metadata(path).map_err(|err| {
            CratonctlError::Config(format!("failed to inspect {}: {err}", path.display()))
        })?;
        if !metadata.is_dir() {
            return Err(CratonctlError::Config(format!(
                "path exists but is not a directory: {}",
                path.display()
            )));
        }
        if directory_mode(&metadata) == Some(mode) {
            return Ok(ComponentReport {
                path: display_path(path),
                status: "skipped".into(),
                detail: "already exists".into(),
            });
        }
        set_mode(path, mode)?;
        return Ok(ComponentReport {
            path: display_path(path),
            status: "created".into(),
            detail: format!("permissions fixed to {mode:04o}"),
        });
    }

    fs::create_dir_all(path).map_err(|err| {
        CratonctlError::Config(format!("failed to create directory {}: {err}", path.display()))
    })?;
    set_mode(path, mode)?;
    Ok(ComponentReport {
        path: display_path(path),
        status: "created".into(),
        detail: "created".into(),
    })
}

fn directory_mode(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        Some(metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

fn ensure_existing_file_mode(
    path: &Path,
    mode: u32,
    label: &str,
) -> Result<ComponentReport, CratonctlError> {
    let metadata = fs::metadata(path).map_err(|err| {
        CratonctlError::Config(format!("failed to inspect {}: {err}", path.display()))
    })?;
    if !metadata.is_file() {
        return Err(CratonctlError::Config(format!(
            "path exists but is not a file: {}",
            path.display()
        )));
    }
    if directory_mode(&metadata) == Some(mode) {
        return Ok(ComponentReport {
            path: display_path(path),
            status: "skipped".into(),
            detail: "already exists".into(),
        });
    }
    set_mode(path, mode)?;
    Ok(ComponentReport {
        path: display_path(path),
        status: "skipped".into(),
        detail: format!("fixed permissions on existing {label}: {mode:04o}"),
    })
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn set_mode(path: &Path, mode: u32) -> Result<(), CratonctlError> {
    #[cfg(unix)]
    {
        let permissions = fs::Permissions::from_mode(mode);
        fs::set_permissions(path, permissions).map_err(|err| {
            CratonctlError::Config(format!(
                "failed to set permissions on {}: {err}",
                path.display()
            ))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

fn prompt_ntfy_topic_url(config_path: &Path) -> Result<Option<NtfyTopicUrl>, CratonctlError> {
    eprint!("Creating {}...\\n  ntfy topic URL [skip]: ", config_path.display());
    io::stderr()
        .flush()
        .map_err(|err| CratonctlError::Config(format!("failed to flush prompt: {err}")))?;

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|err| CratonctlError::Config(format!("failed to read stdin: {err}")))?;
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("skip") {
        return Ok(None);
    }
    parse_ntfy_topic_url(trimmed).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NtfyTopicUrl {
    base_url: String,
    topic: String,
}

fn parse_ntfy_topic_url(value: &str) -> Result<NtfyTopicUrl, CratonctlError> {
    if value.chars().any(|ch| ch == '"' || matches!(ch, '\n' | '\r' | '\0') || ch.is_control()) {
        return Err(CratonctlError::Config(
            "ntfy topic URL contains unsupported characters".into(),
        ));
    }

    let without_scheme = value
        .strip_prefix("http://")
        .ok_or_else(|| CratonctlError::Config("ntfy topic URL must start with http://".into()))?;
    let slash = without_scheme.rfind('/').ok_or_else(|| {
        CratonctlError::Config(
            "ntfy topic URL must include a topic path, for example http://127.0.0.1:8080/craton-alerts"
                .into(),
        )
    })?;
    let host = &without_scheme[..slash];
    let topic = &without_scheme[slash + 1..];
    if host.is_empty() || topic.is_empty() {
        return Err(CratonctlError::Config(
            "ntfy topic URL must include both host and topic".into(),
        ));
    }
    Ok(NtfyTopicUrl {
        base_url: format!("http://{host}"),
        topic: topic.into(),
    })
}

fn render_config_template(ntfy: Option<&NtfyTopicUrl>, paths: &InitPaths) -> String {
    let ntfy_section = ntfy.map_or_else(
        || NTFY_SECTION_COMMENTED.to_string(),
        |value| {
            NTFY_SECTION_ENABLED
                .replace("{{NTFY_BASE_URL}}", &value.base_url)
                .replace("{{NTFY_TOPIC}}", &value.topic)
        },
    );

    CONFIG_TEMPLATE
        .replace("{{NTFY_SECTION}}", &ntfy_section)
        .replace("{{STATE_DIR}}", &display_path(&paths.state_dir))
        .replace("{{TOKEN_PATH}}", &display_path(&paths.token_path))
}

fn render_unit_template(paths: &InitPaths) -> String {
    let mut rendered = UNIT_TEMPLATE.replace(
        "ExecStart=/usr/local/bin/cratond /etc/craton/config.toml",
        &format!("ExecStart=/usr/local/bin/cratond {}", display_path(&paths.config_path)),
    );
    rendered = rendered.replace(
        "ReadWritePaths=/var/lib/craton /run/craton /root/.picoclaw",
        &format!(
            "ReadWritePaths={} {} /root/.picoclaw",
            display_path(&paths.state_dir),
            display_path(&paths.runtime_dir)
        ),
    );
    if paths.state_dir != Path::new(DEFAULT_STATE_DIR) {
        rendered = rendered.replace(
            "StateDirectory=craton\nStateDirectoryMode=0750\n",
            "",
        );
    }
    rendered
}

fn atomic_write_text(path: &Path, content: &str, mode: u32) -> Result<(), CratonctlError> {
    let parent = path.parent().ok_or_else(|| {
        CratonctlError::Config(format!("path has no parent directory: {}", path.display()))
    })?;
    let temp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("craton"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let temp_path = parent.join(temp_name);

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|err| {
            CratonctlError::Config(format!(
                "failed to create temporary file {}: {err}",
                temp_path.display()
            ))
        })?;
    file.write_all(content.as_bytes()).map_err(|err| {
        CratonctlError::Config(format!("failed to write {}: {err}", temp_path.display()))
    })?;
    file.sync_all().map_err(|err| {
        CratonctlError::Config(format!("failed to sync {}: {err}", temp_path.display()))
    })?;
    drop(file);
    set_mode(&temp_path, mode)?;
    fs::rename(&temp_path, path).map_err(|err| {
        CratonctlError::Config(format!(
            "failed to rename {} to {}: {err}",
            temp_path.display(),
            path.display()
        ))
    })?;
    sync_directory(parent)?;
    Ok(())
}

#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn sync_directory(path: &Path) -> Result<(), CratonctlError> {
    #[cfg(unix)]
    {
        let dir = File::open(path).map_err(|err| {
            CratonctlError::Config(format!("failed to open directory {}: {err}", path.display()))
        })?;
        dir.sync_all().map_err(|err| {
            CratonctlError::Config(format!("failed to sync directory {}: {err}", path.display()))
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn generate_token_hex() -> Result<String, CratonctlError> {
    let mut bytes = [0u8; 32];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|err| CratonctlError::Config(format!("failed to read /dev/urandom: {err}")))?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn run_command_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<(), CratonctlError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                CratonctlError::Config(format!("{program} is not available on PATH"))
            } else {
                CratonctlError::Config(format!("failed to spawn {program}: {err}"))
            }
        })?;

    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            CratonctlError::Config(format!("failed to wait for {program}: {err}"))
        })? {
            if status.success() {
                return Ok(());
            }
            return Err(CratonctlError::Config(format!(
                "{program} {} failed with status {status}",
                args.join(" ")
            )));
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CratonctlError::Config(format!(
                "{program} {} timed out after {}s",
                args.join(" "),
                timeout.as_secs()
            )));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn symbol(status: &str) -> &'static str {
    match status {
        "skipped" => "-",
        _ => "✓",
    }
}

fn detail_suffix(component: &ComponentReport) -> String {
    if component.detail.starts_with("permissions fixed")
        || component.detail.starts_with("fixed permissions on existing")
    {
        format!(" ({})", component.detail)
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_template_with_commented_ntfy_is_valid_toml() {
        let rendered = render_config_template(None, &resolve_paths(&InitArgs {
            non_interactive: true,
            config_dir: None,
            state_dir: None,
        })
        .unwrap_or_else(|err| panic!("unexpected resolve error: {err}")));
        let parsed: toml::Value =
            toml::from_str(&rendered).unwrap_or_else(|err| panic!("unexpected toml error: {err}"));
        assert!(parsed.get("daemon").is_some());
        assert!(parsed.get("backup").is_some());
    }

    #[test]
    fn unit_template_contains_expected_directives() {
        assert!(UNIT_TEMPLATE.contains("Type=notify"));
        assert!(UNIT_TEMPLATE.contains("WatchdogSec=30"));
        assert!(UNIT_TEMPLATE.contains("ExecStart=/usr/local/bin/cratond"));
    }

    #[test]
    fn token_generation_produces_64_char_hex() {
        let token = hex_encode(&[0x12; 32]);

        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_ntfy_topic_url_splits_base_and_topic() {
        let parsed = parse_ntfy_topic_url("http://127.0.0.1:8080/craton-alerts")
            .unwrap_or_else(|err| panic!("unexpected parse error: {err}"));
        assert_eq!(parsed.base_url, "http://127.0.0.1:8080");
        assert_eq!(parsed.topic, "craton-alerts");
    }

    #[test]
    fn parse_ntfy_topic_url_rejects_quotes() {
        assert!(matches!(
            parse_ntfy_topic_url("http://127.0.0.1:8080/bad\"topic"),
            Err(CratonctlError::Config(_))
        ));
    }

    #[test]
    fn resolve_paths_rejects_relative_config_dir() {
        let result = resolve_paths(&InitArgs {
            non_interactive: true,
            config_dir: Some("relative/config".into()),
            state_dir: None,
        });
        assert!(matches!(result, Err(CratonctlError::Config(_))));
    }

    #[test]
    fn resolve_paths_rejects_newline_in_state_dir() {
        let value = if cfg!(windows) {
            String::from(r"C:\bad
path")
        } else {
            String::from("/bad\npath")
        };
        let result = resolve_paths(&InitArgs {
            non_interactive: true,
            config_dir: None,
            state_dir: Some(value),
        });
        assert!(matches!(result, Err(CratonctlError::Config(_))));
    }

    #[cfg(unix)]
    #[test]
    fn existing_token_permissions_are_fixed() {
        let temp_root = std::env::temp_dir().join(format!(
            "cratonctl-init-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&temp_root)
            .unwrap_or_else(|err| panic!("unexpected dir error: {err}"));
        let token_path = temp_root.join("remediation-token");
        fs::write(&token_path, "token")
            .unwrap_or_else(|err| panic!("unexpected file error: {err}"));
        set_mode(&token_path, 0o644)
            .unwrap_or_else(|err| panic!("unexpected mode error: {err}"));

        let report = ensure_existing_file_mode(&token_path, 0o600, "token file")
            .unwrap_or_else(|err| panic!("unexpected ensure error: {err}"));

        assert_eq!(report.status, "skipped");
        assert_eq!(report.detail, "fixed permissions on existing token file: 0600");
        let metadata = fs::metadata(&token_path)
            .unwrap_or_else(|err| panic!("unexpected metadata error: {err}"));
        assert_eq!(directory_mode(&metadata), Some(0o600));

        fs::remove_file(&token_path).ok();
        fs::remove_dir_all(&temp_root).ok();
    }

}
