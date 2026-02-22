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

/// Check if a temperature falls within a bucket
fn temp_in_bucket(temp: f64, bucket: &TempBucket) -> bool {
    temp >= bucket.min_temp - 0.5 && temp <= bucket.max_temp + 0.5
}

/// Calculate probability for each temperature bucket given a forecast.
///
/// If multi-model temperatures are available, uses consensus:
/// - Counts how many models place forecast in each bucket
/// - Only flags a bucket as tradeable if 3+ of 4 models agree
/// - Falls back to normal distribution if no multi-model data
pub fn calculate_probabilities(
    forecast: &CityForecast,
    buckets: &[TempBucket],
) -> HashMap<String, f64> {
    let mut probs = HashMap::new();

    // If we have multi-model data, use consensus-weighted approach
    if forecast.model_temps.len() >= 3 {
        let n_models = forecast.model_temps.len() as f64;

        // Count models per bucket
        let mut bucket_counts: HashMap<String, usize> = HashMap::new();
        for (_model, &temp) in &forecast.model_temps {
            for bucket in buckets {
                if temp_in_bucket(temp, bucket) {
                    *bucket_counts.entry(bucket.label.clone()).or_insert(0) += 1;
                    break; // each model temp goes in exactly one bucket
                }
            }
        }

        // Find max consensus
        let max_consensus = bucket_counts.values().cloned().max().unwrap_or(0);

        // Use normal distribution centered on mean, but scale by consensus
        let dist = match Normal::new(forecast.high_temp, forecast.std_dev) {
            Ok(d) => d,
            Err(_) => return probs,
        };

        for bucket in buckets {
            let base_p = if bucket.max_temp.is_infinite() {
                1.0 - dist.cdf(bucket.min_temp - 0.5)
            } else if bucket.min_temp.is_infinite() && bucket.min_temp < 0.0 {
                dist.cdf(bucket.max_temp + 0.5)
            } else {
                dist.cdf(bucket.max_temp + 0.5) - dist.cdf(bucket.min_temp - 0.5)
            };

            let count = bucket_counts.get(&bucket.label).cloned().unwrap_or(0);
            let consensus_frac = count as f64 / n_models;

            // If 3+ models agree, boost probability; if <3, dampen heavily
            let adjusted_p = if count >= 3 {
                // Strong consensus: blend base probability with consensus fraction
                // Weight consensus more heavily
                0.3 * base_p + 0.7 * consensus_frac
            } else if max_consensus >= 3 {
                // Another bucket has strong consensus; dampen this one
                base_p * 0.3
            } else {
                // No strong consensus anywhere: low confidence across the board
                base_p * 0.5
            };

            debug!(
                "  Bucket '{}': base_P={:.4} consensus={}/{} adjusted_P={:.4}",
                bucket.label, base_p, count, forecast.model_temps.len(), adjusted_p
            );

            probs.insert(bucket.label.clone(), adjusted_p);
        }

        // Normalize
        let total: f64 = probs.values().sum();
        if total > 0.0 && (total - 1.0).abs() > 0.001 {
            for v in probs.values_mut() {
                *v /= total;
            }
        }

        return probs;
    }

    // Fallback: single-model normal distribution (original behavior)
    let dist = match Normal::new(forecast.high_temp, forecast.std_dev) {
        Ok(d) => d,
        Err(_) => return probs,
    };

    for bucket in buckets {
        let p = if bucket.max_temp.is_infinite() {
            1.0 - dist.cdf(bucket.min_temp - 0.5)
        } else if bucket.min_temp.is_infinite() && bucket.min_temp < 0.0 {
            dist.cdf(bucket.max_temp + 0.5)
        } else {
            dist.cdf(bucket.max_temp + 0.5) - dist.cdf(bucket.min_temp - 0.5)
        };

        debug!(
            "  Bucket '{}': P={:.4} (forecast={:.1}, sigma={:.1})",
            bucket.label, p, forecast.high_temp, forecast.std_dev
        );

        probs.insert(bucket.label.clone(), p);
    }

    // Normalize
    let total: f64 = probs.values().sum();
    if total > 0.0 && (total - 1.0).abs() > 0.001 {
        for v in probs.values_mut() {
            *v /= total;
        }
    }

    probs
}

/// Calculate bucket probabilities from ensemble members (non-parametric).
/// Each member "votes" for a bucket. The fraction of members in each bucket
/// IS the probability estimate. This naturally captures flow-dependent uncertainty.
pub fn calculate_probabilities_ensemble(
    members: &[f64],
    buckets: &[TempBucket],
) -> HashMap<String, f64> {
    let mut probs = HashMap::new();
    let n = members.len() as f64;

    if n == 0.0 {
        return probs;
    }

    for bucket in buckets {
        let count = members.iter()
            .filter(|&&temp| temp >= bucket.min_temp - 0.5 && temp <= bucket.max_temp + 0.5)
            .count();
        let prob = count as f64 / n;
        probs.insert(bucket.label.clone(), prob);

        debug!(
            "  Bucket '{}': {}/{} members = P={:.4}",
            bucket.label, count, members.len(), prob
        );
    }

    // Normalize if members didn't all land in known buckets
    let total: f64 = probs.values().sum();
    if total > 0.0 && (total - 1.0).abs() > 0.01 {
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
            model_temps: HashMap::new(),
        };

        let buckets = vec![
            TempBucket::new(f64::NEG_INFINITY, 37.0, "37F or lower".to_string()),
            TempBucket::new(38.0, 39.0, "38-39F".to_string()),
            TempBucket::new(40.0, 41.0, "40-41F".to_string()),
            TempBucket::new(42.0, 43.0, "42-43F".to_string()),
            TempBucket::new(44.0, f64::INFINITY, "44F or higher".to_string()),
        ];

        let probs = calculate_probabilities(&forecast, &buckets);

        let p_peak = probs.get("40-41F").unwrap();
        let p_low = probs.get("37F or lower").unwrap();
        assert!(p_peak > p_low, "Peak bucket should have higher probability than tail");

        let total: f64 = probs.values().sum();
        assert!((total - 1.0).abs() < 0.01, "Probabilities should sum to ~1.0, got {}", total);
    }

    #[test]
    fn test_multi_model_consensus() {
        let mut model_temps = HashMap::new();
        model_temps.insert("best_match".to_string(), 40.5);
        model_temps.insert("gfs_seamless".to_string(), 41.0);
        model_temps.insert("icon_seamless".to_string(), 40.8);
        model_temps.insert("ecmwf_ifs025".to_string(), 40.2);

        let forecast = CityForecast {
            city: "nyc".to_string(),
            date: "2026-02-17".to_string(),
            high_temp: 40.6,
            unit: TempUnit::Fahrenheit,
            std_dev: 2.5,
            model_temps,
        };

        let buckets = vec![
            TempBucket::new(f64::NEG_INFINITY, 37.0, "37F or lower".to_string()),
            TempBucket::new(38.0, 39.0, "38-39F".to_string()),
            TempBucket::new(40.0, 41.0, "40-41F".to_string()),
            TempBucket::new(42.0, 43.0, "42-43F".to_string()),
            TempBucket::new(44.0, f64::INFINITY, "44F or higher".to_string()),
        ];

        let probs = calculate_probabilities(&forecast, &buckets);

        // All 4 models are in the 40-41F bucket, so it should dominate
        let p_consensus = probs.get("40-41F").unwrap();
        assert!(*p_consensus > 0.5, "Consensus bucket should have >50% probability, got {:.3}", p_consensus);
    }
}
