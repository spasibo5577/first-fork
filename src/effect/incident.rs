//! Markdown incident report writer.
//!
//! Writes structured incident reports to the `PicoClaw` workspace
//! using atomic file writes for crash safety.

use crate::model::IncidentReport;
use std::fmt::Write;
use std::path::Path;

/// Writes an incident report as a markdown file.
///
/// Directory: `/root/.picoclaw/workspace/incidents/`
/// File name: `{timestamp}_{kind}.md`
#[allow(dead_code)] // Phase 4: wired via WriteIncident command
pub fn write_report(report: &IncidentReport) {
    const INCIDENT_DIR: &str = "/root/.picoclaw/workspace/incidents";

    let mut content = String::with_capacity(512);

    let _ = writeln!(content, "# Incident: {:?}", report.kind);
    let _ = writeln!(content);
    let _ = writeln!(content, "**Timestamp:** {}", report.timestamp_epoch_secs);

    if let Some(ref svc) = report.service {
        let _ = writeln!(content, "**Service:** {svc}");
    }

    let _ = writeln!(content);
    let _ = writeln!(content, "## Details");
    let _ = writeln!(content);

    for (key, value) in &report.details {
        let _ = writeln!(content, "- **{key}:** {value}");
    }

    let filename = format!("{}_{:?}.md", report.timestamp_epoch_secs, report.kind);

    let dir = Path::new(INCIDENT_DIR);
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("[cratond] cannot create incident dir: {e}");
        return;
    }

    let path = dir.join(&filename);
    if let Err(e) = crate::persist::atomic_write(&path, content.as_bytes()) {
        eprintln!("[cratond] failed to write incident {filename}: {e}");
    } else {
        eprintln!("[cratond] incident written: {filename}");
    }
}