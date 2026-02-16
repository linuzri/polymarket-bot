use std::collections::HashMap;
use statrs::distribution::{ContinuousCDF, Normal};
use tracing::debug;

use super::CityForecast;

/// Temperature bucket (min inclusive, max exclusive)
/// For "X or higher" buckets, max = f64::INFINITY
/// For "X or lower" buckets, min = f64::NEG_INFINITY
#[derive(Debug, Clone, PartialEq)]
pub struct TempBucket {
    pub min_temp: f64,
    pub max_temp: f64,
    pub label: String,
}

impl TempBucket {
    pub fn new(min_temp: f64, max_temp: f64, label: String) -> Self {
        Self { min_temp, max_temp, label }
    }
}

/// Calculate probability for each temperature bucket given a forecast
///
/// Uses a normal distribution centered on the forecast high with the given std_dev.
/// Returns a HashMap mapping bucket label -> probability (0.0 to 1.0).
pub fn calculate_probabilities(
    forecast: &CityForecast,
    buckets: &[TempBucket],
) -> HashMap<String, f64> {
    let mut probs = HashMap::new();

    let dist = match Normal::new(forecast.high_temp, forecast.std_dev) {
        Ok(d) => d,
        Err(_) => return probs,
    };

    for bucket in buckets {
        let p = if bucket.max_temp.is_infinite() {
            // "X or higher" — P(T >= min)
            1.0 - dist.cdf(bucket.min_temp - 0.5)
        } else if bucket.min_temp.is_infinite() && bucket.min_temp < 0.0 {
            // "X or lower" — P(T <= max)
            dist.cdf(bucket.max_temp + 0.5)
        } else {
            // Range bucket "X-Y" — P(min <= T <= max)
            dist.cdf(bucket.max_temp + 0.5) - dist.cdf(bucket.min_temp - 0.5)
        };

        debug!(
            "  Bucket '{}': P={:.4} (forecast={:.1}, σ={:.1})",
            bucket.label, p, forecast.high_temp, forecast.std_dev
        );

        probs.insert(bucket.label.clone(), p);
    }

    // Normalize probabilities to sum to 1.0
    let total: f64 = probs.values().sum();
    if total > 0.0 && (total - 1.0).abs() > 0.001 {
        for v in probs.values_mut() {
            *v /= total;
        }
    }

    probs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weather::TempUnit;

    #[test]
    fn test_bucket_probabilities() {
        let forecast = CityForecast {
            city: "nyc".to_string(),
            date: "2026-02-17".to_string(),
            high_temp: 40.0,
            unit: TempUnit::Fahrenheit,
            std_dev: 3.0,
        };

        let buckets = vec![
            TempBucket::new(f64::NEG_INFINITY, 37.0, "37°F or lower".to_string()),
            TempBucket::new(38.0, 39.0, "38-39°F".to_string()),
            TempBucket::new(40.0, 41.0, "40-41°F".to_string()),
            TempBucket::new(42.0, 43.0, "42-43°F".to_string()),
            TempBucket::new(44.0, f64::INFINITY, "44°F or higher".to_string()),
        ];

        let probs = calculate_probabilities(&forecast, &buckets);

        // The 40-41°F bucket should have the highest probability
        let p_peak = probs.get("40-41°F").unwrap();
        let p_low = probs.get("37°F or lower").unwrap();
        assert!(p_peak > p_low, "Peak bucket should have higher probability than tail");

        // All probabilities should sum to ~1.0
        let total: f64 = probs.values().sum();
        assert!((total - 1.0).abs() < 0.01, "Probabilities should sum to ~1.0, got {}", total);
    }
}
