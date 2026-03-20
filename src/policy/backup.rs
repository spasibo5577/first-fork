//! Backup FSM transitions — pure decision logic.
//!
//! Determines backup phase transitions and compensation actions.
//! Schedule format: `odd_days:HH:MM` or `daily:HH:MM`.

use crate::model::{BackupPhase, ServiceId, ServiceRestore, ServiceSnapshot};
use crate::schedule::{self, Schedule, WallClock};

/// Checks if a backup should start now.
#[must_use]
pub fn should_start(
    schedule_str: &str,
    now_wall: WallClock,
    last_backup_day: Option<u32>,
    current_phase: &BackupPhase,
) -> bool {
    if !current_phase.is_idle() {
        return false;
    }

    let Some(schedule) = parse_backup_schedule(schedule_str) else {
        return false;
    };
    schedule::is_due(&schedule, now_wall, last_backup_day)
}

/// Determines compensation actions for crash recovery from persisted phase.
#[must_use]
pub fn crash_compensation(phase: &BackupPhase) -> Vec<CompensationAction> {
    let mut actions = Vec::new();

    if phase.is_idle() {
        return actions;
    }

    if phase.needs_restic_unlock() {
        actions.push(CompensationAction::ResticUnlock);
    }

    if phase.needs_service_recovery() {
        if let Some(snapshots) = phase.pre_backup_services() {
            for snap in snapshots {
                if snap.was_running {
                    actions.push(CompensationAction::StartService {
                        id: snap.id.clone(),
                        unit: snap.unit.clone(),
                    });
                }
            }
        } else {
            // Defensive: phase needs recovery but no snapshot data.
            actions.push(CompensationAction::StartServiceByName {
                name: "continuwuity".to_string(),
            });
        }
    }

    actions.push(CompensationAction::ResetToIdle);
    actions
}

/// Actions that crash compensation should perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompensationAction {
    ResticUnlock,
    StartService { id: ServiceId, unit: String },
    /// Fallback when pre-backup snapshot data is unavailable.
    StartServiceByName { name: String },
    ResetToIdle,
}

/// Determines next backup phase after current one completes.
///
/// Used by the reducer to advance the backup FSM. The `pre_backup`
/// snapshot is threaded through phases that need crash-recovery data.
#[allow(dead_code)] // Phase 4: backup FSM advancement in reducer
#[must_use]
pub fn next_phase(
    current: &BackupPhase,
    run_id: &str,
    pre_backup: &[ServiceSnapshot],
    verify_enabled: bool,
) -> BackupPhase {
    match current {
        BackupPhase::Idle => BackupPhase::Locked {
            run_id: run_id.to_string(),
        },
        BackupPhase::Locked { .. } => BackupPhase::ResticUnlocking {
            run_id: run_id.to_string(),
        },
        BackupPhase::ResticUnlocking { .. } => BackupPhase::ServicesStopping {
            run_id: run_id.to_string(),
            pre_backup_state: pre_backup.to_vec(),
        },
        BackupPhase::ServicesStopping { .. } => BackupPhase::ResticRunning {
            run_id: run_id.to_string(),
            pre_backup_state: pre_backup.to_vec(),
        },
        BackupPhase::ResticRunning { .. } => BackupPhase::ServicesStarting {
            run_id: run_id.to_string(),
            remaining: pre_backup
                .iter()
                .filter(|s| s.was_running)
                .map(|s| ServiceRestore {
                    id: s.id.clone(),
                    unit: s.unit.clone(),
                    attempts: 0,
                    docker_restarted: false,
                })
                .collect(),
        },
        BackupPhase::ServicesStarting { .. } => BackupPhase::ServicesVerifying {
            run_id: run_id.to_string(),
            started: pre_backup
                .iter()
                .filter(|s| s.was_running)
                .map(|s| s.id.clone())
                .collect(),
        },
        BackupPhase::ServicesVerifying { .. } => BackupPhase::RetentionRunning {
            run_id: run_id.to_string(),
        },
        BackupPhase::RetentionRunning { .. } => {
            if verify_enabled {
                BackupPhase::Verifying {
                    run_id: run_id.to_string(),
                }
            } else {
                BackupPhase::Idle
            }
        }
        BackupPhase::Verifying { .. } => BackupPhase::Idle,
    }
}

fn parse_backup_schedule(s: &str) -> Option<Schedule> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let hour: u32 = parts[1].parse().ok()?;
    let minute: u32 = parts[2].parse().ok()?;

    match parts[0] {
        "odd_days" => Some(Schedule::OddDays { hour, minute }),
        "daily" => Some(Schedule::Daily { hour, minute }),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn idle_to_locked() {
        let next = next_phase(&BackupPhase::Idle, "run-1", &[], false);
        assert!(matches!(next, BackupPhase::Locked { .. }));
    }

    #[test]
    fn full_cycle_without_verify() {
        let pre = vec![ServiceSnapshot {
            id: ServiceId("continuwuity".into()),
            was_running: true,
            unit: "continuwuity.service".into(),
        }];

        let mut phase = BackupPhase::Idle;
        for _ in 0..8 {
            phase = next_phase(&phase, "r1", &pre, false);
        }
        assert!(phase.is_idle(), "expected Idle, got {phase:?}");
    }

    #[test]
    fn crash_in_restic_running_recovers_services() {
        let pre = vec![
            ServiceSnapshot {
                id: ServiceId("continuwuity".into()),
                was_running: true,
                unit: "continuwuity.service".into(),
            },
            ServiceSnapshot {
                id: ServiceId("gatus".into()),
                was_running: false,
                unit: "gatus.service".into(),
            },
        ];

        let phase = BackupPhase::ResticRunning {
            run_id: "r1".into(),
            pre_backup_state: pre,
        };

        let actions = crash_compensation(&phase);

        assert!(actions.contains(&CompensationAction::ResticUnlock));
        assert!(actions.iter().any(|a| matches!(
            a,
            CompensationAction::StartService { id, .. } if id.as_str() == "continuwuity"
        )));
        assert!(!actions.iter().any(|a| matches!(
            a,
            CompensationAction::StartService { id, .. } if id.as_str() == "gatus"
        )));
        assert!(actions.contains(&CompensationAction::ResetToIdle));
    }

    #[test]
    fn crash_in_idle_no_actions() {
        assert!(crash_compensation(&BackupPhase::Idle).is_empty());
    }

    #[test]
    fn crash_in_retention_no_service_recovery() {
        let phase = BackupPhase::RetentionRunning {
            run_id: "r1".into(),
        };
        let actions = crash_compensation(&phase);
        assert!(!actions
            .iter()
            .any(|a| matches!(a, CompensationAction::StartService { .. })));
        assert!(actions.contains(&CompensationAction::ResetToIdle));
    }

    #[test]
    fn should_start_on_odd_day() {
        let wall = WallClock {
            year: 2025,
            month: 1,
            day: 15,
            hour: 4,
            minute: 0,
            second: 0,
            weekday: 2,
        };
        assert!(should_start(
            "odd_days:04:00",
            wall,
            None,
            &BackupPhase::Idle
        ));
    }

    #[test]
    fn should_not_start_on_even_day() {
        let wall = WallClock {
            year: 2025,
            month: 1,
            day: 16,
            hour: 4,
            minute: 0,
            second: 0,
            weekday: 3,
        };
        assert!(!should_start(
            "odd_days:04:00",
            wall,
            None,
            &BackupPhase::Idle
        ));
    }

    #[test]
    fn should_not_start_if_already_ran_today() {
        let wall = WallClock {
            year: 2025,
            month: 1,
            day: 15,
            hour: 4,
            minute: 0,
            second: 0,
            weekday: 2,
        };
        assert!(!should_start(
            "odd_days:04:00",
            wall,
            Some(15),
            &BackupPhase::Idle
        ));
    }

    #[test]
    fn should_not_start_if_not_idle() {
        let wall = WallClock {
            year: 2025,
            month: 1,
            day: 15,
            hour: 4,
            minute: 0,
            second: 0,
            weekday: 2,
        };
        let phase = BackupPhase::Locked {
            run_id: "x".into(),
        };
        assert!(!should_start("odd_days:04:00", wall, None, &phase));
    }
}