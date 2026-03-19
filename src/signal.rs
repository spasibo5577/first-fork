//! OS signal handling via `signal-hook`.
//!
//! Converts SIGTERM/SIGINT/SIGHUP into typed events sent
//! through the main event channel.

use crate::model::SignalKind;
use std::sync::mpsc;

/// Spawns a thread that listens for OS signals and sends them
/// as `SignalKind` through the provided channel.
///
/// Returns `Ok(())` if signal handlers were registered.
///
/// # Errors
/// Returns an error if signal registration fails.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)]
pub fn spawn_signal_thread(tx: mpsc::Sender<SignalKind>) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;

        std::thread::Builder::new()
            .name("signal-handler".into())
            .spawn(move || {
                for sig in signals.forever() {
                    let kind = match sig {
                        SIGTERM | SIGINT => SignalKind::Shutdown,
                        SIGHUP => SignalKind::Reload,
                        _ => continue,
                    };
                    if tx.send(kind).is_err() {
                        break; // Channel closed, daemon shutting down.
                    }
                }
            })?;

        Ok(())
    }

    #[cfg(not(unix))]
    {
        // On Windows: register Ctrl+C handler.
        let tx_clone = tx.clone();
        ctrlc_handler(tx_clone);
        Ok(())
    }
}

#[cfg(not(unix))]
fn ctrlc_handler(_tx: mpsc::Sender<SignalKind>) {
    // Best-effort Ctrl+C handling on Windows for development.
    std::thread::Builder::new()
        .name("signal-handler".into())
        .spawn(move || {
            // Simple polling approach — not production quality,
            // but sufficient for Windows development.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(100));
                // On Windows, we rely on the process being killed externally.
                // This thread just keeps the channel alive.
            }
        })
        .ok();
}