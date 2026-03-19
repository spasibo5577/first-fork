//! Health probe implementations: HTTP, DNS, systemd, exec.
//!
//! Each probe returns a `ProbeResult`. No side effects beyond
//! the probe itself (no state mutation, no notifications).

use crate::effect::exec;
use crate::model::{ProbeError, ProbeResult, ProbeSpec, ServiceId, StdoutCheck};
use std::io::{Read, Write};
use std::net::UdpSocket;
use std::time::{Duration, Instant};

/// Runs a single health probe and returns the result.
pub fn run_probe(service_id: &ServiceId, spec: &ProbeSpec, unit: &str) -> ProbeResult {
    let start = Instant::now();

    let result = match spec {
        ProbeSpec::Http {
            url,
            timeout_secs,
            expect_status,
        } => check_http(url, Duration::from_secs(*timeout_secs), *expect_status),
        ProbeSpec::Dns {
            server,
            port,
            query,
            timeout_secs,
        } => check_dns(server, *port, query, Duration::from_secs(*timeout_secs)),
        ProbeSpec::SystemdActive {
            unit: override_unit,
        } => {
            let u = if override_unit.is_empty() {
                unit
            } else {
                override_unit
            };
            check_systemd(u)
        }
        ProbeSpec::Exec {
            argv,
            timeout_secs,
            expect_stdout,
        } => check_exec(argv, Duration::from_secs(*timeout_secs), expect_stdout.as_ref()),
    };

    #[allow(clippy::cast_possible_truncation)]
    let latency_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(()) => ProbeResult::Healthy {
            service: service_id.clone(),
            latency_ms,
        },
        Err(error) => ProbeResult::Unhealthy {
            service: service_id.clone(),
            error,
            latency_ms,
        },
    }
}

/// HTTP probe: GET request, expect specific status code.
fn check_http(url: &str, timeout: Duration, expect_status: u16) -> Result<(), ProbeError> {
    // Minimal HTTP/1.1 GET using raw TCP.
    // We avoid pulling in an HTTP client crate for probes.

    let url_parsed = parse_url(url).ok_or_else(|| ProbeError::DnsFailure {
        message: format!("invalid URL: {url}"),
    })?;

    let addr = format!("{}:{}", url_parsed.host, url_parsed.port);

    let stream = std::net::TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|_| ProbeError::ConnectionRefused)?,
        timeout,
    )
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            ProbeError::Timeout
        } else {
            ProbeError::ConnectionRefused
        }
    })?;

    stream
        .set_read_timeout(Some(timeout))
        .map_err(|_| ProbeError::Timeout)?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|_| ProbeError::Timeout)?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: cratond/1.0\r\n\r\n",
        url_parsed.path, url_parsed.host
    );

    let mut stream = stream;
    stream
        .write_all(request.as_bytes())
        .map_err(|_| ProbeError::ConnectionRefused)?;

    let mut response = Vec::with_capacity(4096);
    let mut buf = [0u8; 4096];
    // Read just enough to get the status line.
    match stream.read(&mut buf) {
        Ok(n) if n > 0 => response.extend_from_slice(&buf[..n]),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => return Err(ProbeError::Timeout),
        _ => return Err(ProbeError::ConnectionRefused),
    }
    // Parse status code from "HTTP/1.1 200 OK\r\n..."
    let response_str = String::from_utf8_lossy(&response);
    let status = parse_http_status(&response_str).ok_or(ProbeError::ConnectionRefused)?;

    if status == expect_status {
        Ok(())
    } else {
        Err(ProbeError::HttpStatus { code: status })
    }
}

/// DNS probe: raw UDP A-query.
fn check_dns(server: &str, port: u16, domain: &str, timeout: Duration) -> Result<(), ProbeError> {
    let addr = format!("{server}:{port}");

    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| ProbeError::DnsFailure {
        message: format!("bind: {e}"),
    })?;
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| ProbeError::DnsFailure {
            message: format!("set timeout: {e}"),
        })?;

    let query = build_dns_query(domain, 0xABCD);
    socket
        .send_to(&query, &addr)
        .map_err(|e| ProbeError::DnsFailure {
            message: format!("send: {e}"),
        })?;

    let mut buf = [0u8; 512];
    let (len, _) = socket.recv_from(&mut buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
            ProbeError::Timeout
        } else {
            ProbeError::DnsFailure {
                message: format!("recv: {e}"),
            }
        }
    })?;

    if len < 12 {
        return Err(ProbeError::DnsFailure {
            message: "response too short".into(),
        });
    }

    // Check response ID matches.
    let resp_id = u16::from_be_bytes([buf[0], buf[1]]);
    if resp_id != 0xABCD {
        return Err(ProbeError::DnsFailure {
            message: "response ID mismatch".into(),
        });
    }

    // Check QR bit (response flag) and RCODE.
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    if flags & 0x8000 == 0 {
        return Err(ProbeError::DnsFailure {
            message: "not a response".into(),
        });
    }
    let rcode = flags & 0x000F;
    if rcode != 0 {
        return Err(ProbeError::DnsFailure {
            message: format!("RCODE={rcode}"),
        });
    }

    // Check answer count > 0.
    let answer_count = u16::from_be_bytes([buf[6], buf[7]]);
    if answer_count == 0 {
        return Err(ProbeError::DnsFailure {
            message: "no answers".into(),
        });
    }

    Ok(())
}

/// Build a minimal DNS A-query packet.
fn build_dns_query(domain: &str, id: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);

    // Header.
    buf.extend_from_slice(&id.to_be_bytes()); // ID
    buf.extend_from_slice(&[0x01, 0x00]); // Flags: RD=1
    buf.extend_from_slice(&[0x00, 0x01]); // QDCOUNT=1
    buf.extend_from_slice(&[0x00, 0x00]); // ANCOUNT=0
    buf.extend_from_slice(&[0x00, 0x00]); // NSCOUNT=0
    buf.extend_from_slice(&[0x00, 0x00]); // ARCOUNT=0

    // Question: QNAME.
    for label in domain.split('.') {
        let len = label.len();
        if len > 63 {
            buf.push(63);
            buf.extend_from_slice(&label.as_bytes()[..63]);
        } else {
            #[allow(clippy::cast_possible_truncation)]
            buf.push(len as u8);
            buf.extend_from_slice(label.as_bytes());
        }
    }
    buf.push(0); // Root label.

    // QTYPE=A(1), QCLASS=IN(1).
    buf.extend_from_slice(&[0x00, 0x01]);
    buf.extend_from_slice(&[0x00, 0x01]);

    buf
}

/// Systemd probe: `systemctl is-active --quiet <unit>`.
fn check_systemd(unit: &str) -> Result<(), ProbeError> {
    let result = exec::run(
        &["systemctl", "is-active", "--quiet", unit],
        Duration::from_secs(5),
    )
    .map_err(|e| ProbeError::ExecFailed {
        exit_code: -1,
        stderr: e.to_string(),
    })?;

    if result.exit_code == 0 {
        Ok(())
    } else {
        Err(ProbeError::ExecFailed {
            exit_code: result.exit_code,
            stderr: format!("unit {unit} not active"),
        })
    }
}

/// Exec probe: run command, check exit code and optionally stdout.
fn check_exec(
    argv: &[String],
    timeout: Duration,
    expect: Option<&StdoutCheck>,
) -> Result<(), ProbeError> {
    if argv.is_empty() {
        return Err(ProbeError::ExecFailed {
            exit_code: -1,
            stderr: "empty argv".into(),
        });
    }

    let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let result = exec::run(&argv_refs, timeout).map_err(|e| ProbeError::ExecFailed {
        exit_code: -1,
        stderr: e.to_string(),
    })?;

    if result.exit_code != 0 {
        return Err(ProbeError::ExecFailed {
            exit_code: result.exit_code,
            stderr: result.stderr_text(),
        });
    }

    // Validate stdout if configured.
    if let Some(check) = expect {
        let stdout = result.stdout_text();
        match check {
            StdoutCheck::Contains { pattern } => {
                if !stdout.contains(pattern.as_str()) {
                    return Err(ProbeError::UnexpectedOutput {
                        detail: format!("stdout does not contain '{pattern}'"),
                    });
                }
            }
            StdoutCheck::NotContains { pattern } => {
                if stdout.contains(pattern.as_str()) {
                    return Err(ProbeError::UnexpectedOutput {
                        detail: format!("stdout contains '{pattern}'"),
                    });
                }
            }
            StdoutCheck::JsonField { pointer, expected } => {
                let parsed: serde_json::Value =
                    serde_json::from_str(&stdout).map_err(|e| ProbeError::UnexpectedOutput {
                        detail: format!("invalid JSON: {e}"),
                    })?;
                let value = parsed.pointer(pointer).and_then(|v| v.as_str());
                match value {
                    Some(v) if v == expected => {}
                    Some(v) => {
                        return Err(ProbeError::UnexpectedOutput {
                            detail: format!("{pointer} = '{v}', expected '{expected}'"),
                        });
                    }
                    None => {
                        return Err(ProbeError::UnexpectedOutput {
                            detail: format!("{pointer} not found in JSON"),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

// ─── Minimal URL parser ────────────────────────────────────────

struct ParsedUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Option<ParsedUrl> {
    let without_scheme = url.strip_prefix("http://")?;

    let (host_port, path) = match without_scheme.find('/') {
        Some(i) => (&without_scheme[..i], &without_scheme[i..]),
        None => (without_scheme, "/"),
    };

    let (host, port) = match host_port.rfind(':') {
        Some(i) => {
            let h = &host_port[..i];
            let p: u16 = host_port[i + 1..].parse().ok()?;
            (h.to_string(), p)
        }
        None => (host_port.to_string(), 80),
    };

    Some(ParsedUrl {
        host,
        port,
        path: path.to_string(),
    })
}

fn parse_http_status(response: &str) -> Option<u16> {
    // "HTTP/1.1 200 OK"
    let first_line = response.lines().next()?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() >= 2 {
        parts[1].parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn dns_query_packet_structure() {
        let packet = build_dns_query("example.com", 0x1234);
        // Header: 12 bytes.
        assert_eq!(packet[0], 0x12); // ID high
        assert_eq!(packet[1], 0x34); // ID low
        // QDCOUNT = 1.
        assert_eq!(packet[4], 0x00);
        assert_eq!(packet[5], 0x01);
        // First label: "example" (7 bytes).
        assert_eq!(packet[12], 7);
        assert_eq!(&packet[13..20], b"example");
        // Second label: "com" (3 bytes).
        assert_eq!(packet[20], 3);
        assert_eq!(&packet[21..24], b"com");
        // Root label.
        assert_eq!(packet[24], 0);
    }

    #[test]
    fn url_parse_with_port() {
        let u = parse_url("http://127.0.0.1:8080/v1/health").unwrap();
        assert_eq!(u.host, "127.0.0.1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.path, "/v1/health");
    }

    #[test]
    fn url_parse_default_port() {
        let u = parse_url("http://example.com/test").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, 80);
        assert_eq!(u.path, "/test");
    }

    #[test]
    fn url_parse_no_path() {
        let u = parse_url("http://localhost:9090").unwrap();
        assert_eq!(u.host, "localhost");
        assert_eq!(u.port, 9090);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn http_status_parsing() {
        assert_eq!(parse_http_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status("HTTP/1.1 503 Service Unavailable\r\n"), Some(503));
        assert_eq!(parse_http_status("garbage"), None);
    }
}