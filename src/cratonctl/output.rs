use crate::cratonctl::dto::{
    BackupRecordDto, CommandResult, DiagnoseResponse, DoctorReport, HealthResponse,
    RecoveryRecordDto, RemediationRecordDto, ServiceDetail, ServiceSummary, StatusSummary,
};

const BANNER: &str = " ██████╗██████╗  █████╗ ████████╗ ██████╗ ███╗   ██╗\n██╔════╝██╔══██╗██╔══██╗╚══██╔══╝██╔═══██╗████╗  ██║\n██║     ██████╔╝███████║   ██║   ██║   ██║██╔██╗ ██║\n██║     ██╔══██╗██╔══██║   ██║   ██║   ██║██║╚██╗██║\n╚██████╗██║  ██║██║  ██║   ██║   ╚██████╔╝██║ ╚████║\n ╚═════╝╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝    ╚═════╝ ╚═╝  ╚═══╝";

#[derive(Debug, Clone, Copy)]
pub struct Presentation {
    pub use_color: bool,
    pub show_banner: bool,
}

pub fn render_health(health: &HealthResponse, presentation: Presentation) -> String {
    if health.is_ok() {
        paint_ok("ok", presentation)
    } else {
        format!("unavailable: {}", paint_warn(&health.reason, presentation))
    }
}

pub fn render_health_quiet(health: &HealthResponse) -> String {
    if health.is_ok() {
        "ok".into()
    } else {
        format!("unavailable: {}", health.reason)
    }
}

pub fn render_status(status: &StatusSummary, presentation: Presentation) -> String {
    let disk = status
        .disk_usage_percent
        .map_or_else(|| "unknown".into(), |value| format!("{value}%"));
    let state_counts = status
        .state_counts
        .iter()
        .map(|(state, count)| format!("{state}={count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut lines = vec![
        format!(
            "health: {} ({})",
            paint_status(&status.health.status, presentation),
            status.health.reason
        ),
        format!(
            "services: {} total, {} degraded",
            status.service_count, status.degraded_count
        ),
        format!("states: {}", paint_meta(&state_counts, presentation)),
        format!("backup: {}", paint_meta(&status.backup_phase, presentation)),
        format!("disk: {}", paint_meta(&disk, presentation)),
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
        status.health.status,
        status.service_count,
        status.degraded_count,
        status.backup_phase
    )
}

pub fn render_services(services: &[ServiceSummary], presentation: Presentation) -> String {
    let rows: Vec<Vec<String>> = services
        .iter()
        .map(|service| {
            vec![
                service.id.clone(),
                paint_status(&service.status, presentation),
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

pub fn render_service(service: &ServiceDetail, presentation: Presentation) -> String {
    let mut lines = vec![
        format!("service: {}", service.id),
        format!("status: {}", paint_status(&service.status, presentation)),
        format!("summary: {}", service.summary),
    ];

    if service.since_mono.is_some() {
        lines.push("age: unavailable over current API".into());
    }

    match &service.raw_status {
        crate::cratonctl::dto::ServiceStatusDto::Suppressed { .. } => {
            lines.push("breaker: open".into());
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

pub fn render_recovery_history(items: &[RecoveryRecordDto], presentation: Presentation) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                csv_or_dash(&item.recovered),
                csv_or_dash(&item.failed),
                if item.docker_restarted {
                    paint_warn("yes", presentation)
                } else {
                    "no".into()
                },
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

pub fn render_backup_history(items: &[BackupRecordDto], presentation: Presentation) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                if item.success {
                    paint_ok("success", presentation)
                } else {
                    paint_fail("failed", presentation)
                },
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

pub fn render_remediation_history(
    items: &[RemediationRecordDto],
    presentation: Presentation,
) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            vec![
                item.mono.to_string(),
                item.action.clone(),
                item.target.clone().unwrap_or_else(|| "-".into()),
                item.source.clone(),
                paint_meta(&item.result, presentation),
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

pub fn render_diagnose(diagnose: &DiagnoseResponse, presentation: Presentation) -> String {
    [
        format!("service: {}", diagnose.service),
        format!("unit: {}", diagnose.unit),
        format!(
            "active: {}",
            if diagnose.active {
                paint_ok("yes", presentation)
            } else {
                paint_fail("no", presentation)
            }
        ),
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

pub fn render_command_result(result: &CommandResult, presentation: Presentation) -> String {
    let accepted = paint_ok("accepted", presentation);
    match (&result.target, &result.detail) {
        (Some(target), Some(detail)) => {
            format!("{accepted}: {} {} ({detail})", result.action, target)
        }
        (Some(target), None) => format!("{accepted}: {} {}", result.action, target),
        (None, Some(detail)) => format!("{accepted}: {} ({detail})", result.action),
        (None, None) => format!("{accepted}: {}", result.action),
    }
}

pub fn render_command_result_quiet(result: &CommandResult) -> String {
    result.status.clone()
}

pub fn render_doctor(report: &DoctorReport, presentation: Presentation) -> String {
    let rows = report
        .checks
        .iter()
        .map(|check| {
            vec![
                paint_check_status(&check.status, presentation),
                check.name.clone(),
                check.detail.clone(),
            ]
        })
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    if presentation.show_banner {
        out.push(BANNER.into());
    }
    out.push(format!("url: {}", paint_meta(&report.url, presentation)));
    out.push(render_table(&["STATE", "CHECK", "DETAIL"], &rows));
    out.push(format!(
        "summary: read_only={} mutating={}",
        if report.read_only_ready {
            paint_ok("yes", presentation)
        } else {
            paint_fail("no", presentation)
        },
        if report.mutating_ready {
            paint_ok("yes", presentation)
        } else {
            paint_warn("no", presentation)
        }
    ));
    out.join("\n\n")
}

pub fn render_doctor_quiet(report: &DoctorReport) -> String {
    report
        .checks
        .iter()
        .map(|check| format!("{}\t{}\t{}", check.status, check.name, check.code))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn render_help(usage: &str, presentation: Presentation) -> String {
    let mut out = Vec::new();
    if presentation.show_banner {
        out.push(BANNER.into());
        out.push(String::new());
    }

    for line in usage.lines() {
        if line.ends_with(':') {
            out.push(paint_section(line, presentation));
        } else if line.starts_with("  ") {
            out.push(format!("  {}", line.trim_start()));
        } else {
            out.push(line.into());
        }
    }

    out.join("\n")
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|value| value.len()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(display_width(cell));
        }
    }

    let header_line = join_row(
        headers.iter().map(std::string::ToString::to_string).collect(),
        &widths,
    );
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
        .map(|(index, value)| pad_display(&value, widths[index]))
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

fn paint_status(status: &str, presentation: Presentation) -> String {
    match status {
        "healthy" => paint_ok(status, presentation),
        "recovering" | "unknown" | "blocked" => paint_warn(status, presentation),
        "failed" | "unhealthy" | "breaker_open" => paint_fail(status, presentation),
        _ => status.into(),
    }
}

fn paint_check_status(status: &str, presentation: Presentation) -> String {
    match status {
        "ok" => paint_ok("OK", presentation),
        "warn" => paint_warn("WARN", presentation),
        "fail" => paint_fail("FAIL", presentation),
        _ => status.to_uppercase(),
    }
}

fn paint_section(text: &str, presentation: Presentation) -> String {
    paint(text, "36;1", presentation)
}

fn paint_ok(text: &str, presentation: Presentation) -> String {
    paint(text, "32", presentation)
}

fn paint_warn(text: &str, presentation: Presentation) -> String {
    paint(text, "33", presentation)
}

fn paint_fail(text: &str, presentation: Presentation) -> String {
    paint(text, "31", presentation)
}

fn paint_meta(text: &str, presentation: Presentation) -> String {
    paint(text, "2", presentation)
}

fn paint(text: &str, code: &str, presentation: Presentation) -> String {
    if presentation.use_color {
        format!("\u{1b}[{code}m{text}\u{1b}[0m")
    } else {
        text.into()
    }
}

fn pad_display(value: &str, width: usize) -> String {
    let visible = display_width(value);
    if visible >= width {
        value.into()
    } else {
        format!("{value}{}", " ".repeat(width - visible))
    }
}

fn display_width(value: &str) -> usize {
    let mut width = 0usize;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code_ch in chars.by_ref() {
                if code_ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        width += 1;
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Presentation = Presentation {
        use_color: false,
        show_banner: false,
    };

    #[test]
    fn health_renderer_is_compact() {
        let health = HealthResponse {
            status: "ok".into(),
            reason: "ok".into(),
        };
        assert_eq!(render_health(&health, PLAIN), "ok");
    }

    #[test]
    fn services_renderer_contains_header() {
        let rendered = render_services(
            &[ServiceSummary {
                id: "ntfy".into(),
                status: "healthy".into(),
                summary: "healthy".into(),
            }],
            PLAIN,
        );
        assert!(rendered.contains("SERVICE"));
        assert!(rendered.contains("ntfy"));
    }

    #[test]
    fn service_renderer_hides_raw_monotonic_fields_in_human_output() {
        let detail = ServiceDetail {
            id: "ntfy".into(),
            status: "healthy".into(),
            summary: "healthy".into(),
            since_mono: Some(123),
            raw_status: crate::cratonctl::dto::ServiceStatusDto::Healthy { since_mono: 123 },
        };

        let rendered = render_service(&detail, PLAIN);
        assert!(rendered.contains("age: unavailable over current API"));
        assert!(!rendered.contains("since_mono: 123"));
    }

    #[test]
    fn colored_tables_keep_columns_aligned() {
        let rendered = render_services(
            &[
                ServiceSummary {
                    id: "alpha".into(),
                    status: "healthy".into(),
                    summary: "healthy".into(),
                },
                ServiceSummary {
                    id: "beta".into(),
                    status: "failed".into(),
                    summary: "failed".into(),
                },
            ],
            Presentation {
                use_color: true,
                show_banner: false,
            },
        );
        let lines = rendered.lines().collect::<Vec<_>>();
        assert_eq!(lines[2].find('|'), lines[3].find('|'));
    }
}
