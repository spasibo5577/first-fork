//! Fire-and-forget HTTP POST to `PicoClaw` AI bridge.
//!
//! Timeout 2 seconds. Errors logged to stderr, never affect control flow.
//! AI bridge NEVER influences recovery decisions.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

/// Sends an event to `PicoClaw`. Fire-and-forget: spawns a thread,
/// never blocks the caller, errors are logged only.
#[allow(dead_code)] // Phase 4: wired via TriggerPicoClaw command
pub fn trigger(picoclaw_url: &str, event_type: &str, details: &BTreeMap<String, String>) {
    let url = picoclaw_url.to_string();
    let body = serde_json::json!({
        "event_type": event_type,
        "details": details,
        "source": "cratond",
    })
    .to_string();

    std::thread::Builder::new()
        .name("picoclaw-trigger".into())
        .spawn(move || {
            if let Err(e) = send_post(&url, &body) {
                crate::log::raw(&format!("[cratond] PicoClaw trigger failed (non-fatal): {e}"));
            }
        })
        .ok();
}

fn send_post(url: &str, body: &str) -> Result<(), String> {
    let without_scheme = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("unsupported URL scheme: {url}"))?;

    let (host_port, path) = match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None => (without_scheme, "/trigger"),
    };

    let addr: std::net::SocketAddr = host_port
        .parse()
        .map_err(|e| format!("invalid address {host_port}: {e}"))?;

    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| format!("connect: {e}"))?;

    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("set timeout: {e}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;

    Ok(())
}
