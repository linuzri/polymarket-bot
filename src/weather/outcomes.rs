use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn, debug};

use super::strategy::WeatherTrade;
use super::{get_cities, WeatherConfig};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TradeOutcome {
    pub market_question: String,
    pub bucket_label: String,
    pub trade_date: String,
    pub resolution_date: String,
    pub city: String,
    pub predicted_high: f64,
    pub noaa_predicted: Option<f64>,
    pub actual_high: Option<f64>,
    pub forecast_error: Option<f64>,
    pub trade_result: Option<String>,
    pub amount_spent: f64,
    pub amount_returned: Option<f64>,
    pub pnl: Option<f64>,
}

/// Fetch actual observed high temperature for a past date from Open-Meteo archive API
pub async fn fetch_actual_high(
    http: &reqwest::Client,
    lat: f64,
    lon: f64,
    date: &str,
    timezone: &str,
) -> Result<Option<f64>> {
    let url = format!(
        "https://archive-api.open-meteo.com/v1/archive?latitude={:.4}&longitude={:.4}&start_date={}&end_date={}&daily=temperature_2m_max&timezone={}",
        lat, lon, date, date, timezone
    );

    debug!("Fetching actual high for {} from archive API", date);

    let resp: serde_json::Value = http.get(&url).send().await?.json().await?;

    if let Some(daily) = resp.get("daily") {
        if let Some(temps) = daily.get("temperature_2m_max").and_then(|v| v.as_array()) {
            if let Some(temp) = temps.first().and_then(|v| v.as_f64()) {
                return Ok(Some(temp));
            }
        }
    }

    Ok(None)
}

/// Check outcomes for resolved trades — fetch actual temperatures and write to trade_outcomes.jsonl
pub async fn check_outcomes(http: &reqwest::Client, config: &WeatherConfig) {
    let all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => return,
    };

    // Load already-processed outcomes to avoid duplicates
    let existing_keys: std::collections::HashSet<String> = match std::fs::read_to_string("trade_outcomes.jsonl") {
        Ok(data) => data.lines()
            .filter_map(|line| serde_json::from_str::<TradeOutcome>(line).ok())
            .map(|o| format!("{}|{}", o.market_question, o.bucket_label))
            .collect(),
        Err(_) => std::collections::HashSet::new(),
    };

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let cities = get_cities(config);

    for trade in &all_trades {
        if trade.dry_run {
            continue;
        }

        // Skip if already processed
        let key = format!("{}|{}", trade.market_question, trade.bucket_label);
        if existing_keys.contains(&key) {
            continue;
        }

        // Extract resolution date from market slug (e.g. "highest-temperature-in-chicago-on-february-28-2026")
        let resolution_date = match extract_date_from_slug(trade.market_slug.as_deref()) {
            Some(d) => d,
            None => continue,
        };

        // Only check if resolution date has passed
        if resolution_date >= today {
            continue;
        }

        // Find matching city for coordinates
        let city = match cities.iter().find(|c| c.name == trade.city) {
            Some(c) => c,
            None => continue,
        };

        // Fetch actual temperature
        let actual_high = match fetch_actual_high(http, city.lat, city.lon, &resolution_date, &city.timezone).await {
            Ok(Some(temp)) => Some(temp),
            Ok(None) => {
                debug!("No actual temp available yet for {} on {}", trade.city, resolution_date);
                continue;
            }
            Err(e) => {
                warn!("Failed to fetch actual temp for {} on {}: {}", trade.city, resolution_date, e);
                continue;
            }
        };

        let predicted_high = trade.ensemble_mean.or(trade.open_meteo_mean).unwrap_or(0.0);
        let forecast_error = actual_high.map(|a| predicted_high - a);

        let outcome = TradeOutcome {
            market_question: trade.market_question.clone(),
            bucket_label: trade.bucket_label.clone(),
            trade_date: trade.timestamp.clone(),
            resolution_date: resolution_date.clone(),
            city: trade.city.clone(),
            predicted_high,
            noaa_predicted: trade.noaa_temp,
            actual_high,
            forecast_error,
            trade_result: trade.outcome.clone(),
            amount_spent: trade.cost,
            amount_returned: trade.pnl.map(|p| trade.cost + p),
            pnl: trade.pnl,
        };

        info!(
            "OUTCOME: {} {} | predicted={:.1} actual={:.1} error={:.1} | {}",
            trade.city, trade.bucket_label,
            predicted_high,
            actual_high.unwrap_or(0.0),
            forecast_error.unwrap_or(0.0),
            trade.outcome.as_deref().unwrap_or("unknown")
        );

        // Append to JSONL
        if let Ok(json) = serde_json::to_string(&outcome) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true).append(true).open("trade_outcomes.jsonl")
            {
                let _ = writeln!(f, "{}", json);
            }
        }

        // Rate limit
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}

/// Extract date from market slug like "highest-temperature-in-chicago-on-february-28-2026"
fn extract_date_from_slug(slug: Option<&str>) -> Option<String> {
    let slug = slug?;
    // Find "on-MONTH-DAY-YEAR" pattern
    let parts: Vec<&str> = slug.split('-').collect();
    let on_idx = parts.iter().position(|&p| p == "on")?;
    if on_idx + 3 >= parts.len() {
        return None;
    }

    let month_str = parts[on_idx + 1];
    let day_str = parts[on_idx + 2];
    let year_str = parts[on_idx + 3];

    let month = match month_str {
        "january" => 1, "february" => 2, "march" => 3, "april" => 4,
        "may" => 5, "june" => 6, "july" => 7, "august" => 8,
        "september" => 9, "october" => 10, "november" => 11, "december" => 12,
        _ => return None,
    };

    let day: u32 = day_str.parse().ok()?;
    let year: i32 = year_str.parse().ok()?;

    Some(format!("{:04}-{:02}-{:02}", year, month, day))
}
