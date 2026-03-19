//! Circuit breaker — pure functions for state transitions.
//!
//! The breaker protects against restart storms. When a service
//! is restarted too many times within a window, the breaker
//! trips (`Open`) and suppresses further restarts until cooldown.
//!
//! States: `Closed` → `Open` → `HalfOpen` → `Closed`.

use crate::model::BreakerState;

/// Records a restart attempt. Returns the new breaker state.
///
/// If `restarts_in_window >= threshold` and the breaker is `Closed`,
/// it transitions to `Open`.
#[must_use]
pub fn record_restart(
    breaker: &BreakerState,
    restarts_in_window: u32,
    threshold: u32,
    cooldown_secs: u64,
    now_mono: u64,
) -> BreakerState {
    match breaker {
        BreakerState::Closed => {
            if restarts_in_window >= threshold {
                BreakerState::Open {
                    until_mono_secs: now_mono + cooldown_secs,
                    trip_count: 1,
                }
            } else {
                BreakerState::Closed
            }
        }
        BreakerState::HalfOpen {
            previous_trip_count,
            ..
        } => {
            // Restart during half-open means the probe attempt failed.
            // Re-open with incremented trip count.
            BreakerState::Open {
                until_mono_secs: now_mono + cooldown_secs,
                trip_count: previous_trip_count + 1,
            }
        }
        BreakerState::Open { .. } => breaker.clone(),
    }
}

/// Called when a health probe succeeds. May transition `HalfOpen` → `Closed`.
#[must_use]
pub fn on_healthy_probe(breaker: &BreakerState) -> BreakerState {
    match breaker {
        BreakerState::HalfOpen { .. } => BreakerState::Closed,
        other => other.clone(),
    }
}

/// Checks if the breaker should transition from `Open` → `HalfOpen`.
/// Call this before making recovery decisions.
#[must_use]
pub fn maybe_transition(breaker: &BreakerState, now_mono: u64) -> BreakerState {
    match breaker {
        BreakerState::Open {
            until_mono_secs,
            trip_count,
        } if now_mono >= *until_mono_secs => BreakerState::HalfOpen {
            probe_attempt: 0,
            previous_trip_count: *trip_count,
        },
        other => other.clone(),
    }
}

/// Returns true if recovery is currently allowed.
#[must_use]
pub fn allows_recovery(breaker: &BreakerState, now_mono: u64) -> bool {
    match breaker {
        BreakerState::Closed | BreakerState::HalfOpen { .. } => true,
        BreakerState::Open {
            until_mono_secs, ..
        } => now_mono >= *until_mono_secs,
    }
}

/// Explicitly resets the breaker to `Closed` (e.g., operator action).
#[must_use]
pub fn reset() -> BreakerState {
    BreakerState::Closed
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn closed_stays_closed_below_threshold() {
        let b = record_restart(&BreakerState::Closed, 2, 3, 3600, 100);
        assert!(matches!(b, BreakerState::Closed));
    }

    #[test]
    fn trips_at_threshold() {
        let b = record_restart(&BreakerState::Closed, 3, 3, 3600, 100);
        assert!(matches!(
            b,
            BreakerState::Open {
                until_mono_secs: 3700,
                trip_count: 1
            }
        ));
    }

    #[test]
    fn open_denies_recovery() {
        assert!(!allows_recovery(
            &BreakerState::Open {
                until_mono_secs: 200,
                trip_count: 1
            },
            100
        ));
    }

    #[test]
    fn open_allows_after_cooldown() {
        assert!(allows_recovery(
            &BreakerState::Open {
                until_mono_secs: 200,
                trip_count: 1
            },
            200
        ));
    }

    #[test]
    fn open_transitions_to_half_open() {
        let b = maybe_transition(
            &BreakerState::Open {
                until_mono_secs: 100,
                trip_count: 1,
            },
            100,
        );
        assert!(matches!(b, BreakerState::HalfOpen { previous_trip_count: 1, .. }));
    }

    #[test]
    fn open_stays_open_before_cooldown() {
        let b = maybe_transition(
            &BreakerState::Open {
                until_mono_secs: 200,
                trip_count: 1,
            },
            100,
        );
        assert!(matches!(b, BreakerState::Open { .. }));
    }

    #[test]
    fn half_open_closes_on_healthy() {
        let b = on_healthy_probe(&BreakerState::HalfOpen {
            probe_attempt: 0,
            previous_trip_count: 1,
        });
        assert!(matches!(b, BreakerState::Closed));
    }

    #[test]
    fn half_open_reopens_on_restart() {
        let b = record_restart(
            &BreakerState::HalfOpen {
                probe_attempt: 0,
                previous_trip_count: 1,
            },
            1,
            3,
            3600,
            500,
        );
        match b {
            BreakerState::Open { trip_count, .. } => assert_eq!(trip_count, 2),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn trip_count_increments() {
        // Start: Open with trip_count=2
        let b1 = BreakerState::Open {
            until_mono_secs: 100,
            trip_count: 2,
        };
        // Cooldown expires → HalfOpen (carries previous_trip_count=2)
        let b2 = maybe_transition(&b1, 100);
        assert!(matches!(
            b2,
            BreakerState::HalfOpen {
                previous_trip_count: 2,
                ..
            }
        ));
        // Restart during HalfOpen → Open with trip_count=3
        let b3 = record_restart(&b2, 1, 3, 3600, 200);
        match b3 {
            BreakerState::Open { trip_count, .. } => assert_eq!(trip_count, 3),
            other => panic!("expected Open, got {other:?}"),
        }
    }

    #[test]
    fn closed_allows_recovery() {
        assert!(allows_recovery(&BreakerState::Closed, 0));
    }

    #[test]
    fn half_open_allows_recovery() {
        assert!(allows_recovery(
            &BreakerState::HalfOpen {
                probe_attempt: 0,
                previous_trip_count: 0,
            },
            0
        ));
    }

    #[test]
    fn reset_returns_closed() {
        assert!(matches!(reset(), BreakerState::Closed));
    }
}