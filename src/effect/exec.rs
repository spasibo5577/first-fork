//! Safe process execution with mandatory timeouts, process group
//! isolation, and graceful kill escalation.
//!
//! Every external command runs in its own process group (`setpgid`).
//! On timeout: SIGTERM to group → 5s grace → SIGKILL to group.

use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Outcome of running an external command.
#[derive(Debug)]
pub struct ExecResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    #[allow(dead_code)] // Phase 4: effect result reporting to reducer
    pub duration: Duration,
    #[allow(dead_code)] // Phase 4: effect result reporting to reducer
    pub killed: bool,
}

impl ExecResult {
    /// Returns stdout as lossy UTF-8, trimmed.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }

    /// Returns stderr as lossy UTF-8, trimmed.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Runs a command with the given timeout.
///
/// # Errors
/// Returns an error if the command cannot be started.
/// A non-zero exit code is NOT an error — check `result.exit_code`.
pub fn run(argv: &[&str], timeout: Duration) -> Result<ExecResult, ExecError> {
    if argv.is_empty() {
        return Err(ExecError::EmptyArgv);
    }

    let start = Instant::now();

    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }

    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Process group isolation (Unix only).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn().map_err(|e| ExecError::Spawn {
        cmd: argv[0].to_string(),
        source: e,
    })?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = collect_output(&mut child);
                return Ok(ExecResult {
                    stdout: output.0,
                    stderr: output.1,
                    exit_code: exit_code_from_status(status),
                    duration: start.elapsed(),
                    killed: false,
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    return Ok(kill_child(&mut child, start));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(ExecError::Wait {
                    cmd: argv[0].to_string(),
                    source: e,
                });
            }
        }
    }
}

/// Runs a command only if not in dry-run mode, or if read-only.
pub fn run_dry_aware(
    argv: &[&str],
    timeout: Duration,
    dry_run: bool,
) -> Result<ExecResult, ExecError> {
    if dry_run && !is_read_only(argv) {
        eprintln!("DRY-RUN: would execute: {}", argv.join(" "));
        return Ok(ExecResult {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
            duration: Duration::ZERO,
            killed: false,
        });
    }
    run(argv, timeout)
}

/// Kill with escalation: SIGTERM → 5s → SIGKILL.
fn kill_child(child: &mut std::process::Child, start: Instant) -> ExecResult {
    #[allow(unused_variables)]
    let pid = child.id();

    #[cfg(unix)]
    {
        #[allow(clippy::cast_possible_wrap)]
        let pgid = pid as i32;
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }

    let kill_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = collect_output(child);
                return ExecResult {
                    stdout: output.0,
                    stderr: output.1,
                    exit_code: -1,
                    duration: start.elapsed(),
                    killed: true,
                };
            }
            Ok(None) if Instant::now() >= kill_deadline => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }

    #[cfg(unix)]
    {
        #[allow(clippy::cast_possible_wrap)]
        let pgid = pid as i32;
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }

    let _ = child.wait();
    let output = collect_output(child);

    ExecResult {
        stdout: output.0,
        stderr: output.1,
        exit_code: -9,
        duration: start.elapsed(),
        killed: true,
    }
}

/// Collects stdout/stderr, truncating to 64KB.
fn collect_output(child: &mut std::process::Child) -> (Vec<u8>, Vec<u8>) {
    const MAX: usize = 64 * 1024;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    if let Some(ref mut out) = child.stdout {
        let _ = io::Read::take(out, MAX as u64).read_to_end(&mut stdout);
    }
    if let Some(ref mut err) = child.stderr {
        let _ = io::Read::take(err, MAX as u64).read_to_end(&mut stderr);
    }

    (stdout, stderr)
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.code().unwrap_or_else(|| {
            status.signal().map_or(-1, |s| -s)
        })
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(-1)
    }
}

/// Read-only command whitelist for dry-run mode.
fn is_read_only(argv: &[&str]) -> bool {
    if argv.is_empty() {
        return false;
    }

    let name = argv[0];
    if matches!(name, "du" | "df" | "free" | "cat" | "journalctl" | "find") {
        return true;
    }

    if argv.len() >= 2 {
        let key = format!("{} {}", argv[0], argv[1]);
        if matches!(
            key.as_str(),
            "systemctl is-active"
                | "systemctl status"
                | "docker info"
                | "docker inspect"
                | "docker ps"
                | "tailscale status"
                | "restic snapshots"
                | "restic check"
                | "apt-get -s"
        ) {
            return true;
        }
    }

    false
}

#[derive(Debug)]
pub enum ExecError {
    EmptyArgv,
    Spawn { cmd: String, source: io::Error },
    Wait { cmd: String, source: io::Error },
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyArgv => f.write_str("empty argv"),
            Self::Spawn { cmd, source } => write!(f, "spawning {cmd}: {source}"),
            Self::Wait { cmd, source } => write!(f, "waiting for {cmd}: {source}"),
        }
    }
}

impl std::error::Error for ExecError {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn echo_succeeds() {
        #[cfg(unix)]
        let result = run(&["echo", "hello"], Duration::from_secs(5)).unwrap();
        #[cfg(not(unix))]
        let result = run(&["cmd", "/C", "echo", "hello"], Duration::from_secs(5)).unwrap();

        assert_eq!(result.exit_code, 0);
        assert!(!result.killed);
        assert!(!result.stdout.is_empty());
    }

    #[test]
    fn nonexistent_command_errors() {
        let result = run(
            &["this_command_does_not_exist_12345"],
            Duration::from_secs(1),
        );
        assert!(result.is_err());
    }

    #[test]
    fn empty_argv_errors() {
        assert!(matches!(
            run(&[], Duration::from_secs(1)),
            Err(ExecError::EmptyArgv)
        ));
    }

    #[test]
    fn dry_run_blocks_mutation() {
        let result =
            run_dry_aware(&["systemctl", "restart", "ntfy"], Duration::from_secs(5), true)
                .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.duration == Duration::ZERO);
    }

    #[test]
    fn dry_run_allows_read_only() {
        assert!(is_read_only(&["systemctl", "is-active", "ntfy"]));
        assert!(is_read_only(&["docker", "info"]));
        assert!(is_read_only(&["journalctl", "-u", "ntfy"]));
        assert!(!is_read_only(&["systemctl", "restart", "ntfy"]));
        assert!(!is_read_only(&["docker", "restart", "x"]));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_process() {
        let result = run(&["sleep", "60"], Duration::from_millis(200)).unwrap();
        assert!(result.killed);
    }
}