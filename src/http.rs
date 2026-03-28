//! HTTP API server using `tiny_http`.
//!
//! Runs in a dedicated thread. Read-only endpoints access
//! a shared snapshot. Mutating endpoints send events to the
//! control loop via the event channel.
//!
//! # Auth
//! `POST /api/v1/remediate` requires `Authorization: Bearer <token>`.
//! The token is loaded/generated at startup and written to `ai.token_path`.
//! The token is NEVER written to the audit trail — `source` is always `"http:api"`.

use crate::model::{CommandRequest, Event, RemediationAction, ServiceId, TaskKind};
use std::io::Read;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

/// Shared snapshot that the control loop publishes periodically.
/// HTTP handlers read this without blocking the control loop.
pub type SharedSnapshot = Arc<Mutex<String>>;

/// Creates a new empty snapshot.
#[must_use]
pub fn empty_snapshot() -> SharedSnapshot {
    Arc::new(Mutex::new("{}".to_string()))
}

/// Starts the HTTP server in a background thread.
///
/// `token` is the bearer token required for mutating endpoints.
///
/// # Errors
/// Returns an error if the server cannot bind to the address.
pub fn spawn_http_thread(
    listen_addr: &str,
    snapshot: SharedSnapshot,
    event_tx: mpsc::Sender<Event>,
    token: String,
) -> Result<(), String> {
    let server = tiny_http::Server::http(listen_addr)
        .map_err(|e| format!("HTTP bind {listen_addr}: {e}"))?;

    eprintln!("[cratond] HTTP API listening on {listen_addr}");

    std::thread::Builder::new()
        .name("http-api".into())
        .spawn(move || {
            serve(server, snapshot, event_tx, token);
        })
        .map_err(|e| format!("spawning HTTP thread: {e}"))?;

    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn serve(
    server: tiny_http::Server,
    snapshot: SharedSnapshot,
    event_tx: mpsc::Sender<Event>,
    token: String,
) {
    for mut request in server.incoming_requests() {
        let method = request.method().to_string();
        let url = request.url().to_string();

        let response = match (method.as_str(), url.as_str()) {
            ("GET", "/health") => handle_health(&snapshot),
            ("GET", "/api/v1/state") => handle_state(&snapshot),
            ("GET", path) if path.starts_with("/api/v1/history/") => {
                handle_history(&snapshot, path)
            }
            ("GET", path) if path.starts_with("/api/v1/diagnose/") => handle_diagnose(path),
            ("POST", path) if path.starts_with("/trigger/") => handle_trigger(path, &event_tx),
            ("POST", "/api/v1/remediate") => handle_remediate(&mut request, &event_tx, &token),
            _ => json_response(404, r#"{"error":"not found"}"#),
        };

        if let Err(e) = request.respond(response) {
            eprintln!("[cratond] HTTP response error: {e}");
        }
    }
}

fn handle_health(snapshot: &SharedSnapshot) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let snap = match snapshot.lock() {
        Ok(s) => s.clone(),
        Err(_) => "{}".to_string(),
    };

    // Check heartbeat freshness: snapshot must have been published within 30s.
    let now_epoch = epoch_secs_now();
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&snap) {
        if let Some(published) = parsed
            .get("snapshot_epoch_secs")
            .and_then(serde_json::Value::as_u64)
        {
            if now_epoch.saturating_sub(published) > 30 {
                return json_response(
                    503,
                    r#"{"status":"unavailable","reason":"control loop heartbeat stale"}"#,
                );
            }
        }
    }

    // Check outbox overflow.
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&snap) {
        if parsed
            .get("outbox_overflow")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return json_response(
                503,
                r#"{"status":"unavailable","reason":"alert outbox overflow — undelivered alerts lost"}"#,
            );
        }
    }

    let status = if snap.contains("\"unhealthy\"") || snap.contains("\"failed\"") {
        "degraded"
    } else {
        "ok"
    };

    let body = format!(r#"{{"status":"{status}"}}"#);
    json_response(200, &body)
}

fn epoch_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn handle_state(snapshot: &SharedSnapshot) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let snap = match snapshot.lock() {
        Ok(s) => s.clone(),
        Err(_) => "{}".to_string(),
    };
    json_response(200, &snap)
}

fn handle_history(
    snapshot: &SharedSnapshot,
    _path: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let snap = match snapshot.lock() {
        Ok(s) => s.clone(),
        Err(_) => "{}".to_string(),
    };
    json_response(200, &snap)
}

fn handle_diagnose(path: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let service_name = path.strip_prefix("/api/v1/diagnose/").unwrap_or("");
    if service_name.is_empty() {
        return json_response(400, r#"{"error":"service name required"}"#);
    }

    // Run diagnostic commands.
    let unit = format!("{service_name}.service");

    let active = crate::effect::exec::run(
        &["systemctl", "is-active", "--quiet", &unit],
        std::time::Duration::from_secs(5),
    )
    .is_ok_and(|r| r.exit_code == 0);

    let journal = crate::effect::exec::run(
        &["journalctl", "-u", &unit, "--no-pager", "-n", "50"],
        std::time::Duration::from_secs(10),
    )
    .map_or_else(|e| format!("(failed: {e})"), |r| r.stdout_text());

    let status_output = crate::effect::exec::run(
        &["systemctl", "status", &unit, "--no-pager"],
        std::time::Duration::from_secs(5),
    )
    .map_or_else(|e| format!("(failed: {e})"), |r| r.stdout_text());

    let body = serde_json::json!({
        "service": service_name,
        "unit": unit,
        "active": active,
        "journal_last_50": journal,
        "systemctl_status": status_output,
    });

    json_response(200, &body.to_string())
}

fn handle_trigger(
    path: &str,
    event_tx: &mpsc::Sender<Event>,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let task_name = path.strip_prefix("/trigger/").unwrap_or("");

    let task_kind = match task_name {
        "recovery" => TaskKind::Recovery,
        "backup" => TaskKind::Backup,
        "disk-monitor" | "disk_monitor" => TaskKind::DiskMonitor,
        "apt-updates" | "apt_updates" => TaskKind::AptUpdates,
        "docker-updates" | "docker_updates" => TaskKind::DockerUpdates,
        "daily-summary" | "daily_summary" => TaskKind::DailySummary,
        _ => {
            let body = format!(r#"{{"error":"unknown task: {task_name}"}}"#);
            return json_response(404, &body);
        }
    };

    let event = Event::HttpCommand(CommandRequest::Trigger(task_kind));

    match event_tx.send(event) {
        Ok(()) => json_response(
            202,
            &format!(r#"{{"status":"accepted","task":"{task_name}"}}"#),
        ),
        Err(_) => json_response(503, r#"{"error":"control loop unavailable"}"#),
    }
}

/// Handles `POST /api/v1/remediate`.
///
/// Expects `Authorization: Bearer <token>` and a JSON body:
/// `{"action": "RestartService", "target": "ntfy", "reason": "manual"}`
///
/// Source written to the audit trail is always `"http:api"` — the token
/// is never stored in the remediation log.
fn handle_remediate(
    request: &mut tiny_http::Request,
    event_tx: &mpsc::Sender<Event>,
    expected_token: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    // --- Bearer token check ---
    let auth = request
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    let provided = auth
        .strip_prefix("Bearer ")
        .unwrap_or("")
        .trim()
        .to_string();

    if provided != expected_token || expected_token.is_empty() {
        return json_response(401, r#"{"error":"unauthorized"}"#);
    }

    // --- Read body (max 4 KB) ---
    let mut body = Vec::with_capacity(512);
    if request
        .as_reader()
        .take(4096)
        .read_to_end(&mut body)
        .is_err()
    {
        return json_response(400, r#"{"error":"failed to read body"}"#);
    }

    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_response(400, r#"{"error":"invalid JSON"}"#),
    };

    // --- Parse action ---
    let action_str = parsed.get("action").and_then(|v| v.as_str()).unwrap_or("");

    let action = match action_str {
        "RestartService" => RemediationAction::RestartService,
        "DockerRestart" => RemediationAction::DockerRestart,
        "TriggerBackup" => RemediationAction::TriggerBackup,
        "MarkMaintenance" => RemediationAction::MarkMaintenance,
        "ClearMaintenance" => RemediationAction::ClearMaintenance,
        "ClearBreaker" => RemediationAction::ClearBreaker,
        "ClearFlapping" => RemediationAction::ClearFlapping,
        "RunDiskCleanup" => RemediationAction::RunDiskCleanup,
        "ResticUnlock" => RemediationAction::ResticUnlock,
        _ => {
            let msg = format!(r#"{{"error":"unknown action: {action_str}"}}"#);
            return json_response(400, &msg);
        }
    };

    // --- Parse optional target ---
    let target = parsed
        .get("target")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| ServiceId(s.to_string()));

    let reason = parsed
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("http api")
        .to_string();

    // Source is always "http:api" — the token is never written to the audit trail.
    let event = Event::HttpCommand(CommandRequest::Remediate {
        action,
        target,
        source: "http:api".to_string(),
        reason,
    });

    match event_tx.send(event) {
        Ok(()) => json_response(202, r#"{"status":"accepted"}"#),
        Err(_) => json_response(503, r#"{"error":"control loop unavailable"}"#),
    }
}

fn json_response(status: u16, body: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let data = body.as_bytes().to_vec();
    let len = data.len();

    #[allow(clippy::expect_used)]
    let content_type =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("static header is always valid");

    tiny_http::Response::new(
        tiny_http::StatusCode(status),
        vec![content_type],
        std::io::Cursor::new(data),
        Some(len),
        None,
    )
}
