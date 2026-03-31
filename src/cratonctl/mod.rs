pub mod auth;
pub mod cli;
pub mod client;
pub mod dto;
pub mod error;
pub mod init;
pub mod output;

use crate::cratonctl::error::CratonctlError;
use serde::Serialize;
use std::io::IsTerminal;

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
    if let cli::Command::Help { text, top_level } = &parsed.command {
        return Ok(handle_help(&parsed.global, text, *top_level));
    }
    if let cli::Command::Init(args) = &parsed.command {
        return handle_init(args, &parsed.global);
    }

    let resolved = auth::resolve(&parsed.global)?;
    let client = client::Client::new(&resolved.url);

    match &parsed.command {
        cli::Command::Help { .. } => unreachable!("help commands return before client setup"),
        cli::Command::Init(_) => unreachable!("init commands return before client setup"),
        cli::Command::AuthStatus => handle_auth_status(&resolved, &parsed.global),
        cli::Command::Health => handle_health(&client, &parsed.global),
        cli::Command::Status => handle_status(&client, &parsed.global),
        cli::Command::Services => handle_services(&client, &parsed.global),
        cli::Command::Service { id } => handle_service(&client, &parsed.global, id),
        cli::Command::History { kind } => handle_history(&client, &parsed.global, *kind),
        cli::Command::Diagnose { service } => handle_diagnose(&client, &parsed.global, service),
        cli::Command::Doctor => handle_doctor(&client, &resolved, &parsed.global),
        cli::Command::Trigger { task } => handle_trigger(&client, &resolved, &parsed.global, task),
        cli::Command::Restart { service } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "RestartService",
            target: Some(service.as_str()),
            reason: None,
            label: "restart",
            display_target: Some(service.as_str()),
        }),
        cli::Command::DockerRestart { container } => handle_remediation_command(&client, &resolved, &parsed.global, RemediationSpec {
            action: "DockerRestart",
            target: Some(container.as_str()),
            reason: Some("operator"),
            label: "docker restart",
            display_target: Some(container.as_str()),
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
    let presentation = default_presentation(global);
    render(
        &health,
        global,
        || output::render_health(&health, presentation),
        || output::render_health_quiet(&health),
    )?;
    Ok(i32::from(!health.is_ok()))
}

fn handle_init(args: &init::InitArgs, global: &cli::GlobalArgs) -> Result<i32, CratonctlError> {
    let report = init::run(args)?;
    render(
        &report,
        global,
        || init::render_human(&report),
        || init::render_quiet(&report),
    )?;
    Ok(0)
}

fn handle_auth_status(
    resolved: &auth::ResolvedConfig,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let report = auth::auth_status(global, resolved);
    let presentation = auth_presentation(global);
    render(
        &report,
        global,
        || output::render_auth_status(&report, presentation),
        || output::render_auth_status_quiet(&report),
    )?;
    Ok(0)
}

fn handle_status(
    client: &client::Client,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let health = client.get_json::<dto::HealthResponse>("/health")?;
    let state = client.get_json::<dto::StateSnapshot>("/api/v1/state")?;
    let status = dto::StatusSummary::from_parts(health, &state);
    let presentation = default_presentation(global);
    render(
        &status,
        global,
        || output::render_status(&status, presentation),
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
    let presentation = default_presentation(global);
    render(
        &services,
        global,
        || output::render_services(&services, presentation),
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
    let presentation = default_presentation(global);
    render(
        &service,
        global,
        || output::render_service(&service, presentation),
        || output::render_service_quiet(&service),
    )?;
    Ok(0)
}

fn handle_history(
    client: &client::Client,
    global: &cli::GlobalArgs,
    kind: cli::HistoryKind,
) -> Result<i32, CratonctlError> {
    let presentation = default_presentation(global);
    match kind {
        cli::HistoryKind::Recovery => {
            let items = client.get_json::<Vec<dto::RecoveryRecordDto>>("/api/v1/history/recovery")?;
            render(
                &items,
                global,
                || output::render_recovery_history(&items, presentation),
                || output::render_recovery_history_quiet(&items),
            )?;
        }
        cli::HistoryKind::Backup => {
            let items = client.get_json::<Vec<dto::BackupRecordDto>>("/api/v1/history/backup")?;
            render(
                &items,
                global,
                || output::render_backup_history(&items, presentation),
                || output::render_backup_history_quiet(&items),
            )?;
        }
        cli::HistoryKind::Remediation => {
            let items =
                client.get_json::<Vec<dto::RemediationRecordDto>>("/api/v1/history/remediation")?;
            render(
                &items,
                global,
                || output::render_remediation_history(&items, presentation),
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
    let presentation = default_presentation(global);
    render(
        &diagnose,
        global,
        || output::render_diagnose(&diagnose, presentation),
        || output::render_diagnose_quiet(&diagnose),
    )?;
    Ok(0)
}

fn handle_doctor(
    client: &client::Client,
    resolved: &auth::ResolvedConfig,
    global: &cli::GlobalArgs,
) -> Result<i32, CratonctlError> {
    let mut checks = build_health_checks(client);
    let state_result = client.get_json::<dto::StateSnapshot>("/api/v1/state");
    match &state_result {
        Ok(state) => checks.push(dto::DoctorCheck {
            name: "GET /api/v1/state".into(),
            status: "ok".into(),
            code: "state_ok".into(),
            detail: format!("parsed state snapshot for {} services", state.services.len()),
        }),
        Err(error) => checks.push(dto::DoctorCheck {
            name: "GET /api/v1/state".into(),
            status: "fail".into(),
            code: "state_request_failed".into(),
            detail: error.message(),
        }),
    }

    let token_check = auth::diagnose_token(resolved);
    checks.push(dto::DoctorCheck {
        name: "token access".into(),
        status: token_check.status.into(),
        code: token_check.code.into(),
        detail: token_check.detail,
    });

    let read_only_ready = checks_read_only_ready(&checks) && state_result.is_ok();
    let mutating_ready = auth::require_token(resolved).is_ok();
    checks.push(read_only_check(read_only_ready));
    checks.push(mutating_check(resolved));

    let report = dto::DoctorReport {
        url: resolved.url.clone(),
        checks,
        read_only_ready,
        mutating_ready,
    };
    let presentation = doctor_presentation(global);
    render(
        &report,
        global,
        || output::render_doctor(&report, presentation),
        || output::render_doctor_quiet(&report),
    )?;
    Ok(i32::from(report.has_failures()))
}

fn build_health_checks(client: &client::Client) -> Vec<dto::DoctorCheck> {
    let health_result = client.get_json::<dto::HealthResponse>("/health");
    match health_result {
        Ok(health) => vec![
            dto::DoctorCheck {
                name: "daemon URL".into(),
                status: "ok".into(),
                code: "daemon_url_reachable".into(),
                detail: "daemon URL is reachable".into(),
            },
            dto::DoctorCheck {
                name: "GET /health".into(),
                status: if health.is_ok() { "ok" } else { "warn" }.into(),
                code: if health.is_ok() {
                    "health_ok".into()
                } else {
                    "health_unavailable".into()
                },
                detail: format!("status={} reason={}", health.status, health.reason),
            },
        ],
        Err(error) => vec![
            dto::DoctorCheck {
                name: "daemon URL".into(),
                status: if matches!(error, CratonctlError::Transport(_)) {
                    "fail"
                } else {
                    "ok"
                }
                .into(),
                code: if matches!(error, CratonctlError::Transport(_)) {
                    "daemon_url_unreachable".into()
                } else {
                    "daemon_url_reachable".into()
                },
                detail: error.message(),
            },
            dto::DoctorCheck {
                name: "GET /health".into(),
                status: "fail".into(),
                code: "health_request_failed".into(),
                detail: error.message(),
            },
        ],
    }
}

fn checks_read_only_ready(checks: &[dto::DoctorCheck]) -> bool {
    checks
        .iter()
        .find(|check| check.name == "GET /health")
        .is_some_and(|check| check.status != "fail")
}

fn read_only_check(read_only_ready: bool) -> dto::DoctorCheck {
    dto::DoctorCheck {
        name: "read-only commands".into(),
        status: if read_only_ready { "ok" } else { "fail" }.into(),
        code: if read_only_ready {
            "read_only_ready".into()
        } else {
            "read_only_not_ready".into()
        },
        detail: if read_only_ready {
            "basic read-only preconditions look good".into()
        } else {
            "one or more daemon API checks failed".into()
        },
    }
}

fn mutating_check(resolved: &auth::ResolvedConfig) -> dto::DoctorCheck {
    match auth::require_token(resolved) {
        Ok(_) => dto::DoctorCheck {
            name: "mutating commands".into(),
            status: "ok".into(),
            code: "mutating_ready".into(),
            detail: "token looks usable for mutating commands".into(),
        },
        Err(error) => dto::DoctorCheck {
            name: "mutating commands".into(),
            status: "warn".into(),
            code: "mutating_not_ready".into(),
            detail: error.message(),
        },
    }
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
        detail: response.detail,
    };
    let presentation = default_presentation(global);
    render(
        &result,
        global,
        || output::render_command_result(&result, presentation),
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
    let presentation = default_presentation(global);
    render(
        &result,
        global,
        || output::render_command_result(&result, presentation),
        || output::render_command_result_quiet(&result),
    )?;
    Ok(0)
}

fn handle_help(global: &cli::GlobalArgs, help_text: &str, top_level: bool) -> i32 {
    let base_help = if top_level {
        cli::usage()
    } else {
        help_text.to_owned()
    };
    let text = if global.json || global.quiet || !top_level {
        base_help
    } else {
        output::render_help(&base_help, help_presentation(global))
    };
    println!("{text}");
    0
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
        detail: response.detail,
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
                "code": error.code(),
                "message": error.message(),
                "exit_code": error.exit_code(),
            }
        });
        println!("{value}");
    } else {
        eprintln!("{error}");
    }
}

fn default_presentation(global: &cli::GlobalArgs) -> output::Presentation {
    output::Presentation {
        use_color: use_color(global),
        show_banner: false,
    }
}

fn doctor_presentation(global: &cli::GlobalArgs) -> output::Presentation {
    output::Presentation {
        use_color: use_color(global),
        show_banner: show_banner(global),
    }
}

fn help_presentation(global: &cli::GlobalArgs) -> output::Presentation {
    output::Presentation {
        use_color: use_color(global),
        show_banner: show_banner(global),
    }
}

fn auth_presentation(global: &cli::GlobalArgs) -> output::Presentation {
    output::Presentation {
        use_color: use_color(global),
        show_banner: show_banner(global),
    }
}

fn use_color(global: &cli::GlobalArgs) -> bool {
    !global.json && !global.no_color && std::io::stdout().is_terminal()
}

fn show_banner(global: &cli::GlobalArgs) -> bool {
    !global.json && !global.quiet && std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutating_check_reports_auth_reason_when_token_file_is_missing() {
        let resolved = auth::resolve(&cli::GlobalArgs {
            token_file: Some("missing-token-for-doctor-test".into()),
            ..cli::GlobalArgs::default()
        })
        .unwrap_or_else(|err| panic!("unexpected resolve error: {err}"));

        let check = mutating_check(&resolved);
        assert_eq!(check.status, "warn");
        assert_eq!(check.code, "mutating_not_ready");
        assert!(check.detail.contains("token file not found"));
    }

    #[test]
    fn mutating_check_reports_ready_when_inline_token_is_present() {
        let resolved = auth::resolve(&cli::GlobalArgs {
            token: Some("inline-token".into()),
            ..cli::GlobalArgs::default()
        })
        .unwrap_or_else(|err| panic!("unexpected resolve error: {err}"));

        let check = mutating_check(&resolved);
        assert_eq!(check.status, "ok");
        assert_eq!(check.code, "mutating_ready");
        assert_eq!(check.detail, "token looks usable for mutating commands");
    }
}





