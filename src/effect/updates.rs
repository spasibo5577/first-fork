//! APT and Docker update checks.
//!
//! Parses `apt-get -s dist-upgrade` output to count pending updates.
//! Compares Docker image IDs to detect available updates.

use std::time::Duration;

/// Result of checking APT updates.
#[allow(dead_code)] // Phase 4: returned by check_apt, consumed in reducer
#[derive(Debug)]
pub struct AptCheckResult {
    pub upgradeable: u32,
    pub security: u32,
    pub packages: Vec<String>,
}

/// Checks for available APT updates by running `apt-get -s dist-upgrade`.
///
/// Returns `None` if the command fails.
#[allow(dead_code)] // Phase 4: wired via CheckAptUpdates command
#[must_use]
pub fn check_apt() -> Option<AptCheckResult> {
    let result = crate::effect::exec::run(
        &["apt-get", "-s", "dist-upgrade"],
        Duration::from_secs(120),
    )
    .ok()?;

    if result.exit_code != 0 {
        return None;
    }

    let stdout = result.stdout_text();
    Some(parse_apt_simulate(&stdout))
}

/// Parses the output of `apt-get -s dist-upgrade`.
fn parse_apt_simulate(output: &str) -> AptCheckResult {
    let mut packages = Vec::new();
    let mut security = 0u32;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Inst ") {
            let pkg_name = rest.split_whitespace().next().unwrap_or("").to_string();
            if !pkg_name.is_empty() {
                packages.push(pkg_name);
            }
            if rest.contains("security") || rest.contains("Security") {
                security += 1;
            }
        }
    }

    let upgradeable = u32::try_from(packages.len()).unwrap_or(u32::MAX);

    AptCheckResult {
        upgradeable,
        security,
        packages,
    }
}

/// Checks if a Docker image has an update available.
///
/// Pulls the image and compares the new ID with the current one.
/// Returns `true` if an update is available.
#[allow(dead_code)] // Phase 4: wired via CheckDockerUpdates command
#[must_use]
pub fn check_docker_image(image: &str) -> Option<bool> {
    let current = crate::effect::exec::run(
        &["docker", "inspect", "--format", "{{.Id}}", image],
        Duration::from_secs(15),
    )
    .ok()?;

    if current.exit_code != 0 {
        return None;
    }
    let current_id = current.stdout_text();

    let pull = crate::effect::exec::run(
        &["docker", "pull", "-q", image],
        Duration::from_secs(300),
    )
    .ok()?;

    if pull.exit_code != 0 {
        return None;
    }

    let updated = crate::effect::exec::run(
        &["docker", "inspect", "--format", "{{.Id}}", image],
        Duration::from_secs(15),
    )
    .ok()?;

    if updated.exit_code != 0 {
        return None;
    }
    let updated_id = updated.stdout_text();

    Some(current_id != updated_id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_apt_empty() {
        let result = parse_apt_simulate("");
        assert_eq!(result.upgradeable, 0);
        assert_eq!(result.security, 0);
    }

    #[test]
    fn parse_apt_with_packages() {
        let output = "\
Reading package lists...
Building dependency tree...
Inst libssl3 (3.0.13-1 Debian:13/stable [arm64]) [security]
Inst curl (7.88.1-10+deb12u5 Debian:12.5/stable [arm64])
Inst wget (1.21.3-1 Debian:12/stable [arm64])
Conf libssl3 (3.0.13-1 Debian:13/stable [arm64])
Conf curl (7.88.1-10+deb12u5 Debian:12.5/stable [arm64])
Conf wget (1.21.3-1 Debian:12/stable [arm64])";

        let result = parse_apt_simulate(output);
        assert_eq!(result.upgradeable, 3);
        assert_eq!(result.security, 1);
        assert!(result.packages.contains(&"libssl3".to_string()));
        assert!(result.packages.contains(&"curl".to_string()));
        assert!(result.packages.contains(&"wget".to_string()));
    }

    #[test]
    fn parse_apt_no_updates() {
        let output = "\
Reading package lists...
Building dependency tree...
0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.";

        let result = parse_apt_simulate(output);
        assert_eq!(result.upgradeable, 0);
    }
}