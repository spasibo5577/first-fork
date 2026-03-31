//! Disk monitoring and cleanup via `statfs` syscall and system commands.
//!
//! No parsing of `df` output — direct syscall for reliability.

use crate::state::DiskSample;
use std::time::Duration;

/// Reads current disk usage for the given path via `statfs`.
///
/// Returns `None` on Windows (dev builds) or if syscall fails.
#[allow(dead_code)] // Phase 4: wired via CheckDiskUsage command
#[allow(clippy::unnecessary_wraps)] // Unix path genuinely returns None on statfs failure
#[must_use]
pub fn get_usage(path: &str, mono_secs: u64) -> Option<DiskSample> {
    #[cfg(unix)]
    {
        let c_path = std::ffi::CString::new(path).ok()?;
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        let ret = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
        if ret != 0 {
            crate::log::raw(&format!(
                "[cratond] statfs({path}) failed: {}",
                std::io::Error::last_os_error()
            ));
            return None;
        }

        let total = stat.f_blocks as u64 * stat.f_bsize as u64;
        let free = stat.f_bfree as u64 * stat.f_bsize as u64;

        if total == 0 {
            return None;
        }

        #[allow(clippy::cast_possible_truncation)]
        let usage_percent = (((total - free) * 100) / total) as u32;

        Some(DiskSample {
            mono: mono_secs,
            usage_percent,
            free_bytes: free,
        })
    }

    #[cfg(not(unix))]
    {
        let _ = (path, mono_secs);
        Some(DiskSample {
            mono: mono_secs,
            usage_percent: 50,
            free_bytes: 10_000_000_000,
        })
    }
}

/// Standard cleanup: `apt clean` + journal vacuum.
#[allow(dead_code)] // Phase 4: wired via RunDiskCleanup command
pub fn cleanup_standard() {
    crate::log::raw("[cratond] disk cleanup: standard");

    let _ = crate::effect::exec::run(&["apt-get", "clean"], Duration::from_secs(60));

    let _ = crate::effect::exec::run(
        &["journalctl", "--vacuum-time=3d"],
        Duration::from_secs(30),
    );
}

/// Aggressive cleanup: standard + `docker image prune -a`.
///
/// Only call when docker-daemon lease is free and backup is not running.
#[allow(dead_code)] // Phase 4: wired via RunDiskCleanup command
pub fn cleanup_aggressive() {
    cleanup_standard();

    crate::log::raw("[cratond] disk cleanup: aggressive (docker prune)");
    let _ = crate::effect::exec::run(
        &["docker", "image", "prune", "-a", "-f"],
        Duration::from_secs(120),
    );
}

/// Formats bytes into human-readable string.
#[allow(dead_code)] // Phase 4: used in disk alerts
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        #[allow(clippy::cast_precision_loss)]
        let gb = bytes as f64 / GB as f64;
        format!("{gb:.1} GB")
    } else if bytes >= MB {
        #[allow(clippy::cast_precision_loss)]
        let mb = bytes as f64 / MB as f64;
        format!("{mb:.1} MB")
    } else {
        #[allow(clippy::cast_precision_loss)]
        let kb = bytes as f64 / KB as f64;
        format!("{kb:.0} KB")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formatting() {
        assert_eq!(human_bytes(500), "0 KB");
        assert_eq!(human_bytes(1_500_000), "1.4 MB");
        assert_eq!(human_bytes(2_500_000_000), "2.3 GB");
    }

    #[test]
    fn get_usage_returns_something() {
        #[cfg(unix)]
        {
            let sample = get_usage("/", 100);
            assert!(sample.is_some());
            let s = sample.unwrap();
            assert!(s.usage_percent <= 100);
            assert!(s.free_bytes > 0);
        }
        #[cfg(not(unix))]
        {
            let sample = get_usage("C:\\", 100);
            assert!(sample.is_some());
        }
    }
}
