use crate::cratonctl::dto::{
    BackupRecordDto, CommandResult, DiagnoseResponse, HealthResponse, RecoveryRecordDto,
    RemediationRecordDto, ServiceDetail, ServiceSummary, StatusSummary,
};

pub fn render_health(health: &HealthResponse) -> String {
    if health.is_ok() {
        "ok".into()
    } else {
        format!("unavailable: {}", health.reason)
    }
}

pub fn render_health_quiet(health: &HealthResponse) -> String {
    render_health(health)
}

pub fn render_status(status: &StatusSummary) -> String {
    let disk = status
        .disk_usage_percent
        .map_or_else(|| "unknown".into(), |value| format!("{value}%"));
    let mut lines = vec![
        format!("health: {} ({})", status.health.status, status.health.reason),
        format!(
            "services: {} total, {} degraded",
            status.service_count, status.degraded_count
        ),
        format!("backup: {}", status.backup_phase),
        format!("disk: {disk}"),
    ];

    if status.shutting_down {
        lines.push("daemon: shutting down".into());
    }
    if status.outbox_overflow {
        lines.push("alerts: outbox overflow".into());
    }

    lines.join("\n")
}

pub fn render_status_quiet(status: &StatusSummary) -> String {
    format!(
        "{} services={} degraded={} backup={}",
        status.health.status, status.service_count, status.degraded_count, status.backup_phase
    )
}

pub fn render_services(services: &[ServiceSummary]) -> String {
    let rows: Vec<Vec<String>> = services
        .iter()
        .map(|service| {
            vec![
                service.id.clone(),
                service.status.clone(),
                service.summary.clone(),
            ]
        })
        .collect();
    render_table(&["SERVICE", "STATUS", "SUMMARY"], &rows)
}

pub fn render_services_quiet(services: &[ServiceSummary]) -> String {
    services
        .iter()
        .map(|service| format!("{}\t{}", service.id, service.status))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_service(service: &ServiceDetail) -> String {
    let mut lines = vec![
        format!("service: {}", service.id),
        format!("status: {}", service.status),
        format!("summary: {}", service.summary),
    ];

    if let Some(since_mono) = service.since_mono {
        lines.push(format!("since_mono: {since_mono}"));
    }

    match &service.raw_status {
        crate::cratonctl::dto::ServiceStatusDto::Suppressed { until_mono } => {
            lines.push(format!("until_mono: {until_mono}"));
        }
        crate::cratonctl::dto::ServiceStatusDto::BlockedByDep { root } => {
            lines.push(format!("root_dependency: {root}"));
        }
        crate::cratonctl::dto::ServiceStatusDto::Unhealthy { consecutive, .. } => {
            lines.push(format!("consecutive_failures: {consecutive}"));
        }
        crate::cratonctl::dto::ServiceStatusDto::Unknown
        | crate::cratonctl::dto::ServiceStatusDto::Healthy { .. }
        | crate::cratonctl::dto::ServiceStatusDto::Recovering { .. }
        | crate::cratonctl::dto::ServiceStatusDto::Failed { .. } => {}
    }

    lines.join("\n")
}

pub fn render_service_quiet(service: &ServiceDetail) -> String {
    format!("{}\t{}", service.id, service.status)
}

pub fn render_recovery_history(items: &[RecoveryRecordDto]) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                csv_or_dash(&item.recovered),
                csv_or_dash(&item.failed),
                yes_no(item.docker_restarted).into(),
                item.duration_ms.to_string(),
            ]
        })
        .collect();
    render_table(
        &["MONO", "RECOVERED", "FAILED", "DOCKER", "DURATION_MS"],
        &rows,
    )
}

pub fn render_recovery_history_quiet(items: &[RecoveryRecordDto]) -> String {
    let lines = items
        .iter()
        .map(|item| {
            format!(
                "{}\t{}\t{}",
                item.mono,
                item.recovered.join(","),
                item.failed.join(",")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render_empty_or_lines(&lines)
}

pub fn render_backup_history(items: &[BackupRecordDto]) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                if item.success { "success" } else { "failed" }.into(),
                yes_no(item.partial).into(),
                item.duration_secs.to_string(),
                item.error.clone().unwrap_or_else(|| "-".into()),
            ]
        })
        .collect();
    render_table(&["MONO", "RESULT", "PARTIAL", "DURATION_S", "ERROR"], &rows)
}

pub fn render_backup_history_quiet(items: &[BackupRecordDto]) -> String {
    let lines = items
        .iter()
        .map(|item| {
            format!(
                "{}\t{}\t{}",
                item.mono,
                if item.success { "success" } else { "failed" },
                item.error.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render_empty_or_lines(&lines)
}

pub fn render_remediation_history(items: &[RemediationRecordDto]) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                item.action.clone(),
                item.target.clone().unwrap_or_else(|| "-".into()),
                item.source.clone(),
                item.result.clone(),
            ]
        })
        .collect();
    render_table(&["MONO", "ACTION", "TARGET", "SOURCE", "RESULT"], &rows)
}

pub fn render_remediation_history_quiet(items: &[RemediationRecordDto]) -> String {
    let lines = items
        .iter()
        .map(|item| {
            format!(
                "{}\t{}\t{}",
                item.mono,
                item.action,
                item.target.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render_empty_or_lines(&lines)
}

pub fn render_diagnose(diagnose: &DiagnoseResponse) -> String {
    [
        format!("service: {}", diagnose.service),
        format!("unit: {}", diagnose.unit),
        format!("active: {}", yes_no(diagnose.active)),
        String::new(),
        "systemctl status:".into(),
        non_empty_block(&diagnose.systemctl_status),
        String::new(),
        "journal last 50:".into(),
        non_empty_block(&diagnose.journal_last_50),
    ]
    .join("\n")
}

pub fn render_diagnose_quiet(diagnose: &DiagnoseResponse) -> String {
    format!(
        "{}\t{}\t{}",
        diagnose.service,
        diagnose.unit,
        yes_no(diagnose.active)
    )
}

pub fn render_command_result(result: &CommandResult) -> String {
    match (&result.target, &result.detail) {
        (Some(target), Some(detail)) => {
            format!("accepted: {} {} ({detail})", result.action, target)
        }
        (Some(target), None) => format!("accepted: {} {}", result.action, target),
        (None, Some(detail)) => format!("accepted: {} ({detail})", result.action),
        (None, None) => format!("accepted: {}", result.action),
    }
}

pub fn render_command_result_quiet(result: &CommandResult) -> String {
    result.status.clone()
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|value| value.len()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.len());
        }
    }

    let header_line = join_row(headers.iter().map(std::string::ToString::to_string).collect(), &widths);
    let divider = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>()
        .join("-+-");

    let body = rows
        .iter()
        .map(|row| join_row(row.clone(), &widths))
        .collect::<Vec<_>>();

    if body.is_empty() {
        format!("{header_line}\n{divider}\n(empty)")
    } else {
        format!("{header_line}\n{divider}\n{}", body.join("\n"))
    }
}

fn join_row(values: Vec<String>, widths: &[usize]) -> String {
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join(" | ")
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn csv_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".into()
    } else {
        values.join(",")
    }
}

fn non_empty_block(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        "(empty)".into()
    } else {
        trimmed.into()
    }
}

fn render_empty_or_lines(lines: &str) -> String {
    if lines.is_empty() {
        "(empty)".into()
    } else {
        lines.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_renderer_is_compact() {
        let health = HealthResponse {
            status: "ok".into(),
            reason: "ok".into(),
        };
        assert_eq!(render_health(&health), "ok");
    }

    #[test]
    fn services_renderer_contains_header() {
        let rendered = render_services(&[ServiceSummary {
            id: "ntfy".into(),
            status: "healthy".into(),
            summary: "healthy".into(),
        }]);
        assert!(rendered.contains("SERVICE"));
        assert!(rendered.contains("ntfy"));
    }
}
