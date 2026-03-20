//! Minimal `sd_notify` implementation — no external dependencies.
//!
//! Protocol: send text messages to the Unix datagram socket
//! specified by `NOTIFY_SOCKET` environment variable.

#[cfg(unix)]
use std::os::unix::net::UnixDatagram;

/// Handle for sending notifications to systemd.
pub struct SdNotify {
    #[cfg(unix)]
    socket: Option<UnixDatagram>,
    #[cfg(unix)]
    path: String,
}

impl SdNotify {
    /// Creates a notifier from the environment.
    /// Returns a no-op notifier if `NOTIFY_SOCKET` is not set.
    #[must_use]
    pub fn from_env() -> Self {
        #[cfg(unix)]
        {
            let path = match std::env::var("NOTIFY_SOCKET") {
                Ok(p) if !p.is_empty() => p,
                _ => {
                    return Self {
                        socket: None,
                        path: String::new(),
                    }
                }
            };

            std::env::remove_var("NOTIFY_SOCKET");
            let socket = UnixDatagram::unbound().ok();

            Self { socket, path }
        }

        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// Sends a raw message to systemd.
    pub fn notify(&self, msg: &str) {
        #[cfg(unix)]
        self.notify_unix(msg);

        #[cfg(not(unix))]
        {
            let _ = (self, msg);
        }
    }

    #[cfg(unix)]
    fn notify_unix(&self, msg: &str) {
        let Some(ref socket) = self.socket else {
            return;
        };

        if self.path.starts_with('@') {
            let abstract_path = format!("\0{}", &self.path[1..]);
            if socket.send_to(msg.as_bytes(), &abstract_path).is_err() {
                let _ = socket.send_to(msg.as_bytes(), &self.path);
            }
            return;
        }

        let _ = socket.send_to(msg.as_bytes(), &self.path);
    }

    pub fn ready(&self) {
        self.notify("READY=1");
    }

    pub fn watchdog(&self) {
        self.notify("WATCHDOG=1");
    }

    pub fn stopping(&self) {
        self.notify("STOPPING=1");
    }

    pub fn status(&self, status: &str) {
        self.notify(&format!("STATUS={status}"));
    }
}

/// Returns recommended watchdog ping interval from `WATCHDOG_USEC`.
/// Returns half the interval as recommended by systemd docs.
#[allow(dead_code)] // Phase 4: adaptive watchdog interval
#[must_use]
pub fn watchdog_interval() -> Option<std::time::Duration> {
    let usec: u64 = std::env::var("WATCHDOG_USEC").ok()?.parse().ok()?;

    if let Ok(pid_str) = std::env::var("WATCHDOG_PID") {
        if let Ok(pid) = pid_str.parse::<u32>() {
            if pid != std::process::id() {
                return None;
            }
        }
    }

    Some(std::time::Duration::from_micros(usec / 2))
}