pub mod auth;
pub mod cli;
pub mod client;
pub mod dto;
pub mod error;
pub mod output;

use crate::cratonctl::error::CratonctlError;
use serde::Serialize;

pub fn run(args: Vec<String>) -> i32 {
    let json_errors = args.iter().any(|arg| arg == "--json");
    match cli::parse(args).and_then(|parsed| run_cli(&parsed)) {
        Ok(code) => code,
        Err(err) => {
            emit_error(&err, json_errors);
            err.exit_code()
        }
    }
}

fn run_cli(parsed: &cli::Cli) -> Result<i32, CratonctlError> {
    let resolved = auth::resolve(&parsed.global)?;
    let client = client::Client::new(&resolved.url);

    match &parsed.command {
        cli::Command::Health => handle_health(&client, &parsed.global),
        cli::Command::Status => handle_status(&client, &parsed.global),
        cli::Command::Services => handle_services(&client, &parsed.global),
        cli::Command::Service { id } => handle_service(&client, &parsed.global, id),
        cli::Command::History { kind } => handle_history(&client, &parsed.global, *kind),
        cli::Command::Diagnose { service } => handle_diagnose(&client, &parsed.global, service),
        cli::Command::Trigger { task } => handle_trigger(&client, &resolved, &parsed.global, task),
        cli::Command::Restart { service } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "RestartService",
            target: Some(service.as_str()),
            reason: None,
            label: "restart",
            display_target: Some(service.as_str()),
        }),
        cli::Command::MaintenanceSet { service, reason } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "MarkMaintenance",
            target: Some(service.as_str()),
            reason: Some(reason.as_str()),
            label: "maintenance set",
            display_target: Some(service.as_str()),
        }),
        cli::Command::MaintenanceClear { service } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "ClearMaintenance",
            target: Some(service.as_str()),
            reason: None,
            label: "maintenance clear",
            display_target: Some(service.as_str()),
        }),
        cli::Command::BreakerClear { service } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "ClearBreaker",
            target: Some(service.as_str()),
            reason: None,
            label: "breaker clear",
            display_target: Some(service.as_str()),
        }),
        cli::Command::FlappingClear { service } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "ClearFlapping",
            target: Some(service.as_str()),
            reason: None,
            label: "flapping clear",
            display_target: Some(service.as_str()),
        }),
        cli::Command::BackupRun => handle_trigger(&client, &resolved, &parsed.global, "backup"),
        cli::Command::BackupUnlock => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "ResticUnlock",
            target: None,
            reason: None,
            label: "backup unlock",
            display_target: None,
        }),
        cli::Command::DiskCleanup => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "RunDiskCleanup",
            target: None,
            reason: None,
            label: "disk cleanup",
            display_target: None,
        }),
    }
}

#[derive(Clone, Copy)]
struct RemediationSpec<'a> {
    action: &'a str,
    target: Option<&'a str>,
    reason: Option<&'a str>,
    label: &'a str,
    display_target: Option<&'a str>,
}

fn handle_health(
    client: &client::Client,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let health = client.get_json::<dto::HealthResponse>("/health")?;
    render(
        &health,
        global,
        || output::render_health(&health),
        || output::render_health_quiet(&health),
    )?;
    Ok(i32::from(!health.is_ok()))
}

fn handle_status(
    client: &client::Client,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let health = client.get_json::<dto::HealthResponse>("/health")?;
    let state = client.get_json::<dto::StateSnapshot>("/api/v1/state")?;
    let status = dto::StatusSummary::from_parts(health, &state);
    render(
        &status,
        global,
        || output::render_status(&status),
        || output::render_status_quiet(&status),
    )?;
    Ok(i32::from(!status.health.is_ok()))
}

fn handle_services(
    client: &client::Client,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let state = client.get_json::<dto::StateSnapshot>("/api/v1/state")?;
    let services = dto::ServiceSummary::list_from_snapshot(state);
    render(
        &services,
        global,
        || output::render_services(&services),
        || output::render_services_quiet(&services),
    )?;
    Ok(0)
}

fn handle_service(
    client: &client::Client,
    global: &cli::GlobalArgs,
    id: &str,
) -> Result<i32, CratonctlError> {
    let state = client.get_json::<dto::StateSnapshot>("/api/v1/state")?;
    let service = dto::ServiceDetail::from_snapshot(&state, id)
        .ok_or_else(|| CratonctlError::Daemon(format!("service not found: {id}")))?;
    render(
        &service,
        global,
        || output::render_service(&service),
        || output::render_service_quiet(&service),
    )?;
    Ok(0)
}

fn handle_history(
    client: &client::Client,
    global: &cli::GlobalArgs,
    kind: cli::HistoryKind,
) -> Result<i32, CratonctlError> {
    match kind {
        cli::HistoryKind::Recovery => {
            let items = client.get_json::<Vec<dto::RecoveryRecordDto>>("/api/v1/history/recovery")?;
            render(
                &items,
                global,
                || output::render_recovery_history(&items),
                || output::render_recovery_history_quiet(&items),
            )?;
        }
        cli::HistoryKind::Backup => {
            let items = client.get_json::<Vec<dto::BackupRecordDto>>("/api/v1/history/backup")?;
            render(
                &items,
                global,
                || output::render_backup_history(&items),
                || output::render_backup_history_quiet(&items),
            )?;
        }
        cli::HistoryKind::Remediation => {
            let items =
                client.get_json::<Vec<dto::RemediationRecordDto>>("/api/v1/history/remediation")?;
            render(
                &items,
                global,
                || output::render_remediation_history(&items),
                || output::render_remediation_history_quiet(&items),
            )?;
        }
    }
    Ok(0)
}

fn handle_diagnose(
    client: &client::Client,
    global: &cli::GlobalArgs,
    service: &str,
) -> Result<i32, CratonctlError> {
    let path = format!("/api/v1/diagnose/{}", client::path_segment(service));
    let diagnose = client.get_json::<dto::DiagnoseResponse>(&path)?;
    render(
        &diagnose,
        global,
        || output::render_diagnose(&diagnose),
        || output::render_diagnose_quiet(&diagnose),
    )?;
    Ok(0)
}

fn handle_trigger(
    client: &client::Client,
    resolved: &auth::ResolvedConfig,
    global: &cli::GlobalArgs,
    task: &str,
) -> Result<i32, CratonctlError> {
    let token = auth::require_token(resolved)?;
    let path = format!("/trigger/{}", client::path_segment(task));
    let response = client.post_json::<dto::CommandAcceptedResponse>(&path, "{}", token)?;
    let result = dto::CommandResult {
        action: "trigger".into(),
        status: response.status,
        target: response.task.or_else(|| Some(task.into())),
        detail: None,
    };
    render(
        &result,
        global,
        || output::render_command_result(&result),
        || output::render_command_result_quiet(&result),
    )?;
    Ok(0)
}

fn handle_remediation_command(
    client: &client::Client,
    resolved: &auth::ResolvedConfig,
    global: &cli::GlobalArgs,
    spec: RemediationSpec<'_>,
) -> Result<i32, CratonctlError> {
    let result = remediate(
        client,
        resolved,
        spec.action,
        spec.target,
        spec.reason,
        spec.label,
        spec.display_target,
    )?;
    render(
        &result,
        global,
        || output::render_command_result(&result),
        || output::render_command_result_quiet(&result),
    )?;
    Ok(0)
}

fn remediate(
    client: &client::Client,
    resolved: &auth::ResolvedConfig,
    action: &str,
    target: Option<&str>,
    reason: Option<&str>,
    label: &str,
    display_target: Option<&str>,
) -> Result<dto::CommandResult, CratonctlError> {
    let token = auth::require_token(resolved)?;
    let body = build_remediation_body(action, target, reason);
    let response =
        client.post_json::<dto::CommandAcceptedResponse>("/api/v1/remediate", &body, token)?;
    Ok(dto::CommandResult {
        action: label.into(),
        status: response.status,
        target: display_target.map(str::to_string),
        detail: None,
    })
}

fn build_remediation_body(action: &str, target: Option<&str>, reason: Option<&str>) -> String {
    let mut value = serde_json::json!({ "action": action });
    if let Some(target) = target {
        value["target"] = serde_json::Value::String(target.into());
    }
    if let Some(reason) = reason {
        value["reason"] = serde_json::Value::String(reason.into());
    }
    value.to_string()
}

fn render<T, F, Q>(
    value: &T,
    global: &cli::GlobalArgs,
    human: F,
    quiet: Q,
) -> Result<(), CratonctlError>
where
    T: Serialize,
    F: FnOnce() -> String,
    Q: FnOnce() -> String,
{
    if global.json {
        let json = serde_json::to_string_pretty(value)
            .map_err(|err| CratonctlError::Parse(format!("failed to serialize JSON output: {err}")))?;
        println!("{json}");
    } else {
        let text = if global.quiet { quiet() } else { human() };
        if !text.is_empty() {
            println!("{text}");
        }
    }
    Ok(())
}

fn emit_error(error: &CratonctlError, json_errors: bool) {
    if json_errors {
        let value = serde_json::json!({
            "error": {
                "kind": error.kind(),
                "message": error.message(),
                "exit_code": error.exit_code(),
            }
        });
        println!("{value}");
    } else {
        eprintln!("{error}");
    }
}
