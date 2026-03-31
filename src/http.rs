//! HTTP API server using `tiny_http`.
//!
//! Runs in a dedicated thread. Read-only endpoints access
//! a shared snapshot. Mutating endpoints send events to the
//! control loop via the event channel.
//!
//! # Auth
//! `POST /api/v1/remediate` and `POST /trigger/{task}` require
//! `Authorization: Bearer <token>`.
//! The token is loaded/generated at startup and written to `ai.token_path`.
//! The token is NEVER written to the audit trail — `source` is always `"http:api"`.

use crate::model::{
    CommandRequest, Event, HttpCommandRequest, HttpCommandResponse, RemediationAction, ServiceId,
    TaskKind,
};
use std::collections::BTreeSet;
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
    known_services: Arc<BTreeSet<String>>,
) -> Result<(), String> {
    let server = tiny_http::Server::http(listen_addr)
        .map_err(|e| format!("HTTP bind {listen_addr}: {e}"))?;

    crate::log::raw(&format!("[cratond] HTTP API listening on {listen_addr}"));

    std::thread::Builder::new()
        .name("http-api".into())
        .spawn(move || {
            serve(server, snapshot, event_tx, token, known_services);
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
    known_services: Arc<BTreeSet<String>>,
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
            ("GET", path) if path.starts_with("/api/v1/diagnose/") => {
                handle_diagnose(path, &known_services)
            }
            ("POST", path) if path.starts_with("/trigger/") => {
                let auth = authorization_header(&request);
                handle_trigger(path, &auth, &event_tx, &token)
            }
            ("POST", "/api/v1/remediate") => handle_remediate(&mut request, &event_tx, &token),
            _ => json_response(404, r#"{"error":"not found"}"#),
        };

        if let Err(e) = request.respond(response) {
            crate::log::raw(&format!("[cratond] HTTP response error: {e}"));
        }
    }
}

fn handle_health(snapshot: &SharedSnapshot) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let snap = match snapshot.lock() {
        Ok(s) => s.clone(),
        Err(_) => "{}".to_string(),
    };

    let parsed = serde_json::from_str::<serde_json::Value>(&snap).unwrap_or_default();
    health_response(&parsed, epoch_secs_now(), monotonic_secs_now())
}

fn health_response(
    parsed: &serde_json::Value,
    now_epoch: u64,
    now_mono: u64,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let stale_snapshot = parsed
        .get("snapshot_epoch_secs")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|published| now_epoch.saturating_sub(published) > 30);
    if stale_snapshot {
        return json_response(503, r#"{"status":"unavailable","reason":"stale_snapshot"}"#);
    }

    let outbox_overflow = parsed
        .get("outbox_overflow")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if outbox_overflow {
        return json_response(503, r#"{"status":"unavailable","reason":"outbox_overflow"}"#);
    }

    let stale_probe_cycle = parsed
        .get("last_probe_cycle_mono")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|last_probe| now_mono.saturating_sub(last_probe) > 600);
    if stale_probe_cycle {
        return json_response(503, r#"{"status":"unavailable","reason":"stale_health_cycle"}"#);
    }

    let shutting_down = parsed
        .get("shutting_down")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if shutting_down {
        return json_response(503, r#"{"status":"unavailable","reason":"shutting_down"}"#);
    }

    let degraded_reasons = parsed
        .get("degraded_reasons")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !degraded_reasons.is_empty() {
        return json_response(
            503,
            &serde_json::json!({
                "status": "unavailable",
                "reason": "degraded",
                "degraded_reasons": degraded_reasons,
            })
            .to_string(),
        );
    }

    json_response(200, r#"{"status":"ok","reason":"ok"}"#)
}

fn epoch_secs_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn monotonic_secs_now() -> u64 {
    #[cfg(unix)]
    {
        let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        ts.tv_sec as u64
    }

    #[cfg(not(unix))]
    {
        use std::sync::OnceLock;
        use std::time::Instant;

        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_secs()
    }
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
    path: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let snap = match snapshot.lock() {
        Ok(s) => s.clone(),
        Err(_) => "{}".to_string(),
    };

    let target = path.strip_prefix("/api/v1/history/").unwrap_or("");
    let parsed = serde_json::from_str::<serde_json::Value>(&snap).unwrap_or_else(|_| {
        serde_json::json!({
            "backup_history": [],
            "recovery_history": [],
            "remediation_history": []
        })
    });

    let response_value = match target {
        "recovery" => parsed.get("recovery_history").cloned().unwrap_or_else(|| {
            serde_json::json!([])
        }),
        "backup" => parsed.get("backup_history").cloned().unwrap_or_else(|| {
            serde_json::json!([])
        }),
        "remediation" => parsed.get("remediation_history").cloned().unwrap_or_else(|| {
            serde_json::json!([])
        }),
        _ => return json_response(404, r#"{"error":"not found"}"#),
    };

    json_response(200, &response_value.to_string())
}

fn handle_diagnose(
    path: &str,
    known_services: &BTreeSet<String>,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let service_name = path.strip_prefix("/api/v1/diagnose/").unwrap_or("");
    if service_name.is_empty() {
        return json_response(400, r#"{"error":"service name required"}"#);
    }
    if !known_services.contains(service_name) {
        return json_response(404, &format!(r#"{{"error":"unknown service: {service_name}"}}"#));
    }

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

fn authorization_header(request: &tiny_http::Request) -> String {
    request
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default()
}

fn is_authorized_bearer(auth_header: &str, expected_token: &str) -> bool {
    let provided = auth_header
        .strip_prefix("Bearer ")
        .unwrap_or("")
        .trim();

    !expected_token.is_empty() && constant_time_eq(provided.as_bytes(), expected_token.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn handle_trigger(
    path: &str,
    auth_header: &str,
    event_tx: &mpsc::Sender<Event>,
    expected_token: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    handle_trigger_with_timeout(
        path,
        auth_header,
        event_tx,
        expected_token,
        std::time::Duration::from_secs(10),
    )
}

fn handle_trigger_with_timeout(
    path: &str,
    auth_header: &str,
    event_tx: &mpsc::Sender<Event>,
    expected_token: &str,
    ack_timeout: std::time::Duration,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    if !is_authorized_bearer(auth_header, expected_token) {
        return json_response(401, r#"{"error":"unauthorized"}"#);
    }

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

    let (response_tx, response_rx) = mpsc::channel();
    let event = Event::HttpCommand(HttpCommandRequest::with_response(
        CommandRequest::Trigger(task_kind),
        response_tx,
    ));

    match event_tx.send(event) {
        Ok(()) => match response_rx.recv_timeout(ack_timeout) {
            Ok(HttpCommandResponse::Accepted { detail }) => json_response(
                200,
                &serde_json::json!({
                    "status": "completed",
                    "task": task_name,
                    "detail": detail,
                })
                .to_string(),
            ),
            Ok(HttpCommandResponse::Rejected { reason }) => {
                json_response(409, &serde_json::json!({ "error": reason }).to_string())
            }
            Ok(HttpCommandResponse::Error { error }) => {
                json_response(500, &serde_json::json!({ "error": error }).to_string())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                json_response(504, r#"{"error":"control loop did not respond in time"}"#)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                json_response(503, r#"{"error":"control loop terminated"}"#)
            }
        },
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
    handle_remediate_with_timeout(
        request,
        event_tx,
        expected_token,
        std::time::Duration::from_secs(10),
    )
}

fn handle_remediate_with_timeout(
    request: &mut tiny_http::Request,
    event_tx: &mpsc::Sender<Event>,
    expected_token: &str,
    ack_timeout: std::time::Duration,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let auth = authorization_header(request);
    if !is_authorized_bearer(&auth, expected_token) {
        return json_response(401, r#"{"error":"unauthorized"}"#);
    }

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

    let (response_tx, response_rx) = mpsc::channel();
    let event = Event::HttpCommand(HttpCommandRequest::with_response(
        CommandRequest::Remediate {
            action,
            target,
            source: "http:api".to_string(),
            reason,
        },
        response_tx,
    ));

    match event_tx.send(event) {
        Ok(()) => match response_rx.recv_timeout(ack_timeout) {
            Ok(HttpCommandResponse::Accepted { detail }) => json_response(
                200,
                &serde_json::json!({
                    "status": "completed",
                    "detail": detail,
                })
                .to_string(),
            ),
            Ok(HttpCommandResponse::Rejected { reason }) => {
                json_response(409, &serde_json::json!({ "error": reason }).to_string())
            }
            Ok(HttpCommandResponse::Error { error }) => {
                json_response(500, &serde_json::json!({ "error": error }).to_string())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                json_response(504, r#"{"error":"control loop did not respond in time"}"#)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                json_response(503, r#"{"error":"control loop terminated"}"#)
            }
        },
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_json(
        resp: tiny_http::Response<std::io::Cursor<Vec<u8>>>,
    ) -> (tiny_http::StatusCode, serde_json::Value) {
        let status = resp.status_code();
        let mut body = String::new();
        resp.into_reader().read_to_string(&mut body).unwrap();
        let data: serde_json::Value = serde_json::from_str(&body).unwrap();
        (status, data)
    }

    fn snapshot_with(value: &serde_json::Value) -> SharedSnapshot {
        Arc::new(Mutex::new(value.to_string()))
    }

    fn known_services() -> BTreeSet<String> {
        BTreeSet::from(["ntfy".to_string(), "unbound".to_string()])
    }

    #[test]
    fn health_returns_200_for_fresh_snapshot() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(200));
        assert_eq!(data["status"], "ok");
        assert_eq!(data["reason"], "ok");
    }

    #[test]
    fn health_returns_503_for_stale_snapshot() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now().saturating_sub(31),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(503));
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason"], "stale_snapshot");
    }

    #[test]
    fn health_returns_503_for_outbox_overflow() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": true,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(503));
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason"], "outbox_overflow");
    }

    #[test]
    fn health_returns_503_when_shutting_down() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": true,
            "degraded_reasons": [],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(503));
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason"], "shutting_down");
    }

    #[test]
    fn health_ignores_service_failure_details_when_typed_health_fields_are_ok() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {
                "ntfy": { "status": "failed", "error": "boom", "since_mono": 1 }
            }
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(200));
        assert_eq!(data["status"], "ok");
        assert_eq!(data["reason"], "ok");
    }

    #[test]
    fn health_returns_503_for_stale_probe_cycle() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": 1_700_000_000u64,
            "last_probe_cycle_mono": 100u64,
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {}
        });

        let (status, data) = read_json(health_response(&snapshot_value, 1_700_000_000, 701));

        assert_eq!(status, tiny_http::StatusCode(503));
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason"], "stale_health_cycle");
    }

    #[test]
    fn health_returns_503_when_degraded_reasons_present() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": ["probe_spawn_failed"],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_health(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(503));
        assert_eq!(data["status"], "unavailable");
        assert_eq!(data["reason"], "degraded");
        assert_eq!(data["degraded_reasons"], serde_json::json!(["probe_spawn_failed"]));
    }

    #[test]
    fn trigger_without_bearer_token_is_unauthorized() {
        let (tx, _rx) = mpsc::channel();

        let (status, data) = read_json(handle_trigger("/trigger/recovery", "", &tx, "secret-token"));

        assert_eq!(status, tiny_http::StatusCode(401));
        assert_eq!(data, serde_json::json!({"error":"unauthorized"}));
    }

    #[test]
    fn trigger_with_invalid_bearer_token_is_unauthorized() {
        let (tx, _rx) = mpsc::channel();

        let (status, data) = read_json(handle_trigger(
            "/trigger/recovery",
            "Bearer wrong-token",
            &tx,
            "secret-token",
        ));

        assert_eq!(status, tiny_http::StatusCode(401));
        assert_eq!(data, serde_json::json!({"error":"unauthorized"}));
    }

    #[test]
    fn trigger_with_valid_bearer_token_accepts_known_task() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let event = rx.recv().unwrap();
            let Event::HttpCommand(req) = event else {
                panic!("expected HttpCommand");
            };
            assert_eq!(req.command, CommandRequest::Trigger(TaskKind::Recovery));
            req.response_tx
                .unwrap_or_else(|| panic!("response channel missing"))
                .send(HttpCommandResponse::Accepted {
                    detail: "task scheduled: recovery".to_string(),
                })
                .unwrap();
        });

        let (status, data) = read_json(handle_trigger_with_timeout(
            "/trigger/recovery",
            "Bearer secret-token",
            &tx,
            "secret-token",
            std::time::Duration::from_millis(50),
        ));

        assert_eq!(status, tiny_http::StatusCode(200));
        assert_eq!(
            data,
            serde_json::json!({
                "status":"completed",
                "task":"recovery",
                "detail":"task scheduled: recovery"
            })
        );
    }

    #[test]
    fn trigger_can_return_rejected_response() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let event = rx.recv().unwrap();
            let Event::HttpCommand(req) = event else {
                panic!("expected HttpCommand");
            };
            req.response_tx
                .unwrap_or_else(|| panic!("response channel missing"))
                .send(HttpCommandResponse::Rejected {
                    reason: "rate limited".to_string(),
                })
                .unwrap();
        });

        let (status, data) = read_json(handle_trigger_with_timeout(
            "/trigger/recovery",
            "Bearer secret-token",
            &tx,
            "secret-token",
            std::time::Duration::from_millis(50),
        ));

        assert_eq!(status, tiny_http::StatusCode(409));
        assert_eq!(data, serde_json::json!({"error":"rate limited"}));
    }

    #[test]
    fn trigger_times_out_when_control_loop_does_not_ack() {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _event = rx.recv().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(100));
        });

        let (status, data) = read_json(handle_trigger_with_timeout(
            "/trigger/recovery",
            "Bearer secret-token",
            &tx,
            "secret-token",
            std::time::Duration::from_millis(10),
        ));

        assert_eq!(status, tiny_http::StatusCode(504));
        assert_eq!(data, serde_json::json!({"error":"control loop did not respond in time"}));
    }

    #[test]
    fn trigger_with_valid_bearer_token_preserves_unknown_task_behavior() {
        let (tx, _rx) = mpsc::channel();

        let (status, data) = read_json(handle_trigger(
            "/trigger/not-a-task",
            "Bearer secret-token",
            &tx,
            "secret-token",
        ));

        assert_eq!(status, tiny_http::StatusCode(404));
        assert_eq!(data, serde_json::json!({"error":"unknown task: not-a-task"}));
    }

    #[test]
    fn read_only_endpoints_remain_accessible_without_token() {
        let snapshot_value = serde_json::json!({
            "snapshot_epoch_secs": epoch_secs_now(),
            "last_probe_cycle_mono": monotonic_secs_now(),
            "outbox_overflow": false,
            "shutting_down": false,
            "degraded_reasons": [],
            "services": {}
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (health_status, _) = read_json(handle_health(&snapshot));
        let (state_status, _) = read_json(handle_state(&snapshot));

        assert_eq!(health_status, tiny_http::StatusCode(200));
        assert_eq!(state_status, tiny_http::StatusCode(200));
    }

    #[test]
    fn constant_time_eq_accepts_equal_inputs() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
    }

    #[test]
    fn constant_time_eq_rejects_different_inputs() {
        assert!(!constant_time_eq(b"secret-token", b"secret-tokfn"));
    }

    #[test]
    fn constant_time_eq_rejects_different_lengths() {
        assert!(!constant_time_eq(b"secret-token", b"secret"));
    }

    #[test]
    fn history_recovery_returns_only_recovery_list() {
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(
            r#"{"recovery_history":[{"mono":1,"recovered":[],"failed":[],"docker_restarted":false,"duration_ms":0}],"backup_history":[{"mono":2,"success":true,"partial":false,"error":null,"duration_secs":1}],"snapshot_epoch_secs":123}"#.to_string(),
        ));

        let (status, data) = read_json(handle_history(&snapshot, "/api/v1/history/recovery"));
        assert_eq!(status, tiny_http::StatusCode(200));
        assert!(data.is_array());
        assert_eq!(data[0]["mono"], 1);
    }

    #[test]
    fn history_backup_returns_only_backup_list() {
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(
            r#"{"recovery_history":[{"mono":1,"recovered":[],"failed":[],"docker_restarted":false,"duration_ms":0}],"backup_history":[{"mono":2,"success":true,"partial":false,"error":null,"duration_secs":1}],"snapshot_epoch_secs":123}"#.to_string(),
        ));

        let (status, data) = read_json(handle_history(&snapshot, "/api/v1/history/backup"));
        assert_eq!(status, tiny_http::StatusCode(200));
        assert!(data.is_array());
        assert_eq!(data[0]["mono"], 2);
    }

    #[test]
    fn history_remediation_returns_empty_array() {
        let snapshot: SharedSnapshot = Arc::new(Mutex::new(
            r#"{"recovery_history":[{"mono":1,"recovered":[],"failed":[],"docker_restarted":false,"duration_ms":0}],"backup_history":[{"mono":2,"success":true,"partial":false,"error":null,"duration_secs":1}],"snapshot_epoch_secs":123}"#.to_string(),
        ));

        let (status, data) = read_json(handle_history(&snapshot, "/api/v1/history/remediation"));
        assert_eq!(status, tiny_http::StatusCode(200));
        assert!(data.is_array(), "remediation endpoint should return array");
        assert!(data.as_array().unwrap().is_empty(), "should be empty when not in snapshot");
    }

    #[test]
    fn state_returns_snapshot_object_not_array() {
        let snapshot_value = serde_json::json!({
            "services": {
                "ntfy": { "status": "healthy", "since_mono": 1 }
            },
            "snapshot_epoch_secs": 123
        });
        let snapshot = snapshot_with(&snapshot_value);

        let (status, data) = read_json(handle_state(&snapshot));

        assert_eq!(status, tiny_http::StatusCode(200));
        assert!(data.is_object());
        assert!(!data.is_array());
        assert_eq!(data["snapshot_epoch_secs"], 123);
    }

    #[test]
    fn diagnose_unknown_service_returns_404() {
        let (status, data) =
            read_json(handle_diagnose("/api/v1/diagnose/not-configured", &known_services()));

        assert_eq!(status, tiny_http::StatusCode(404));
        assert_eq!(data, serde_json::json!({"error":"unknown service: not-configured"}));
    }
}
