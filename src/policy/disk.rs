//! Disk monitoring decisions — pure logic.

use crate::model::CleanupLevel;
use crate::state::DiskSample;

/// What to do based on current disk usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskDecision {
    /// Disk usage is fine.
    Ok,
    /// Warning threshold exceeded — run standard cleanup.
    Warning { usage_percent: u32 },
    /// Critical threshold exceeded — run aggressive cleanup.
    Critical { usage_percent: u32 },
}

impl DiskDecision {
    #[must_use]
    pub fn cleanup_level(&self) -> Option<CleanupLevel> {
        match self {
            Self::Ok => None,
            Self::Warning { .. } => Some(CleanupLevel::Standard),
            Self::Critical { .. } => Some(CleanupLevel::Aggressive),
        }
    }
}

/// Evaluates disk usage against thresholds.
#[must_use]
pub fn evaluate(usage_percent: u32, warn: u32, critical: u32) -> DiskDecision {
    if usage_percent >= critical {
        DiskDecision::Critical { usage_percent }
    } else if usage_percent >= warn {
        DiskDecision::Warning { usage_percent }
    } else {
        DiskDecision::Ok
    }
}

/// OLS linear regression for disk usage prediction.
///
/// Returns `(predicted_24h_percent, slope_per_hour, r_squared)`.
/// Returns `None` if insufficient data (< 3 samples).
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn predict(samples: &[DiskSample], horizon_hours: f64) -> Option<DiskPrediction> {
    let n = samples.len();
    if n < 3 {
        return None;
    }

    let first_mono = samples[0].mono as f64;
    let xs: Vec<f64> = samples
        .iter()
        .map(|s| (s.mono as f64 - first_mono) / 3600.0)
        .collect();
    let ys: Vec<f64> = samples.iter().map(|s| f64::from(s.usage_percent)).collect();

    let n_f = n as f64;
    let sum_x: f64 = xs.iter().sum();
    let sum_y: f64 = ys.iter().sum();
    let mean_x = sum_x / n_f;
    let mean_y = sum_y / n_f;

    let mut num = 0.0_f64;
    let mut den = 0.0_f64;
    for i in 0..n {
        let dx = xs[i] - mean_x;
        let dy = ys[i] - mean_y;
        num += dx * dy;
        den += dx * dx;
    }

    if den.abs() < f64::EPSILON {
        return None;
    }

    let slope = num / den;
    let intercept = mean_y - slope * mean_x;

    // R²
    let mut ss_res = 0.0_f64;
    let mut ss_tot = 0.0_f64;
    for i in 0..n {
        let predicted = intercept + slope * xs[i];
        ss_res += (ys[i] - predicted).powi(2);
        ss_tot += (ys[i] - mean_y).powi(2);
    }

    let r_squared = if ss_tot > f64::EPSILON {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };

    let current_hours = xs.last().copied().unwrap_or(0.0);
    let predicted = (intercept + slope * (current_hours + horizon_hours))
        .clamp(0.0, 100.0);

    Some(DiskPrediction {
        predicted_24h_percent: predicted.round() as u32,
        slope_per_hour: (slope * 100.0).round() / 100.0,
        r_squared,
        sample_count: n,
    })
}

/// Result of disk prediction.
#[derive(Debug, Clone)]
pub struct DiskPrediction {
    pub predicted_24h_percent: u32,
    pub slope_per_hour: f64,
    pub r_squared: f64,
    pub sample_count: usize,
}

impl DiskPrediction {
    #[must_use]
    pub fn confidence(&self) -> &'static str {
        if self.sample_count < 3 {
            "insufficient_data"
        } else if self.r_squared < 0.7 {
            "low"
        } else {
            "high"
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ok_below_warning() {
        assert_eq!(evaluate(60, 85, 95), DiskDecision::Ok);
    }

    #[test]
    fn warning_at_threshold() {
        assert_eq!(evaluate(85, 85, 95), DiskDecision::Warning { usage_percent: 85 });
    }

    #[test]
    fn critical_at_threshold() {
        assert_eq!(evaluate(95, 85, 95), DiskDecision::Critical { usage_percent: 95 });
    }

    #[test]
    fn cleanup_levels() {
        assert!(DiskDecision::Ok.cleanup_level().is_none());
        assert_eq!(
            DiskDecision::Warning { usage_percent: 86 }.cleanup_level(),
            Some(CleanupLevel::Standard)
        );
        assert_eq!(
            DiskDecision::Critical { usage_percent: 96 }.cleanup_level(),
            Some(CleanupLevel::Aggressive)
        );
    }

    #[test]
    fn prediction_linear_growth() {
        // Simulate disk growing 1% per 6 hours.
        let samples: Vec<DiskSample> = (0..7)
            .map(|i| DiskSample {
                mono: i * 6 * 3600,
                usage_percent: 70 + i as u32,
                free_bytes: 1_000_000,
            })
            .collect();

        let pred = predict(&samples, 24.0).unwrap();
        // Slope ~= 1/6 per hour ≈ 0.17
        assert!(pred.slope_per_hour > 0.1);
        assert!(pred.slope_per_hour < 0.25);
        // R² should be very high for perfectly linear data.
        assert!(pred.r_squared > 0.99);
        // Prediction should be higher than current.
        assert!(pred.predicted_24h_percent > 76);
        assert_eq!(pred.confidence(), "high");
    }

    #[test]
    fn prediction_insufficient_data() {
        let samples = vec![
            DiskSample { mono: 0, usage_percent: 50, free_bytes: 1_000_000 },
            DiskSample { mono: 3600, usage_percent: 51, free_bytes: 990_000 },
        ];
        assert!(predict(&samples, 24.0).is_none());
    }

    #[test]
    fn prediction_flat() {
        let samples: Vec<DiskSample> = (0..5)
            .map(|i| DiskSample {
                mono: i * 6 * 3600,
                usage_percent: 50,
                free_bytes: 1_000_000,
            })
            .collect();

        let pred = predict(&samples, 24.0).unwrap();
        assert_eq!(pred.predicted_24h_percent, 50);
        assert!(pred.slope_per_hour.abs() < 0.01);
    }
}