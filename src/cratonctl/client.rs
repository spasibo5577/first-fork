use crate::cratonctl::error::CratonctlError;
use serde::de::DeserializeOwned;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const DEFAULT_HTTP_PORT: u16 = 80;

#[derive(Debug, Clone)]
pub struct Client {
    endpoint: Endpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    host: String,
    port: u16,
    base_path: String,
}

#[derive(Debug)]
struct Response {
    status: u16,
    body: Vec<u8>,
}

impl Client {
    pub fn new(base_url: &str) -> Self {
        let endpoint = parse_base_url(base_url).unwrap_or_else(|_| Endpoint {
            host: "127.0.0.1".into(),
            port: 18800,
            base_path: String::new(),
        });
        Self { endpoint }
    }

    pub fn get_json<T>(&self, path: &str) -> Result<T, CratonctlError>
    where
        T: DeserializeOwned,
    {
        let response = self.send_request("GET", path, None, None)?;
        Self::decode_json_response(path, response)
    }

    pub fn post_json<T>(
        &self,
        path: &str,
        body: &str,
        bearer_token: &str,
    ) -> Result<T, CratonctlError>
    where
        T: DeserializeOwned,
    {
        let response = self.send_request("POST", path, Some(body), Some(bearer_token))?;
        Self::decode_json_response(path, response)
    }


    fn decode_json_response<T>(path: &str, response: Response) -> Result<T, CratonctlError>
    where
        T: DeserializeOwned,
    {
        let body_text = String::from_utf8(response.body)
            .map_err(|err| CratonctlError::Parse(format!("response body is not UTF-8: {err}")))?;

        if !(200..300).contains(&response.status) {
            return Err(CratonctlError::Daemon(daemon_error_message(
                path,
                response.status,
                &body_text,
            )));
        }

        serde_json::from_str(&body_text).map_err(|err| {
            CratonctlError::Parse(format!("failed to parse JSON from {path}: {err}"))
        })
    }

    fn send_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        bearer_token: Option<&str>,
    ) -> Result<Response, CratonctlError> {
        let address = format!("{}:{}", self.endpoint.host, self.endpoint.port);
        let socket_addr = address
            .to_socket_addrs()
            .map_err(|err| CratonctlError::Transport(format!("resolve {address}: {err}")))?
            .next()
            .ok_or_else(|| CratonctlError::Transport(format!("resolve {address}: no socket addresses")))?;
        let mut stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(3))
            .map_err(|err| CratonctlError::Transport(format!("connect {address}: {err}")))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

        let request_path = full_path(&self.endpoint.base_path, path);
        let mut request = format!(
            "{method} {request_path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n",
            self.endpoint.host
        );

        if let Some(token) = bearer_token {
            request.push_str("Authorization: Bearer ");
            request.push_str(token);
            request.push_str("\r\n");
        }

        if let Some(body) = body {
            request.push_str("Content-Type: application/json\r\n");
            let _ = std::fmt::Write::write_fmt(
                &mut request,
                format_args!("Content-Length: {}\r\n", body.len()),
            );
            request.push_str("\r\n");
            request.push_str(body);
        } else {
            request.push_str("\r\n");
        }

        stream.write_all(request.as_bytes()).map_err(|err| {
            CratonctlError::Transport(format!("write {method} {request_path}: {err}"))
        })?;
        stream.flush().map_err(|err| {
            CratonctlError::Transport(format!("flush {method} {request_path}: {err}"))
        })?;

        let mut response = Vec::new();
        stream.read_to_end(&mut response).map_err(|err| {
            CratonctlError::Transport(format!("read {method} {request_path}: {err}"))
        })?;

        parse_http_response(&response)
    }
}

pub fn path_segment(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, byte| {
        let is_unreserved =
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~');
        if is_unreserved {
            out.push(char::from(byte));
        } else {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{byte:02X}"));
        }
        out
    })
}

fn parse_base_url(url: &str) -> Result<Endpoint, CratonctlError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| CratonctlError::Config(format!("unsupported URL scheme in {url}")))?;
    let (authority, base_path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    if authority.is_empty() {
        return Err(CratonctlError::Config("missing host in URL".into()));
    }

    let (host, port) = authority.rsplit_once(':').map_or_else(
        || Ok((authority.to_string(), DEFAULT_HTTP_PORT)),
        |(host, port)| {
            let parsed_port = port.parse::<u16>().map_err(|err| {
                CratonctlError::Config(format!("invalid port in URL {url}: {err}"))
            })?;
            Ok((host.to_string(), parsed_port))
        },
    )?;

    if host.is_empty() {
        return Err(CratonctlError::Config("missing host in URL".into()));
    }

    let base_path = if base_path.is_empty() {
        String::new()
    } else {
        format!("/{}", base_path.trim_end_matches('/'))
    };

    Ok(Endpoint {
        host,
        port,
        base_path,
    })
}

fn full_path(base_path: &str, path: &str) -> String {
    if base_path.is_empty() {
        return path.to_string();
    }

    if path == "/" {
        return base_path.to_string();
    }

    format!("{base_path}{path}")
}

fn parse_http_response(raw: &[u8]) -> Result<Response, CratonctlError> {
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| CratonctlError::Parse("malformed HTTP response".into()))?;

    let (head, body) = raw.split_at(header_end + 4);
    let head_text = String::from_utf8(head.to_vec())
        .map_err(|err| CratonctlError::Parse(format!("response headers are not UTF-8: {err}")))?;
    let mut lines = head_text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| CratonctlError::Parse("missing HTTP status line".into()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| CratonctlError::Parse("missing HTTP status code".into()))?
        .parse::<u16>()
        .map_err(|err| CratonctlError::Parse(format!("invalid HTTP status code: {err}")))?;

    Ok(Response {
        status,
        body: body.to_vec(),
    })
}

fn daemon_error_message(path: &str, status: u16, body_text: &str) -> String {
    let error_detail = parse_error_body(body_text).unwrap_or_else(|| compact_body(body_text));
    match status {
        409 => format!("policy rejected request for {path}: {error_detail}"),
        503 => format!("daemon control loop is unavailable for {path}: {error_detail}"),
        504 => "daemon control loop did not respond in time".into(),
        _ => format!("HTTP {status} for {path}: {error_detail}"),
    }
}

fn parse_error_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get("error")?.as_str().map(str::to_string)
}

fn compact_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "(empty body)".into()
    } else {
        trimmed.replace(['\r', '\n'], " ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cratonctl::dto::CommandAcceptedResponse;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parse_base_url_with_port_and_base_path() {
        let endpoint = parse_base_url("http://127.0.0.1:18800/api")
            .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(
            endpoint,
            Endpoint {
                host: "127.0.0.1".into(),
                port: 18800,
                base_path: "/api".into(),
            }
        );
    }

    #[test]
    fn parse_http_response_extracts_status_and_body() {
        let response = parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
            .unwrap_or_else(|err| panic!("unexpected error: {err}"));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn path_segment_encodes_spaces() {
        assert_eq!(path_segment("hello world"), "hello%20world");
    }

    #[test]
    fn post_json_includes_authorization_header() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|err| panic!("unexpected bind error: {err}"));
        let address = listener
            .local_addr()
            .unwrap_or_else(|err| panic!("unexpected local_addr error: {err}"));

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener
                .accept()
                .unwrap_or_else(|err| panic!("unexpected accept error: {err}"));
            let mut request = [0_u8; 4096];
            let read = stream
                .read(&mut request)
                .unwrap_or_else(|err| panic!("unexpected read error: {err}"));
            let request_text = String::from_utf8(request[..read].to_vec())
                .unwrap_or_else(|err| panic!("unexpected utf8 error: {err}"));
            assert!(request_text.contains("Authorization: Bearer test-token"));
            stream
                .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 21\r\n\r\n{\"status\":\"accepted\"}")
                .unwrap_or_else(|err| panic!("unexpected write error: {err}"));
        });

        let client = Client::new(&format!("http://127.0.0.1:{}", address.port()));
        let response = client
            .post_json::<CommandAcceptedResponse>("/trigger/recovery", "{}", "test-token")
            .unwrap_or_else(|err| panic!("unexpected client error: {err}"));

        assert_eq!(response.status, "accepted");
        server
            .join()
            .unwrap_or_else(|_| panic!("unexpected server join failure"));
    }


    #[test]
    fn decode_json_response_reports_policy_rejection_reason() {
        let error = Client::decode_json_response::<CommandAcceptedResponse>(
            "/trigger/recovery",
            Response {
                status: 409,
                body: br#"{"error":"lease conflict"}"#.to_vec(),
            },
        )
        .err()
        .unwrap_or_else(|| panic!("expected daemon error"));
        assert_eq!(
            error.message(),
            "policy rejected request for /trigger/recovery: lease conflict"
        );
    }

    #[test]
    fn decode_json_response_reports_control_loop_timeout() {
        let error = Client::decode_json_response::<CommandAcceptedResponse>(
            "/api/v1/remediate",
            Response {
                status: 504,
                body: br#"{"error":"timeout"}"#.to_vec(),
            },
        )
        .err()
        .unwrap_or_else(|| panic!("expected daemon error"));
        assert_eq!(error.message(), "daemon control loop did not respond in time");
    }

    #[test]
    fn decode_json_response_reports_control_loop_unavailable() {
        let error = Client::decode_json_response::<CommandAcceptedResponse>(
            "/api/v1/remediate",
            Response {
                status: 503,
                body: br#"{"error":"control loop unavailable"}"#.to_vec(),
            },
        )
        .err()
        .unwrap_or_else(|| panic!("expected daemon error"));
        assert_eq!(
            error.message(),
            "daemon control loop is unavailable for /api/v1/remediate: control loop unavailable"
        );
    }

}
