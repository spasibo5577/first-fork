//! Schedule rules for recurring tasks.
//!
//! Pure time computation — no threads, no timers, no side effects.
//! The control loop calls `is_due()` to check if a task should run.

use serde::{Deserialize, Serialize};

/// When a task should execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Schedule {
    /// Run every N seconds.
    Interval {
        interval_secs: u64,
        #[serde(default)]
        initial_delay_secs: u64,
    },
    /// Run once per day at HH:MM.
    Daily { hour: u32, minute: u32 },
    /// Run once per week on a specific day at HH:MM.
    Weekly {
        /// 0 = Monday, 6 = Sunday.
        weekday: u32,
        hour: u32,
        minute: u32,
    },
    /// Run on odd-numbered calendar days at HH:MM.
    OddDays { hour: u32, minute: u32 },
}

/// Simple wall-clock representation for schedule calculation.
/// We avoid pulling in `chrono` — these fields are sufficient.
#[allow(dead_code)] // structural completeness: all fields populated, some consumed in later phases
#[derive(Debug, Clone, Copy)]
pub struct WallClock {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Monday, 6 = Sunday (ISO 8601).
    pub weekday: u32,
}

impl WallClock {
    /// Reads the current local time from the system.
    #[must_use]
    pub fn now() -> Self {
        let epoch = unsafe { libc::time(std::ptr::null_mut()) };
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };

        #[cfg(unix)]
        unsafe {
            libc::localtime_r(&epoch, &mut tm);
        }

        #[cfg(target_os = "windows")]
        unsafe {
            libc::localtime_s(&raw mut tm, &raw const epoch);
        }

        WallClock {
            year: tm.tm_year + 1900,
            month: u32::try_from(tm.tm_mon + 1).unwrap_or(1),
            day: u32::try_from(tm.tm_mday).unwrap_or(1),
            hour: u32::try_from(tm.tm_hour).unwrap_or(0),
            minute: u32::try_from(tm.tm_min).unwrap_or(0),
            second: u32::try_from(tm.tm_sec).unwrap_or(0),
            weekday: u32::try_from((tm.tm_wday + 6) % 7).unwrap_or(0),
        }
    }

    /// Total minutes since midnight.
    #[must_use]
    pub fn minutes_since_midnight(self) -> u32 {
        self.hour * 60 + self.minute
    }

    /// Returns true if the calendar day is odd.
    #[must_use]
    pub fn is_odd_day(self) -> bool {
        self.day % 2 == 1
    }
}

/// Determines whether a task with the given schedule is due to run.
///
/// `last_run_day`: calendar day (1-31) when the task last ran.
/// Prevents running twice on the same day for daily/weekly/odd schedules.
#[must_use]
pub fn is_due(schedule: &Schedule, now: WallClock, last_run_day: Option<u32>) -> bool {
    match schedule {
        Schedule::Interval { .. } => {
            // Interval tasks tracked by elapsed monotonic time, not wall clock.
            false
        }
        Schedule::Daily { hour, minute } => {
            let target = *hour * 60 + *minute;
            let current = now.minutes_since_midnight();
            let in_window = current >= target && current < target + 5;
            let not_run_today = last_run_day != Some(now.day);
            in_window && not_run_today
        }
        Schedule::Weekly {
            weekday,
            hour,
            minute,
        } => {
            if now.weekday != *weekday {
                return false;
            }
            let target = *hour * 60 + *minute;
            let current = now.minutes_since_midnight();
            let in_window = current >= target && current < target + 5;
            let not_run_today = last_run_day != Some(now.day);
            in_window && not_run_today
        }
        Schedule::OddDays { hour, minute } => {
            if !now.is_odd_day() {
                return false;
            }
            let target = *hour * 60 + *minute;
            let current = now.minutes_since_midnight();
            let in_window = current >= target && current < target + 5;
            let not_run_today = last_run_day != Some(now.day);
            in_window && not_run_today
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn clock(day: u32, hour: u32, minute: u32, weekday: u32) -> WallClock {
        WallClock {
            year: 2025,
            month: 1,
            day,
            hour,
            minute,
            second: 0,
            weekday,
        }
    }

    #[test]
    fn daily_due() {
        let sched = Schedule::Daily { hour: 9, minute: 0 };
        assert!(is_due(&sched, clock(15, 9, 0, 2), None));
    }

    #[test]
    fn daily_not_due_wrong_time() {
        let sched = Schedule::Daily { hour: 9, minute: 0 };
        assert!(!is_due(&sched, clock(15, 8, 59, 2), None));
    }

    #[test]
    fn daily_not_due_already_ran() {
        let sched = Schedule::Daily { hour: 9, minute: 0 };
        assert!(!is_due(&sched, clock(15, 9, 2, 2), Some(15)));
    }

    #[test]
    fn daily_due_ran_yesterday() {
        let sched = Schedule::Daily { hour: 9, minute: 0 };
        assert!(is_due(&sched, clock(15, 9, 2, 2), Some(14)));
    }

    #[test]
    fn odd_days_due() {
        let sched = Schedule::OddDays { hour: 4, minute: 0 };
        assert!(is_due(&sched, clock(15, 4, 0, 2), None));
    }

    #[test]
    fn odd_days_not_due_even_day() {
        let sched = Schedule::OddDays { hour: 4, minute: 0 };
        assert!(!is_due(&sched, clock(16, 4, 0, 3), None));
    }

    #[test]
    fn weekly_due_correct_day() {
        let sched = Schedule::Weekly {
            weekday: 6,
            hour: 10,
            minute: 0,
        };
        assert!(is_due(&sched, clock(19, 10, 0, 6), None));
    }

    #[test]
    fn weekly_not_due_wrong_day() {
        let sched = Schedule::Weekly {
            weekday: 6,
            hour: 10,
            minute: 0,
        };
        assert!(!is_due(&sched, clock(15, 10, 0, 2), None));
    }

    #[test]
    fn window_is_5_minutes() {
        let sched = Schedule::Daily { hour: 9, minute: 0 };
        assert!(is_due(&sched, clock(15, 9, 4, 2), None));
        assert!(!is_due(&sched, clock(15, 9, 5, 2), None));
    }

    #[test]
    fn wallclock_now_does_not_panic() {
        let now = WallClock::now();
        assert!(now.year >= 2024);
        assert!((1..=12).contains(&now.month));
        assert!((1..=31).contains(&now.day));
        assert!(now.hour < 24);
        assert!(now.minute < 60);
        assert!(now.weekday < 7);
    }
}