use serde::{Serialize, Deserialize};
use std::fs::OpenOptions;
use std::io::Write;

const CALIBRATION_LOG_PATH: &str = "calibration_log.jsonl";

/// One row written per evaluated bucket (trade_placed=false) and per executed trade (trade_placed=true).
/// After ~300 resolved entries, use these to build the empirical calibration curve C(p,t).
#[derive(Serialize, Deserialize, Clone)]
pub struct CalibrationEntry {
    pub timestamp: String,           // ISO 8601 UTC
    pub city: String,                // e.g. "chicago"
    pub market_date: String,         // e.g. "2026-03-04"
    pub market_question: String,     // full market question text
    pub bucket_label: String,        // e.g. ">=42°F" or "1°C"
    pub bucket_type: String,         // "cumulative_above", "cumulative_below", "narrow", "exact"
    pub model_probability: f64,      // ensemble probability after all adjustments
    pub market_price: f64,           // best ask or mid price at evaluation time
    pub edge: f64,                   // model_probability - market_price
    pub ensemble_mean: f64,          // mean of ensemble member temperatures
    pub ensemble_std: f64,           // std dev of ensemble member temperatures
    pub ensemble_count: usize,       // number of ensemble members (should be 119)
    pub noaa_temp: Option<f64>,      // NOAA forecast if available (US cities)
    pub edge_cv: f64,                // CV from CV-Kelly (-1.0 if not computed)
    pub trade_placed: bool,          // false = evaluation row, true = execution row
    pub trade_amount_usd: f64,       // USD spent (0.0 for evaluation rows)
    pub token_id: String,            // Polymarket token ID for resolution tracking
    pub resolution: Option<String>,  // "YES" or "NO" — populated by outcome tracker later
}

/// Append a single CalibrationEntry to the JSONL log file.
pub fn log_calibration_entry(entry: &CalibrationEntry) {
    let json = match serde_json::to_string(entry) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Failed to serialize calibration entry: {}", e);
            return;
        }
    };

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(CALIBRATION_LOG_PATH);

    match file {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{}", json) {
                eprintln!("Failed to write calibration entry: {}", e);
            }
        }
        Err(e) => eprintln!("Failed to open calibration log: {}", e),
    }
}
