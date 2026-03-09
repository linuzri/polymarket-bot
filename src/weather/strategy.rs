use anyhow::Result;
use chrono::{Utc, Timelike, Datelike};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn, error, debug};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders;

use super::calibration::{CalibrationEntry, log_calibration_entry};
use super::forecast::{self, TempBucket};
use super::markets::{self, WeatherMarket};
use super::noaa::NoaaClient;
use super::open_meteo::OpenMeteoClient;
use super::position_monitor;
use super::{City, CityForecast, ScanSchedule, TempUnit, WeatherConfig, get_cities};

/// Calculate the next scan time based on model release schedule
fn next_scan_time(schedule: &ScanSchedule) -> chrono::DateTime<Utc> {
    let now = Utc::now();
    let current_hour = now.hour();
    let current_minute = now.minute();
    let target_minute = schedule.post_release_delay_minutes as u32;

    // Find the next model release + delay
    for &hour in &schedule.model_release_hours {
        if hour > current_hour || (hour == current_hour && target_minute > current_minute) {
            if let Some(dt) = now.date_naive()
                .and_hms_opt(hour, target_minute, 0)
            {
                return dt.and_utc();
            }
        }
    }

    // Wrap to first release of next day
    let first_hour = schedule.model_release_hours.first().copied().unwrap_or(3);
    let tomorrow = now.date_naive() + chrono::Duration::days(1);
    tomorrow
        .and_hms_opt(first_hour, target_minute, 0)
        .unwrap()
        .and_utc()
}

/// Trade log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherTrade {
    pub timestamp: String,
    pub market_question: String,
    pub bucket_label: String,
    pub city: String,
    pub our_probability: f64,
    pub market_price: f64,
    pub edge: f64,
    pub side: String,
    pub shares: f64,
    pub price: f64,
    pub cost: f64,
    pub dry_run: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub filled: bool,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub market_slug: Option<String>,
    /// Whether the order was confirmed filled on CLOB
    #[serde(default)]
    pub fill_confirmed: bool,
    /// Outcome: "WIN", "LOSS", "NO_FILL", or None if unresolved
    #[serde(default)]
    pub outcome: Option<String>,
    /// Profit/loss in USDC
    #[serde(default)]
    pub pnl: Option<f64>,
    /// Actual high temperature from Weather Underground resolution
    #[serde(default)]
    pub resolution_temp: Option<f64>,
    /// Token ID for CLOB fill checking
    #[serde(default)]
    pub token_id: Option<String>,
    /// Open-Meteo multi-model mean temperature (excluding NOAA)
    #[serde(default)]
    pub open_meteo_mean: Option<f64>,
    /// NOAA temperature for the same date (US cities only)
    #[serde(default)]
    pub noaa_temp: Option<f64>,
    /// Number of ensemble members used for probability calculation
    #[serde(default)]
    pub ensemble_member_count: usize,
    /// Ensemble temperature statistics (min, max, mean)
    #[serde(default)]
    pub ensemble_min: Option<f64>,
    #[serde(default)]
    pub ensemble_max: Option<f64>,
    #[serde(default)]
    pub ensemble_mean: Option<f64>,
    /// Absolute difference between Open-Meteo mean and NOAA forecast
    #[serde(default)]
    pub model_disagreement: Option<f64>,
    /// Fill status: "filled", "partial", "unfilled", "cancelled"
    #[serde(default)]
    pub fill_status: Option<String>,
    /// When fill status was last checked
    #[serde(default)]
    pub fill_checked_at: Option<String>,
    /// Probability calculation method: "ensemble", "consensus", or "normal_dist"
    #[serde(default)]
    pub probability_source: Option<String>,
    /// CTF condition ID for on-chain redemption
    #[serde(default)]
    pub condition_id: Option<String>,
    /// Whether position has been redeemed on-chain (USDC claimed)
    #[serde(default)]
    pub redeemed: bool,
    /// Whether the market uses the neg-risk exchange contract
    #[serde(default)]
    pub neg_risk: bool,
    /// Whether this position was auto-exited by the position monitor
    #[serde(default)]
    pub auto_exited: bool,
    /// Market resolution date (YYYY-MM-DD) for position monitor exit timing
    #[serde(default)]
    pub market_date: Option<String>,
}

/// Scan cycle summary for logging â€” tracks what was evaluated, skipped, and why
#[derive(Serialize, Deserialize, Debug)]
struct ScanSummary {
    timestamp: String,
    markets_discovered: usize,
    markets_evaluated: usize,
    markets_high_disagreement: usize,
    markets_skipped_no_forecast: usize,
    buckets_evaluated: usize,
    buckets_skipped_low_price: usize,
    buckets_skipped_dedup: usize,
    buckets_skipped_narrow: usize,
    buckets_skipped_no_edge: usize,
    buckets_skipped_extreme_edge: usize,
    buckets_skipped_buffer: usize,
    buckets_skipped_low_prob: usize,
    trades_attempted: usize,
    trades_placed: usize,
    ladder_trades_placed: usize,
    total_usd_deployed: f64,
    existing_exposure: f64,
}

// ===== Strategy helper functions =====

/// Task 4: Clamp probability to [0.02, 0.95] before edge calculation.
fn clamp_probability(p: f64) -> f64 {
    p.max(0.02).min(0.95)
}


/// CV-adjusted Kelly: compute coefficient of variation of edge across ensemble bootstrap groups.
/// CV = std_dev_edge / mean_edge. Returns value in [0, 1] clamped for safety.
///
/// Low CV (e.g. 0.1) = ensemble strongly agrees -> bet with confidence
/// High CV (e.g. 0.8) = ensemble disagrees -> reduce bet size
fn compute_edge_cv(
    ensemble_temps: &[f64],
    bucket_min: f64,
    bucket_max: f64,
    market_price: f64,
) -> (f64, usize) {
    if ensemble_temps.len() < 10 {
        return (0.5, 0); // Not enough members - moderate uncertainty
    }

    // Cumulative buckets: min=-INF (or lower) / max=+INF (or higher)
    let is_cumulative_above = bucket_max.is_infinite() && bucket_max > 0.0;
    let is_cumulative_below = bucket_min.is_infinite() && bucket_min < 0.0;

    // Bootstrap: split ensemble into groups of ~10, compute edge per group
    let chunk_size = 10;
    let mut group_edges: Vec<f64> = Vec::new();

    for chunk in ensemble_temps.chunks(chunk_size) {
        let count_in_bucket = chunk.iter().filter(|&&t| {
            if is_cumulative_above {
                t >= bucket_min
            } else if is_cumulative_below {
                t <= bucket_max
            } else {
                t >= bucket_min && t < bucket_max
            }
        }).count();

        let group_prob = count_in_bucket as f64 / chunk.len() as f64;
        group_edges.push(group_prob - market_price);
    }

    let group_count = group_edges.len();

    if group_edges.is_empty() {
        return (0.5, 0);
    }

    let mean_edge: f64 = group_edges.iter().sum::<f64>() / group_edges.len() as f64;

    if mean_edge.abs() < 0.01 {
        return (1.0, group_count); // Near-zero mean edge - maximally uncertain
    }

    let variance: f64 = group_edges.iter()
        .map(|e| (e - mean_edge).powi(2))
        .sum::<f64>() / group_edges.len() as f64;

    ((variance.sqrt() / mean_edge.abs()).clamp(0.0, 1.0), group_count)
}

/// Task 2: Cross-validate ensemble probability against NOAA forecast.
/// Penalises high ensemble confidence when NOAA strongly disagrees;
/// boosts very low probability when NOAA supports the bucket.
fn cross_validate_with_noaa(
    ensemble_prob: f64,
    noaa_temp: Option<f64>,
    bucket: &TempBucket,
    unit: TempUnit,
) -> f64 {
    let Some(noaa) = noaa_temp else {
        return ensemble_prob;
    };

    let tolerance = match unit {
        TempUnit::Fahrenheit => 3.0_f64,
        TempUnit::Celsius    => 2.0_f64,
    };

    // Is NOAA temperature near (within tolerance of) our bucket?
    let noaa_near_bucket = if bucket.min_temp.is_finite() && bucket.max_temp.is_finite() {
        noaa >= bucket.min_temp - tolerance && noaa <= bucket.max_temp + tolerance
    } else if bucket.max_temp.is_finite() {
        noaa <= bucket.max_temp + tolerance
    } else if bucket.min_temp.is_finite() {
        noaa >= bucket.min_temp - tolerance
    } else {
        true
    };

    // Ensemble very high but NOAA disagrees: apply distance-based penalty
    if ensemble_prob > 0.80 && !noaa_near_bucket {
        let distance = if bucket.min_temp.is_finite() && bucket.max_temp.is_finite() {
            let center = (bucket.min_temp + bucket.max_temp) / 2.0;
            (noaa - center).abs()
        } else if bucket.max_temp.is_finite() {
            (noaa - bucket.max_temp).abs()
        } else if bucket.min_temp.is_finite() {
            (noaa - bucket.min_temp).abs()
        } else {
            0.0
        };
        // 5% penalty per unit of distance, capped at 25%
        let penalty = (distance / tolerance * 0.05).min(0.25);
        return (ensemble_prob - penalty).max(0.60);
    }

    // Ensemble very low but NOAA supports the bucket: slight boost
    if ensemble_prob < 0.20 && noaa_near_bucket {
        return (ensemble_prob + 0.10).min(0.40);
    }

    ensemble_prob
}

/// Task 3: Returns true if Open-Meteo and NOAA disagree by more than the
/// threshold (8 F / 4.5 C), indicating the market should be skipped.
fn models_disagree_too_much(forecast: &CityForecast, unit: TempUnit) -> bool {
    let Some(&noaa_temp) = forecast.model_temps.get("noaa") else {
        return false;
    };

    let om_temps: Vec<f64> = forecast.model_temps.iter()
        .filter(|(k, _)| k.as_str() != "noaa")
        .map(|(_, &v)| v)
        .collect();

    if om_temps.is_empty() {
        return false;
    }

    let om_mean = om_temps.iter().sum::<f64>() / om_temps.len() as f64;
    let diff = (om_mean - noaa_temp).abs();

    let threshold = match unit {
        TempUnit::Fahrenheit => 8.0_f64,
        TempUnit::Celsius    => 4.5_f64,
    };

    diff > threshold
}

/// Task 6: Returns true when a narrow bucket lacks sufficient ensemble support.
/// Narrow = range <= 5 F or 3 C; requires at least 5 ensemble members to trade.
fn narrow_bucket_insufficient_ensemble(
    bucket: &TempBucket,
    unit: TempUnit,
    ensemble_count: usize,
) -> bool {
    if !bucket.min_temp.is_finite() || !bucket.max_temp.is_finite() {
        return false;
    }
    let range = bucket.max_temp - bucket.min_temp;
    let narrow_threshold = match unit {
        TempUnit::Fahrenheit => 5.0_f64,
        TempUnit::Celsius    => 3.0_f64,
    };
    range <= narrow_threshold && ensemble_count < 5
}


/// v7 Task 2: Southern Hemisphere seasonal warm bias correction.
fn southern_hemisphere_seasonal_correction(
    southern_hemisphere: bool,
    month: u32,
    betting_cool: bool,
) -> f64 {
    if !southern_hemisphere {
        return 1.0;
    }
    let is_sh_summer = matches!(month, 12 | 1 | 2 | 3);
    if is_sh_summer && betting_cool {
        0.75
    } else {
        1.0
    }
}

/// Weather strategy runner
pub struct WeatherStrategy {
    config: WeatherConfig,
    noaa: NoaaClient,
    open_meteo: OpenMeteoClient,
    notifier: TelegramNotifier,
    http: reqwest::Client,
    dry_run: bool,
    total_exposure: f64,
    trades: Vec<WeatherTrade>,
    placed_this_session: HashSet<String>,
}

impl WeatherStrategy {
    pub fn new(config: WeatherConfig, dry_run: bool) -> Self {
        // Load existing unresolved exposure from trade log
        let existing_exposure = Self::load_existing_exposure();
        if existing_exposure > 0.0 {
            info!("Loaded existing weather exposure: ${:.2}", existing_exposure);
        }

        // Load already-traded position keys to prevent duplicate entries
        let existing_keys = Self::load_open_position_keys();
        if !existing_keys.is_empty() {
            info!("Loaded {} existing position keys (dedup)", existing_keys.len());
        }

        let open_meteo = OpenMeteoClient::new(config.open_meteo_bias_f, config.open_meteo_bias_c);
        Self {
            config,
            noaa: NoaaClient::new(),
            open_meteo,
            notifier: TelegramNotifier::new(),
            http: reqwest::Client::builder()
                .user_agent("polymarket-weather-bot/1.0")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
            dry_run,
            total_exposure: existing_exposure,
            trades: Vec::new(),
            placed_this_session: existing_keys,
        }
    }

    /// Load existing unresolved exposure from strategy_trades.json
    /// Only counts non-dry-run trades from today or future dates (not yet resolved)
    fn load_existing_exposure() -> f64 {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return 0.0,
        };

        trades.iter()
            .filter(|t| !t.dry_run)
            .filter(|t| !t.resolved)
            .filter(|t| !t.redeemed) // Exclude redeemed positions â€” capital already reclaimed
            .filter(|t| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                    let days_ago = (Utc::now() - ts.with_timezone(&Utc)).num_days();
                    days_ago <= 4 // weather markets can be up to 2 days out; 4-day window is safe
                } else {
                    false
                }
            })
            .map(|t| t.cost)
            .sum::<f64>()
            .max(0.0) // Prevent -0.0 from floating-point rounding
    }

    /// Load position keys from strategy_trades.json to prevent duplicate entries
    fn load_open_position_keys() -> HashSet<String> {
        let trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return HashSet::new(),
        };

        trades.iter()
            .filter(|t| !t.dry_run)
            .filter(|t| !t.resolved)
            .filter(|t| !t.redeemed) // Exclude redeemed positions â€” market slot freed
            .filter(|t| {
                // Only consider trades from last 4 days (weather markets are 1-2 days out)
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                    let days_ago = (Utc::now() - ts.with_timezone(&Utc)).num_days();
                    days_ago <= 4
                } else {
                    false
                }
            })
            .map(|t| format!("{}|{}", t.market_question, t.bucket_label))
            .collect()
    }

    /// Check strategy_trades.json for open positions and mark any that have resolved.
    /// A position is resolved if Polymarket's Gamma API shows the market as closed.
    /// Also marks stale unfilled orders (>24h) as resolved to free exposure.
    async fn check_and_mark_resolved(&mut self) {
        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let mut changed = false;

        for trade in all_trades.iter_mut() {
            if trade.resolved || trade.dry_run {
                continue;
            }

            // Check if market is closed via Gamma API using slug (reliable)
            // Fall back to question substring matching if no slug available
            let resolved = if let Some(ref slug) = trade.market_slug {
                let url = format!(
                    "https://gamma-api.polymarket.com/events?slug={}&closed=true",
                    slug
                );
                match self.http.get(&url).send().await {
                    Ok(resp) => {
                        match resp.json::<Vec<serde_json::Value>>().await {
                            Ok(events) => !events.is_empty(),
                            Err(_) => false,
                        }
                    }
                    Err(_) => false,
                }
            } else {
                // Legacy fallback: substring match on question text
                let search_term = if trade.market_question.len() > 30 {
                    &trade.market_question[..30]
                } else {
                    &trade.market_question
                };
                let url = format!(
                    "https://gamma-api.polymarket.com/markets?closed=true&limit=1&question={}",
                    search_term.replace(' ', "%20").replace('?', "%3F")
                );
                match self.http.get(&url).send().await {
                    Ok(resp) => {
                        match resp.text().await {
                            Ok(text) => {
                                let check_len = 50.min(trade.market_question.len());
                                text.contains(&trade.market_question[..check_len]) && text.len() > 10
                            }
                            Err(_) => false,
                        }
                    }
                    Err(_) => false,
                }
            };

            if resolved {
                trade.resolved = true;
                self.total_exposure -= trade.cost;

                // Check if order actually filled on CLOB
                let fill_confirmed = if let Some(ref token_id) = trade.token_id {
                    let maker_addr = "0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853";
                    let url = format!(
                        "https://clob.polymarket.com/trades?maker={}&market={}",
                        maker_addr, token_id
                    );
                    match self.http.get(&url).send().await {
                        Ok(resp) => {
                            match resp.json::<Vec<serde_json::Value>>().await {
                                Ok(trades_list) => !trades_list.is_empty(),
                                Err(_) => false,
                            }
                        }
                        Err(_) => false,
                    }
                } else {
                    false
                };
                trade.fill_confirmed = fill_confirmed;

                // Determine outcome
                if !fill_confirmed {
                    trade.outcome = Some("NO_FILL".to_string());
                    trade.pnl = Some(0.0);
                    info!("NO_FILL (order never matched): {} | {}", trade.market_question, trade.bucket_label);
                } else {
                    // Check if our bucket won by looking at outcomePrices from Gamma API
                    let won = if let Some(ref slug) = trade.market_slug {
                        self.check_bucket_won(slug, &trade.bucket_label).await
                    } else {
                        None
                    };

                    match won {
                        Some(true) => {
                            let pnl = trade.shares - trade.cost;
                            trade.outcome = Some("WIN".to_string());
                            trade.pnl = Some(pnl);
                            info!("WIN +${:.2}: {} | {}", pnl, trade.market_question, trade.bucket_label);
                        }
                        Some(false) => {
                            trade.outcome = Some("LOSS".to_string());
                            trade.pnl = Some(-trade.cost);
                            info!("LOSS -${:.2}: {} | {}", trade.cost, trade.market_question, trade.bucket_label);
                        }
                        None => {
                            // Could not determine outcome â€” mark resolved but unknown
                            trade.outcome = Some("UNKNOWN".to_string());
                            trade.pnl = None;
                            warn!("RESOLVED but outcome unknown: {} | {}", trade.market_question, trade.bucket_label);
                        }
                    }
                }

                // Try to fetch resolution temperature from Weather Underground
                if let Some(ref slug) = trade.market_slug {
                    if let Some(temp) = self.fetch_resolution_temp(slug, &trade.city).await {
                        trade.resolution_temp = Some(temp);
                    }
                }

                info!("Freed ${:.2} exposure (resolved): {} | {}", trade.cost, trade.market_question, trade.bucket_label);
                changed = true;
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if changed {
            if let Ok(json) = serde_json::to_string_pretty(&all_trades) {
                let _ = std::fs::write("strategy_trades.json", json);
            }
        }
    }

    /// Reconcile trade log with actual wallet holdings.
    /// If the proxy wallet no longer holds shares for a trade, mark it as resolved.
    /// This handles manual sells, redeems, and any other external position changes.
    async fn reconcile_with_wallet(&mut self) {
        let proxy_wallet = "0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853";
        let url = format!(
            "https://data-api.polymarket.com/positions?user={}&sizeThreshold=0.1",
            proxy_wallet
        );

        // Fetch current wallet positions
        let held_assets: HashSet<String> = match self.http.get(&url).send().await {
            Ok(resp) => {
                match resp.json::<Vec<serde_json::Value>>().await {
                    Ok(positions) => {
                        positions.iter()
                            .filter_map(|p| p.get("asset").and_then(|a| a.as_str()).map(|s| s.to_string()))
                            .collect()
                    }
                    Err(e) => {
                        warn!("Failed to parse wallet positions: {}", e);
                        return;
                    }
                }
            }
            Err(e) => {
                warn!("Failed to fetch wallet positions: {}", e);
                return;
            }
        };

        info!("Wallet reconciliation: {} active positions on-chain", held_assets.len());

        // Load trade log
        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let mut changed = false;
        for trade in all_trades.iter_mut() {
            if trade.resolved || trade.dry_run {
                continue;
            }

            // If trade has a token_id, check if wallet still holds it
            if let Some(ref token_id) = trade.token_id {
                if !held_assets.contains(token_id) {
                    trade.resolved = true;
                    trade.outcome = Some("MANUAL_CLOSE".to_string());
                    self.total_exposure -= trade.cost;
                    if self.total_exposure < 0.0 {
                        self.total_exposure = 0.0;
                    }
                    // Remove from dedup set so we can re-enter if needed
                    let dedup_key = format!("{}|{}", trade.market_question, trade.bucket_label);
                    self.placed_this_session.remove(&dedup_key);
                    info!("MANUAL_CLOSE (no longer in wallet): {} | {} | freed ${:.2}",
                        trade.market_question, trade.bucket_label, trade.cost);
                    changed = true;
                }
            }
        }

        if changed {
            if let Ok(json) = serde_json::to_string_pretty(&all_trades) {
                let _ = std::fs::write("strategy_trades.json", json);
            }
            info!("Wallet reconciliation: exposure now ${:.2}", self.total_exposure);
        }
    }

    /// Check if our specific bucket won by querying Gamma API for outcome prices
    async fn check_bucket_won(&self, slug: &str, bucket_label: &str) -> Option<bool> {
        // Get all markets for this event
        let url = format!(
            "https://gamma-api.polymarket.com/markets?slug={}&closed=true",
            slug
        );
        let resp = self.http.get(&url).send().await.ok()?;
        let markets: Vec<serde_json::Value> = resp.json().await.ok()?;

        for market in &markets {
            let outcomes = market.get("outcomes")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");
            let outcome_prices = market.get("outcomePrices")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");

            let outcomes: Vec<String> = serde_json::from_str(outcomes).unwrap_or_default();
            let prices: Vec<String> = serde_json::from_str(outcome_prices).unwrap_or_default();

            // Find our bucket in outcomes
            for (i, outcome) in outcomes.iter().enumerate() {
                if outcome == bucket_label {
                    if let Some(price_str) = prices.get(i) {
                        if let Ok(price) = price_str.parse::<f64>() {
                            // Price = 1.0 means this outcome won, 0.0 means it lost
                            return Some(price > 0.5);
                        }
                    }
                }
            }
        }
        None
    }

    /// Fetch the actual resolution temperature from Weather Underground
    async fn fetch_resolution_temp(&self, slug: &str, city: &str) -> Option<f64> {
        // Extract date from slug (e.g. "highest-temperature-in-seoul-on-february-23-2026")
        let cities_config = get_cities(&self.config);
        let city_obj = cities_config.iter().find(|c| c.name == city)?;
        let station = city_obj.wunderground_station.as_deref()?;

        // Parse date from slug
        let date = self.parse_date_from_slug(slug)?;

        let url = format!(
            "https://api.weather.com/v2/pws/history/daily?stationId={}&format=json&units=e&date={}",
            station, date.replace("-", "")
        );
        // Weather Underground API requires an API key; skip if not available
        // For now, return None â€” can be enhanced later with WU API key
        debug!("Resolution temp lookup skipped (no WU API key): {} {}", station, date);
        None
    }

    /// Parse a date string from a market slug
    fn parse_date_from_slug(&self, slug: &str) -> Option<String> {
        // Pattern: "highest-temperature-in-CITY-on-MONTH-DAY-YEAR"
        let months = [
            ("january", "01"), ("february", "02"), ("march", "03"), ("april", "04"),
            ("may", "05"), ("june", "06"), ("july", "07"), ("august", "08"),
            ("september", "09"), ("october", "10"), ("november", "11"), ("december", "12"),
        ];
        for (name, num) in &months {
            if let Some(pos) = slug.find(name) {
                let after = &slug[pos + name.len()..];
                let parts: Vec<&str> = after.split('-').filter(|s| !s.is_empty()).collect();
                if parts.len() >= 2 {
                    let day = parts[0].parse::<u32>().ok()?;
                    let year = parts[1].parse::<u32>().ok()?;
                    return Some(format!("{}-{}-{:02}", year, num, day));
                }
            }
        }
        None
    }

    /// Weekly P&L summary â€” runs Sunday midnight UTC
    async fn weekly_summary(&self) {
        let all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let now = Utc::now();
        let week_ago = now - chrono::Duration::days(7);
        let week_start = week_ago.format("%b %d").to_string();
        let week_end = now.format("%b %d").to_string();

        // All trades from this week (not just resolved)
        let recent_all: Vec<&WeatherTrade> = all_trades.iter()
            .filter(|t| !t.dry_run)
            .filter(|t| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                    ts.with_timezone(&Utc) >= week_ago
                } else {
                    false
                }
            })
            .collect();

        let recent_resolved: Vec<&&WeatherTrade> = recent_all.iter()
            .filter(|t| t.resolved)
            .collect();

        let total_placed = recent_all.len();
        let filled: Vec<&&WeatherTrade> = recent_all.iter()
            .filter(|t| t.fill_status.as_deref() == Some("filled") || t.fill_confirmed)
            .collect();
        let unfilled: Vec<&&WeatherTrade> = recent_all.iter()
            .filter(|t| t.fill_status.as_deref() == Some("unfilled"))
            .collect();

        let wins: Vec<&&WeatherTrade> = recent_resolved.iter()
            .filter(|t| t.outcome.as_deref() == Some("WIN"))
            .copied().collect();
        let losses: Vec<&&WeatherTrade> = recent_resolved.iter()
            .filter(|t| t.outcome.as_deref() == Some("LOSS"))
            .copied().collect();
        let no_fills: Vec<&&WeatherTrade> = recent_resolved.iter()
            .filter(|t| t.outcome.as_deref() == Some("NO_FILL"))
            .copied().collect();
        let pending = total_placed - recent_resolved.len();

        let total_pnl: f64 = recent_resolved.iter()
            .filter_map(|t| t.pnl)
            .sum();

        let win_rate = if wins.len() + losses.len() > 0 {
            (wins.len() as f64 / (wins.len() + losses.len()) as f64) * 100.0
        } else {
            0.0
        };

        let mut msg = format!(
            "WEEKLY PERFORMANCE ({} - {})\n\nTrades: {} placed, {} filled, {} unfilled\nResults: {} wins, {} losses, {} no-fill, {} pending\nP&L: {:+.2}\nWin Rate: {:.0}%",
            week_start, week_end,
            total_placed, filled.len(), unfilled.len(),
            wins.len(), losses.len(), no_fills.len(), pending,
            total_pnl, win_rate
        );

        // Forecast accuracy from outcomes file
        let outcomes: Vec<super::outcomes::TradeOutcome> = match std::fs::read_to_string("trade_outcomes.jsonl") {
            Ok(data) => data.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            Err(_) => Vec::new(),
        };

        let recent_outcomes: Vec<&super::outcomes::TradeOutcome> = outcomes.iter()
            .filter(|o| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&o.trade_date) {
                    ts.with_timezone(&Utc) >= week_ago
                } else {
                    false
                }
            })
            .collect();

        if !recent_outcomes.is_empty() {
            let errors: Vec<f64> = recent_outcomes.iter()
                .filter_map(|o| o.forecast_error.map(|e| e.abs()))
                .collect();
            if !errors.is_empty() {
                let mae = errors.iter().sum::<f64>() / errors.len() as f64;
                msg.push_str(&format!("\n\nForecast Accuracy:\n  MAE: {:.1}F ({} trades)", mae, errors.len()));
            }
        }

        // Guardrail stats from scan log
        let scan_summaries: Vec<ScanSummary> = match std::fs::read_to_string("scan_log.jsonl") {
            Ok(data) => data.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect(),
            Err(_) => Vec::new(),
        };

        let recent_scans: Vec<&ScanSummary> = scan_summaries.iter()
            .filter(|s| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&s.timestamp) {
                    ts.with_timezone(&Utc) >= week_ago
                } else {
                    false
                }
            })
            .collect();

        if !recent_scans.is_empty() {
            let total_disagreement: usize = recent_scans.iter().map(|s| s.markets_high_disagreement).sum();
            let total_narrow: usize = recent_scans.iter().map(|s| s.buckets_skipped_narrow).sum();
            let total_extreme: usize = recent_scans.iter().map(|s| s.buckets_skipped_extreme_edge).sum();
            let total_low_prob: usize = recent_scans.iter().map(|s| s.buckets_skipped_low_prob).sum();

            msg.push_str(&format!(
                "\n\nGuardrails ({} scans):\n  High model disagreement: {} flagged (not skipped)\n  Narrow bucket: {} skipped\n  Extreme edge: {} flagged\n  Low probability: {} skipped",
                recent_scans.len(), total_disagreement, total_narrow, total_extreme, total_low_prob
            ));
        }

        // Edge distribution
        let edge_15_20: Vec<&&WeatherTrade> = recent_all.iter().filter(|t| t.edge >= 0.15 && t.edge < 0.20).collect();
        let edge_20_30: Vec<&&WeatherTrade> = recent_all.iter().filter(|t| t.edge >= 0.20 && t.edge < 0.30).collect();
        let edge_30_plus: Vec<&&WeatherTrade> = recent_all.iter().filter(|t| t.edge >= 0.30).collect();

        if !recent_all.is_empty() {
            let wins_in = |trades: &[&&WeatherTrade]| -> usize {
                trades.iter().filter(|t| t.outcome.as_deref() == Some("WIN")).count()
            };
            msg.push_str(&format!(
                "\n\nEdge Distribution:\n  15-20%%: {} trades ({} wins)\n  20-30%%: {} trades ({} wins)\n  30%%+: {} trades ({} wins)",
                edge_15_20.len(), wins_in(&edge_15_20),
                edge_20_30.len(), wins_in(&edge_20_30),
                edge_30_plus.len(), wins_in(&edge_30_plus)
            ));
        }

        // Top trades
        let best = recent_resolved.iter()
            .filter(|t| t.pnl.is_some())
            .max_by(|a, b| a.pnl.unwrap_or(0.0).partial_cmp(&b.pnl.unwrap_or(0.0)).unwrap());
        let worst = recent_resolved.iter()
            .filter(|t| t.pnl.is_some())
            .min_by(|a, b| a.pnl.unwrap_or(0.0).partial_cmp(&b.pnl.unwrap_or(0.0)).unwrap());

        if best.is_some() || worst.is_some() {
            msg.push_str("\n\nTop trades:");
            if let Some(b) = best {
                msg.push_str(&format!("\n  {:+.2} {} {}", b.pnl.unwrap_or(0.0), b.city, b.bucket_label));
            }
            if let Some(w) = worst {
                msg.push_str(&format!("\n  {:+.2} {} {}", w.pnl.unwrap_or(0.0), w.city, w.bucket_label));
            }
        }

        info!("{}", msg);
        self.notifier.send(&msg).await;
    }

    /// Run a single scan cycle
    pub async fn run_once(&mut self) -> Result<u32> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };

        // Check and mark any resolved positions before scanning for new ones
        self.check_and_mark_resolved().await;

        // Reconcile trade log with actual wallet holdings (detects manual sells)
        self.reconcile_with_wallet().await;

        // Check fill status for pending orders
        self.check_fill_status().await;

        // Reconcile with Polymarket activity API (ground truth for fills/P&L)
        self.reconcile_with_api().await;

        // Check forecast outcomes for resolved trades
        super::outcomes::check_outcomes(&self.http, &self.config).await;

        info!("Weather strategy scan starting ({})", mode);

        // Scan summary counters (Task 1)
        let mut scan = ScanSummary {
            timestamp: Utc::now().to_rfc3339(),
            markets_discovered: 0,
            markets_evaluated: 0,
            markets_high_disagreement: 0,
            markets_skipped_no_forecast: 0,
            buckets_evaluated: 0,
            buckets_skipped_low_price: 0,
            buckets_skipped_dedup: 0,
            buckets_skipped_narrow: 0,
            buckets_skipped_no_edge: 0,
            buckets_skipped_extreme_edge: 0,
            buckets_skipped_buffer: 0,
            buckets_skipped_low_prob: 0,
            trades_attempted: 0,
            trades_placed: 0,
            ladder_trades_placed: 0,
            total_usd_deployed: 0.0,
            existing_exposure: self.total_exposure,
        };

        // Step 1: Discover weather markets
        let weather_markets = markets::discover_weather_markets(&self.http).await?;
        scan.markets_discovered = weather_markets.len();
        if weather_markets.is_empty() {
            let alert = "DISCOVERY FAILURE: Zero weather markets found this scan cycle. \
                         Polymarket slug format may have changed or API timed out. Manual check required.";
            warn!("{}", alert);
            self.notifier.send(alert).await;
            self.write_scan_summary(&scan);
            return Ok(0);
        }
        info!("Found {} weather markets", weather_markets.len());

        // Step 2: Fetch forecasts for relevant cities
        let cities = get_cities(&self.config);
        let forecasts = self.fetch_all_forecasts(&cities).await;
        if forecasts.is_empty() {
            warn!("No forecasts fetched â€” skipping weather strategy");
            return Ok(0);
        }
        info!("Fetched forecasts for {} cities", forecasts.len());

        // Step 3: Match markets to forecasts and find edges
        let mut trades_placed = 0u32;
        let client = PolymarketClient::new()?;

        // v7 Task 3: Check open positions for price deterioration and auto-exit
        if let Err(e) = position_monitor::check_and_exit_deteriorated_positions(
            &client,
            &self.notifier,
            self.dry_run,
        ).await {
            warn!("Position monitor error: {}", e);
        }


        // Pre-load open position keys ONCE (was loading per-bucket â€” 224 file reads/scan)
        let file_position_keys = Self::load_open_position_keys();

        for market in &weather_markets {
            if self.total_exposure >= self.config.max_total_exposure {
                info!("Total weather exposure limit reached (${:.2})", self.total_exposure);
                break;
            }

            // Find matching forecast
            let forecast = match self.find_matching_forecast(market, &forecasts) {
                Some(f) => f.clone(),
                None => {
                    let market_city = market.city.as_deref().unwrap_or("unknown");
                    let market_date = market.date.as_deref().unwrap_or("unknown");
                    let available: Vec<String> = forecasts.iter()
                        .filter(|f| f.city.to_lowercase() == market_city.to_lowercase())
                        .map(|f| f.date.clone())
                        .collect();
                    warn!(
                        "NO FORECAST for market: {} | city={} date={} | Available forecast dates: {:?}",
                        market.question, market_city, market_date, available
                    );
                    scan.markets_skipped_no_forecast += 1;
                    continue;
                }
            };
            scan.markets_evaluated += 1;

            // For same-day markets: fetch current observation as a sanity check (Task 4)
            let mut adjusted_forecast = forecast.clone();
            let market_date = market.date.as_deref().unwrap_or("");
            let today = Utc::now().format("%Y-%m-%d").to_string();
            if market_date == today {
                if let Some(city_name) = market.city.as_deref() {
                    let city_obj = cities.iter().find(|c| c.name == city_name);
                    if let Some(city) = city_obj {
                        match super::observations::fetch_current_temp(&self.http, city).await {
                            Ok(Some((current_temp, _obs_time))) => {
                                if current_temp > adjusted_forecast.high_temp {
                                    info!(
                                        "OBSERVATION ADJUSTMENT: {} current {:.1} > forecast high {:.1}",
                                        city_name, current_temp, adjusted_forecast.high_temp
                                    );
                                    adjusted_forecast.high_temp = current_temp;
                                    adjusted_forecast.std_dev *= 0.6; // Tighter uncertainty on same-day
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                debug!("Failed to fetch observation for {}: {}", city_name, e);
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                }
            }

            // Log resolution station if available (Task 5)
            if let Some(city_name) = market.city.as_deref() {
                if let Some(city) = cities.iter().find(|c| c.name == city_name) {
                    if let Some(station) = &city.wunderground_station {
                        info!("  Resolution station: {} (Weather Underground)", station);
                    }
                }
            }

            // Calculate probabilities â€” prefer ensemble when available (Task 2)
            let buckets_vec: Vec<_> = market.buckets.iter().map(|b| b.temp_bucket.clone()).collect();
            let probs = if let Some(ref members) = adjusted_forecast.ensemble_members {
                if members.len() >= 20 {
                    info!("Using {} ensemble members for probability calculation", members.len());
                    forecast::calculate_probabilities_ensemble(members, &buckets_vec)
                } else {
                    forecast::calculate_probabilities(&adjusted_forecast, &buckets_vec)
                }
            } else {
                forecast::calculate_probabilities(&adjusted_forecast, &buckets_vec)
            };

            // NOAA cross-validation still adjusts probabilities (v4 Task 2) â€” keep that.
            // Hard market skip removed: CV-adjusted Kelly handles disagreement via smaller position size.
            if models_disagree_too_much(&adjusted_forecast, market.unit) {
                let noaa_val = adjusted_forecast.model_temps.get("noaa").copied().unwrap_or(0.0);
                let om_vals: Vec<f64> = adjusted_forecast.model_temps.iter()
                    .filter(|(k, _)| k.as_str() != "noaa")
                    .map(|(_, &v)| v).collect();
                let om_mean_val = if !om_vals.is_empty() {
                    om_vals.iter().sum::<f64>() / om_vals.len() as f64
                } else { 0.0 };
                warn!(
                    "High model disagreement: OM={:.1} vs NOAA={:.1} (diff={:.1}{}). CV-adjusted Kelly will reduce sizing.",
                    om_mean_val, noaa_val,
                    (om_mean_val - noaa_val).abs(),
                    if market.unit == TempUnit::Fahrenheit { "F" } else { "C" }
                );
                scan.markets_high_disagreement += 1;
                // No continue â€” CV-adjusted Kelly handles disagreement via smaller position size
            }

            // Pre-compute diagnostic values for trade logging (Task 7)
            let noaa_temp_diag = adjusted_forecast.model_temps.get("noaa").copied();
            let om_temps_diag: Vec<f64> = adjusted_forecast.model_temps.iter()
                .filter(|(k, _)| k.as_str() != "noaa")
                .map(|(_, &v)| v).collect();
            let open_meteo_mean_diag = if !om_temps_diag.is_empty() {
                Some(om_temps_diag.iter().sum::<f64>() / om_temps_diag.len() as f64)
            } else {
                None
            };
            let model_disagreement_diag = if let (Some(noaa), Some(om_mean)) = (noaa_temp_diag, open_meteo_mean_diag) {
                Some((om_mean - noaa).abs())
            } else {
                None
            };
            let ensemble_count_diag = adjusted_forecast.ensemble_members.as_ref().map_or(0, |m| m.len());
            let (ensemble_min_diag, ensemble_max_diag, ensemble_mean_diag) =
                if let Some(ref members) = adjusted_forecast.ensemble_members {
                    let mn = members.iter().cloned().fold(f64::INFINITY, f64::min);
                    let mx = members.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                    let me = members.iter().sum::<f64>() / members.len() as f64;
                    (Some(mn), Some(mx), Some(me))
                } else {
                    (None, None, None)
                };
            let probability_source_diag = if ensemble_count_diag >= 20 {
                "ensemble"
            } else if adjusted_forecast.model_temps.len() >= 3 {
                "consensus"
            } else {
                "normal_dist"
            }.to_string();

            // Extract ensemble temperatures for CV computation (Task 1: CV-adjusted Kelly)
            let ensemble_temps_for_cv: Vec<f64> = adjusted_forecast.ensemble_members
                .as_ref()
                .map(|m| m.clone())
                .unwrap_or_default();

            // Ensemble standard deviation for calibration logging (Task 2)
            let ensemble_std = if ensemble_temps_for_cv.len() > 1 {
                let mean = ensemble_temps_for_cv.iter().sum::<f64>() / ensemble_temps_for_cv.len() as f64;
                let variance = ensemble_temps_for_cv.iter()
                    .map(|t| (t - mean).powi(2))
                    .sum::<f64>() / ensemble_temps_for_cv.len() as f64;
                variance.sqrt()
            } else {
                0.0
            };

            // Evaluate each bucket for edge
            for bucket in &market.buckets {
                if self.total_exposure >= self.config.max_total_exposure {
                    break;
                }

                let our_prob = match probs.get(&bucket.label) {
                    Some(&p) => p,
                    None => continue,
                };

                let market_price = bucket.yes_price;
                if market_price <= 0.0 || market_price >= 1.0 {
                    continue;
                }

                // Minimum market price filter â€” skip buckets priced below threshold
                // When market prices <5Â¢, our model is unreliable in the tails
                scan.buckets_evaluated += 1;
                if market_price < self.config.min_market_price {
                    debug!("SKIP: {} market price {:.3} below minimum {:.3}",
                        bucket.label, market_price, self.config.min_market_price);
                    scan.buckets_skipped_low_price += 1;
                    continue;
                }

                // Per-position deduplication: skip if we already have a position in this exact market+bucket
                let position_key = format!("{}|{}", market.question, bucket.label);
                if self.placed_this_session.contains(&position_key) {
                    debug!("SKIP: Already have position in {} | {}", market.question, bucket.label);
                    scan.buckets_skipped_dedup += 1;
                    continue;
                }

                // Double-check against trade log file (catches positions from previous sessions
                // that may not be in placed_this_session due to resolved/parsing issues)
                // Uses pre-loaded keys from start of run_once() â€” not per-bucket file reads
                if file_position_keys.contains(&position_key) {
                    warn!("DOUBLE-CHECK SKIP: {} already in trade log", position_key);
                    self.placed_this_session.insert(position_key.clone());
                    scan.buckets_skipped_dedup += 1;
                    continue;
                }

                // Forecast buffer check: skip bets where forecast is too close to bucket threshold.
                // A 1-2Â° shift in forecast can flip the outcome â€” avoid borderline bets.
                let buffer = match market.unit {
                    super::TempUnit::Fahrenheit => self.config.forecast_buffer_f,
                    super::TempUnit::Celsius => self.config.forecast_buffer_c,
                };
                let forecast_temp = adjusted_forecast.high_temp;
                let near_threshold = if bucket.temp_bucket.max_temp.is_finite() {
                    // "X or lower" bucket â€” forecast must be well below max
                    (forecast_temp - bucket.temp_bucket.max_temp).abs() < buffer
                } else if bucket.temp_bucket.min_temp.is_finite() {
                    // "X or higher" bucket â€” forecast must be well above min
                    (forecast_temp - bucket.temp_bucket.min_temp).abs() < buffer
                } else {
                    false
                };
                if near_threshold {
                    debug!(
                        "BUFFER SKIP: {} | forecast={:.1} too close to bucket threshold (buffer={:.1})",
                        bucket.label, forecast_temp, buffer
                    );
                    scan.buckets_skipped_buffer += 1;
                    continue;
                }

                // Task 2: NOAA cross-validation â€” adjust probability when NOAA disagrees
                let our_prob = cross_validate_with_noaa(
                    our_prob,
                    noaa_temp_diag,
                    &bucket.temp_bucket,
                    market.unit,
                );
                // Task 4: Clamp probability to [0.02, 0.95]
                let our_prob = clamp_probability(our_prob);

                // v7 Task 2: Southern Hemisphere seasonal warm bias correction
                let betting_cool = if !bucket.temp_bucket.min_temp.is_finite() {
                    true  // cumulative below
                } else if !bucket.temp_bucket.max_temp.is_finite() {
                    false // cumulative above
                } else {
                    let bucket_mid = (bucket.temp_bucket.min_temp + bucket.temp_bucket.max_temp) / 2.0;
                    let ens_mean = ensemble_mean_diag.unwrap_or(adjusted_forecast.high_temp);
                    bucket_mid < ens_mean
                };
                let city_for_sh: Option<&City> = market.city.as_deref()
                    .and_then(|cn| cities.iter().find(|c| c.name == cn));
                let market_month: Option<u32> = market.date.as_deref()
                    .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                    .map(|d| d.month());
                let sh_correction = if let (Some(city), Some(month)) = (city_for_sh, market_month) {
                    southern_hemisphere_seasonal_correction(city.southern_hemisphere, month, betting_cool)
                } else {
                    1.0
                };
                let our_prob = if sh_correction < 1.0 {
                    info!(
                        "SH SEASONAL CORRECTION: {} {} | prob {:.3} -> {:.3} (factor {:.2})",
                        forecast.city, bucket.label, our_prob, our_prob * sh_correction, sh_correction
                    );
                    (our_prob * sh_correction).clamp(0.02, 0.95)
                } else {
                    our_prob
                };

                // Edge = our probability - market price
                let edge = our_prob - market_price;

                // v7 Task 2: Hard skip for SH summer cool bets with weak edge
                let is_sh_summer = city_for_sh
                    .map(|c| c.southern_hemisphere)
                    .unwrap_or(false)
                    && market_month.map(|m| matches!(m, 12 | 1 | 2 | 3)).unwrap_or(false);
                if is_sh_summer && betting_cool && edge < 0.25 {
                    info!(
                        "SH SUMMER COOL SKIP: {} {} | edge={:.3} below 0.25 threshold",
                        forecast.city, bucket.label, edge
                    );
                    scan.buckets_skipped_no_edge += 1;
                    continue;
                }

                // Task 6: Narrow bucket ensemble check â€” require >= 5 members for tight ranges
                if narrow_bucket_insufficient_ensemble(&bucket.temp_bucket, market.unit, ensemble_count_diag) {
                    debug!(
                        "NARROW ENSEMBLE SKIP: {} | {} | range too tight with only {} ensemble members",
                        market.question, bucket.label, ensemble_count_diag
                    );
                    scan.buckets_skipped_narrow += 1;
                    continue;
                }

                // Narrow bucket filter: single-temp buckets (e.g. "18C") need higher edge
                // because ensemble overestimates probability on tight ranges
                let is_narrow = bucket.temp_bucket.min_temp.is_finite()
                    && bucket.temp_bucket.max_temp.is_finite()
                    && (bucket.temp_bucket.max_temp - bucket.temp_bucket.min_temp) <= 2.0;
                let required_edge = if is_narrow {
                    self.config.min_edge_narrow
                } else {
                    self.config.min_edge
                };

                if edge > self.config.min_edge && edge < required_edge && is_narrow {
                    debug!(
                        "NARROW SKIP: {} | {} | edge={:.3} < narrow_min={:.3} (range={:.1})",
                        market.question, bucket.label, edge, required_edge,
                        bucket.temp_bucket.max_temp - bucket.temp_bucket.min_temp
                    );
                    scan.buckets_skipped_narrow += 1;
                }

                if edge < required_edge {
                    scan.buckets_skipped_no_edge += 1;
                }

                // Task 2: Calibration logging â€” log EVERY evaluated bucket (trade_placed=false).
                // Provides data for future empirical calibration curve C(p,t).
                let bucket_type_str = if !bucket.temp_bucket.max_temp.is_finite() && bucket.temp_bucket.max_temp > 0.0 {
                    "cumulative_above"
                } else if !bucket.temp_bucket.min_temp.is_finite() && bucket.temp_bucket.min_temp < 0.0 {
                    "cumulative_below"
                } else if bucket.temp_bucket.min_temp.is_finite() && bucket.temp_bucket.max_temp.is_finite()
                    && (bucket.temp_bucket.max_temp - bucket.temp_bucket.min_temp).abs() <= 2.0 {
                    "narrow"
                } else {
                    "exact"
                };
                let cal_entry = CalibrationEntry {
                    timestamp: Utc::now().to_rfc3339(),
                    city: forecast.city.clone(),
                    market_date: market.date.clone().unwrap_or_default(),
                    market_question: market.question.clone(),
                    bucket_label: bucket.label.clone(),
                    bucket_type: bucket_type_str.to_string(),
                    model_probability: our_prob,
                    market_price,
                    edge,
                    ensemble_mean: ensemble_mean_diag.unwrap_or(adjusted_forecast.high_temp),
                    ensemble_std,
                    ensemble_count: ensemble_temps_for_cv.len(),
                    noaa_temp: noaa_temp_diag,
                    edge_cv: -1.0, // Not computed yet; updated in trade entry if trade placed
                    trade_placed: false,
                    trade_amount_usd: 0.0,
                    token_id: bucket.token_id.clone(),
                    resolution: None,
                };
                log_calibration_entry(&cal_entry);

                // Minimum probability gate — after calibration so filtered buckets are still logged
                if our_prob < self.config.min_our_probability {
                    warn!("TRADE BLOCKED (prob gate): {} | our_prob={:.2} < min={:.2}",
                        bucket.label, our_prob, self.config.min_our_probability);
                    scan.buckets_skipped_low_prob += 1;
                    continue;
                }

                if edge >= required_edge {
                    info!(
                        "EDGE FOUND: {} | {} | our={:.2} vs mkt={:.2} | edge={:.2}",
                        market.question, bucket.label, our_prob, market_price, edge
                    );

                    // Log per-model temperatures if available
                    if !forecast.model_temps.is_empty() {
                        let mut model_strs: Vec<String> = forecast.model_temps.iter()
                            .map(|(m, t)| format!("{}={:.1}", m, t))
                            .collect();
                        model_strs.sort();
                        let n_models = forecast.model_temps.len();
                        let spread = forecast.model_temps.values().cloned().fold(f64::NEG_INFINITY, f64::max)
                                   - forecast.model_temps.values().cloned().fold(f64::INFINITY, f64::min);
                        println!("     Models ({}/{}): {} | spread={:.1}",
                            n_models, n_models, model_strs.join(", "), spread);
                    }

                    // Kelly criterion position sizing
                    let base_kelly_size = self.calculate_kelly_size(our_prob, market_price, edge);

                    // === CV-ADJUSTED KELLY SIZING ===
                    // Paper: f_emp = f* * (1 - CV_edge) [Eq 4]
                    // Single uncertainty-aware multiplier replaces:
                    //   - bucket_type_factor (narrow/exact/cumulative)
                    //   - extreme_edge_size_factor
                    //   - 15% combined floor
                    let (edge_cv, group_count) = compute_edge_cv(
                        &ensemble_temps_for_cv,
                        bucket.temp_bucket.min_temp,
                        bucket.temp_bucket.max_temp,
                        market_price,
                    );

                    let cv_factor: f64 = (1.0_f64 - edge_cv).max(0.10); // Floor: never below 10% of base Kelly
                    let kelly_size = base_kelly_size * cv_factor;

                    // v7 Task 1: Hard per-bucket cap
                    let kelly_size_before_cap = kelly_size;
                    let kelly_size = kelly_size.min(self.config.max_per_bucket_hard_cap);
                    if kelly_size < kelly_size_before_cap {
                        info!(
                            "BUCKET CAP APPLIED: {} | cv-kelly=${:.2} -> capped at ${:.2}",
                            bucket.label, kelly_size_before_cap, kelly_size
                        );
                    }

                    // Bucket type label for logging only (no longer affects sizing)
                    let bucket_width = bucket.temp_bucket.max_temp - bucket.temp_bucket.min_temp;
                    let bucket_type_label = if !bucket.temp_bucket.max_temp.is_finite() || !bucket.temp_bucket.min_temp.is_finite() {
                        "wide_dir"
                    } else if bucket_width <= 2.0 {
                        "narrow"
                    } else if bucket_width <= 5.0 {
                        "medium"
                    } else {
                        "wide"
                    };

                    info!(
                        "KELLY CV: {} | base=${:.2} cv={:.3} groups={} factor={:.2} -> size=${:.2}",
                        bucket.label, base_kelly_size, edge_cv, group_count, cv_factor, kelly_size
                    );
                    if kelly_size < 0.50 {
                        warn!("TRADE BLOCKED: {} | {} | Kelly size too small (${:.2})", market.question, bucket.label, kelly_size);
                        continue;
                    }

                    // Order-book-aware pricing: fetch book and price relative to best ask
                    // High edge + liquidity = taker (2% fee but guaranteed fill)
                    // Moderate edge = near-ask limit (likely fills within minutes)
                    // Low edge = maker at 85% fair value (may not fill)
                    let order_price = match client.get_order_book(&bucket.token_id).await {
                        Ok(book) => {
                            let best_ask = book.asks.first().map(|a| a.price);
                            let ask_depth: f64 = book.asks.iter().take(3).map(|a| a.size * a.price).sum();

                            // Log order book state for edge trades
                            info!("BOOK: {} | best_ask={} depth=${:.2} | our_prob={:.2} kelly=${:.2}",
                                bucket.label,
                                best_ask.map_or("EMPTY".to_string(), |a| format!("{:.3}", a)),
                                ask_depth, our_prob, kelly_size
                            );

                            match (edge, best_ask, ask_depth) {
                                // HIGH EDGE + LIQUIDITY: take the ask (taker, guaranteed fill)
                                (e, Some(ask), depth) if e > 0.25 && depth >= kelly_size && ask <= our_prob => {
                                    info!("TAKER: edge={:.1}% depth=${:.2} â€” taking ask at {:.2}",
                                        e * 100.0, depth, ask);
                                    ask.min(0.95)
                                },
                                // MODERATE EDGE: post 1-2 cents below best ask
                                (e, Some(ask), _) if e > 0.15 && (ask - 0.02) <= our_prob => {
                                    let price = ((ask - 0.02) * 100.0).round() / 100.0;
                                    let price = price.max(0.01).min(0.95);
                                    info!("NEAR-ASK: edge={:.1}% â€” posting at {:.2} (ask={:.2})",
                                        e * 100.0, price, ask);
                                    price
                                },
                                // DEFAULT: maker at 85% fair value, but cap relative to market mid
                                _ => {
                                    let maker_price = (our_prob * 0.85 * 100.0).round() / 100.0;
                                    // On cheap buckets (<20c), don't bid more than 2x the market mid.
                                    // A 15c bucket with a 56c bid will never fill and wastes the edge.
                                    let price = if market_price < 0.20 {
                                        maker_price.min(market_price * 2.0)
                                    } else {
                                        maker_price
                                    };
                                    let price = price.max(0.01).min(0.95);
                                    info!("MAKER: our_prob={:.2} market_mid={:.2} â€” posting at {:.2}",
                                        our_prob, market_price, price);
                                    price
                                }
                            }
                        },
                        Err(e) => {
                            warn!("Order book fetch failed: {} â€” using maker pricing", e);
                            let maker_price = (our_prob * 0.85 * 100.0).round() / 100.0;
                            let price = if market_price < 0.20 {
                                maker_price.min(market_price * 2.0)
                            } else {
                                maker_price
                            };
                            price.max(0.01).min(0.95)
                        }
                    };

                    // Ensure we still have edge at our order price
                    if our_prob - order_price < 0.04 {
                        warn!("TRADE BLOCKED: {} | {} | Edge too thin at order price ${:.2} vs prob {:.3}",
                            market.question, bucket.label, order_price, our_prob);
                        continue;
                    }

                    let shares = kelly_size / order_price;
                    let shares = (shares * 100.0).floor() / 100.0;

                    let cost = shares * order_price;

                    // Dollar floor
                    if cost < 1.00 { warn!("TRADE BLOCKED: cost ${:.2} below $1.00 minimum", cost); continue; }
                    // Sanity guard: at least 1 share
                    if shares < 1.0 { warn!("TRADE BLOCKED: {:.2} shares below 1-share minimum (cost=${:.2})", shares, cost); continue; }

                    scan.trades_attempted += 1;
                    println!("  >> WEATHER TRADE: {} | {}", market.question, bucket.label);
                    println!("     Our P={:.3} | Mid={:.3} | Edge={:.3} | Kelly=${:.2} | Type={}",
                        our_prob, market_price, edge, kelly_size, bucket_type_label);
                    println!("     LIMIT BUY {:.2} YES @ ${:.4} = ${:.2}", shares, order_price, cost);

                    let mut captured_order_id: Option<String> = None;

                    if !self.dry_run {
                        match orders::place_order(
                            &client,
                            &bucket.token_id,
                            orders::Side::Buy,
                            order_price,
                            shares,
                            market.neg_risk,
                            false,
                        ).await {
                            Ok(result) => {
                                info!("Weather order placed: {} @ ${:.4}", bucket.label, order_price);
                                captured_order_id = result.get("orderID")
                                    .or_else(|| result.get("id"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                self.total_exposure += cost;
                                trades_placed += 1;
                                scan.trades_placed += 1;
                                scan.total_usd_deployed += cost;
                                self.placed_this_session.insert(position_key.clone());
                            }
                            Err(e) => {
                                error!("Weather order failed: {}", e);
                                self.notifier.notify_error("Weather order", &e.to_string()).await;
                                continue;
                            }
                        }
                    } else {
                        println!("     (DRY RUN â€” not executing)");
                        self.total_exposure += cost;
                        trades_placed += 1;
                        self.placed_this_session.insert(position_key.clone());
                    }

                    // Log trade
                    let trade = WeatherTrade {
                        timestamp: Utc::now().to_rfc3339(),
                        market_question: market.question.clone(),
                        bucket_label: bucket.label.clone(),
                        city: forecast.city.clone(),
                        our_probability: our_prob,
                        market_price,
                        edge,
                        side: "BUY_YES".to_string(),
                        shares,
                        price: order_price,
                        cost,
                        dry_run: self.dry_run,
                        resolved: false,
                        filled: false,
                        order_id: captured_order_id,
                        market_slug: Some(market.slug.clone()),
                        fill_confirmed: false,
                        outcome: None,
                        pnl: None,
                        resolution_temp: None,
                        token_id: Some(bucket.token_id.clone()),
                        open_meteo_mean: open_meteo_mean_diag,
                        noaa_temp: noaa_temp_diag,
                        ensemble_member_count: ensemble_count_diag,
                        ensemble_min: ensemble_min_diag,
                        ensemble_max: ensemble_max_diag,
                        ensemble_mean: ensemble_mean_diag,
                        model_disagreement: model_disagreement_diag,
                        probability_source: Some(probability_source_diag.clone()),
                        fill_status: None,
                        fill_checked_at: None,
                        condition_id: Some(market.condition_id.clone()),
                        redeemed: false,
                        neg_risk: market.neg_risk,
                        auto_exited: false,
                        market_date: market.date.clone(),
                    };

                    // Telegram notification
                    let msg = format!(
                        "Weather Trade\n\n{}\nBucket: {}\nCity: {}\n\nOur P: {:.1}% | Market: {:.1}%\nEdge: {:.1}%\n\nBUY {:.2} YES @ ${:.4} = ${:.2}{}",
                        market.question, bucket.label, forecast.city,
                        our_prob * 100.0, market_price * 100.0, edge * 100.0,
                        shares, order_price, cost,
                        if self.dry_run { "\n(DRY RUN)" } else { "" }
                    );
                    self.notifier.send(&msg).await;

                    self.trades.push(trade);

                    // Task 2: Calibration logging — log trade execution (trade_placed=true)
                    let mut trade_cal_entry = cal_entry.clone();
                    trade_cal_entry.trade_placed = true;
                    trade_cal_entry.trade_amount_usd = cost;
                    trade_cal_entry.edge_cv = edge_cv;
                    log_calibration_entry(&trade_cal_entry);

                    // Save immediately after each trade to prevent data loss on crash
                    if let Err(e) = self.save_trade_log() {
                        error!("Failed to save trade log: {}", e);
                    }

                    // Rate limit between orders
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }

            // === LADDERING PASS ===
            // After the main edge-detection pass, do a second pass for micro-positions
            if self.config.enable_laddering {
                // Collect buckets with edge, sorted by edge descending
                let mut ladder_candidates: Vec<(usize, f64, f64, f64)> = Vec::new(); // (idx, model_prob, market_price, edge)
                for (i, bucket) in market.buckets.iter().enumerate() {
                    let model_prob = probs.get(&bucket.label).copied().unwrap_or(0.0);
                    let market_price = bucket.ask_price.unwrap_or(bucket.yes_price);

                    if model_prob < self.config.ladder_min_model_prob {
                        continue;
                    }
                    if market_price > self.config.ladder_max_market_price || market_price <= 0.0 {
                        continue;
                    }
                    if model_prob <= market_price {
                        continue;
                    }

                    let position_key = format!("{}|{}", market.question, bucket.label);
                    if self.placed_this_session.contains(&position_key) {
                        continue;
                    }

                    let edge = model_prob - market_price;
                    ladder_candidates.push((i, model_prob, market_price, edge));
                }

                // Sort by edge descending â€” highest-edge cheap buckets first
                ladder_candidates.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));

                let mut ladder_count = 0usize;
                for (idx, model_prob, market_price, edge) in &ladder_candidates {
                    if ladder_count >= self.config.ladder_max_buckets {
                        break;
                    }
                    if self.total_exposure >= self.config.max_total_exposure {
                        debug!("LADDER SKIP: Would exceed max_total_exposure");
                        break;
                    }

                    let bucket = &market.buckets[*idx];
                    let position_key = format!("{}|{}", market.question, bucket.label);
                    let amount = self.config.ladder_amount_per_bucket;
                    let remaining = self.config.max_total_exposure - self.total_exposure;
                    if amount > remaining {
                        debug!("LADDER SKIP: amount ${:.2} > remaining ${:.2}", amount, remaining);
                        break;
                    }

                    // For ladder bets on cheap buckets, take the ask for speed
                    let order_price = *market_price;
                    let shares = (amount / order_price * 100.0).floor() / 100.0;

                    if shares < 1.0 {
                        debug!("LADDER SKIP: shares {:.2} below 1-share minimum", shares);
                        continue;
                    }

                    let cost = shares * order_price;

                    info!(
                        "LADDER BET: {} | {} | model={:.3} market={:.3} edge={:.3} | ${:.2}",
                        market.question, bucket.label, model_prob, market_price, edge, cost
                    );

                    let mut captured_order_id: Option<String> = None;

                    if !self.dry_run {
                        match orders::place_order(
                            &client,
                            &bucket.token_id,
                            orders::Side::Buy,
                            order_price,
                            shares,
                            market.neg_risk,
                            false,
                        ).await {
                            Ok(result) => {
                                info!("Ladder order placed: {} @ ${:.4}", bucket.label, order_price);
                                captured_order_id = result.get("orderID")
                                    .or_else(|| result.get("id"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                self.total_exposure += cost;
                                trades_placed += 1;
                                scan.ladder_trades_placed += 1;
                                scan.trades_placed += 1;
                                scan.total_usd_deployed += cost;
                                self.placed_this_session.insert(position_key.clone());
                            }
                            Err(e) => {
                                error!("Ladder order failed: {}", e);
                                continue;
                            }
                        }
                    } else {
                        println!("     LADDER (DRY RUN): {} | {} @ ${:.4} x {:.2} = ${:.2}",
                            bucket.label, "BUY_YES", order_price, shares, cost);
                        self.total_exposure += cost;
                        trades_placed += 1;
                        self.placed_this_session.insert(position_key.clone());
                    }

                    let trade = WeatherTrade {
                        timestamp: Utc::now().to_rfc3339(),
                        market_question: market.question.clone(),
                        bucket_label: bucket.label.clone(),
                        city: forecast.city.clone(),
                        our_probability: *model_prob,
                        market_price: *market_price,
                        edge: *edge,
                        side: "LADDER_BUY_YES".to_string(),
                        shares,
                        price: order_price,
                        cost,
                        dry_run: self.dry_run,
                        resolved: false,
                        filled: false,
                        order_id: captured_order_id,
                        market_slug: Some(market.slug.clone()),
                        fill_confirmed: false,
                        outcome: None,
                        pnl: None,
                        resolution_temp: None,
                        token_id: Some(bucket.token_id.clone()),
                        open_meteo_mean: open_meteo_mean_diag,
                        noaa_temp: noaa_temp_diag,
                        ensemble_member_count: ensemble_count_diag,
                        ensemble_min: ensemble_min_diag,
                        ensemble_max: ensemble_max_diag,
                        ensemble_mean: ensemble_mean_diag,
                        model_disagreement: model_disagreement_diag,
                        probability_source: Some(probability_source_diag.clone()),
                        fill_status: None,
                        fill_checked_at: None,
                        condition_id: Some(market.condition_id.clone()),
                        redeemed: false,
                        neg_risk: market.neg_risk,
                        auto_exited: false,
                        market_date: market.date.clone(),
                    };

                    self.trades.push(trade);

                    if let Err(e) = self.save_trade_log() {
                        error!("Failed to save ladder trade log: {}", e);
                    }

                    ladder_count += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }

                if ladder_count > 0 {
                    info!("Laddered {} buckets in market: {}", ladder_count, market.question);
                }
            }
        }

        if trades_placed > 0 {
            info!("Weather strategy: {} trades placed, ${:.2} total exposure", trades_placed, self.total_exposure);
        } else {
            info!("Weather strategy: no edges found this cycle");
        }

        // Write scan summary (Task 1)
        self.write_scan_summary(&scan);

        // Heartbeat: send status every 2 hours (every 4th cycle at 30min intervals)
        let hour = Utc::now().hour();
        let minute = Utc::now().minute();
        if minute < 30 && hour % 2 == 0 {
            let remaining = self.config.max_total_exposure - self.total_exposure;
            let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
            let heartbeat = format!(
                "[{}] [WEATHER HEARTBEAT] Bot running\nExposure: ${:.2}/${:.0} | Available: ${:.2}\nMarkets scanned: {} | Trades this cycle: {}",
                mode, self.total_exposure, self.config.max_total_exposure, remaining, weather_markets.len(), trades_placed
            );
            self.notifier.send(&heartbeat).await;
        }

        Ok(trades_placed)
    }

    /// Fetch forecasts from all sources for all configured cities
    async fn fetch_all_forecasts(&self, cities: &[City]) -> Vec<CityForecast> {
        let mut all_forecasts = Vec::new();

        for city in cities {
            let mut forecasts = match self.open_meteo.fetch_forecast(city).await {
                Ok(f) => f,
                Err(e) => {
                    warn!("Open-Meteo forecast failed for {}: {}", city.name, e);
                    Vec::new()
                }
            };

            // US cities: also fetch NOAA and merge as 5th model
            if city.unit == TempUnit::Fahrenheit {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                match self.noaa.fetch_forecast(city).await {
                    Ok(noaa_forecasts) => {
                        // Merge NOAA temps into Open-Meteo forecasts by date
                        let noaa_by_date: HashMap<String, f64> = noaa_forecasts
                            .into_iter()
                            .map(|f| (f.date.clone(), f.high_temp))
                            .collect();

                        for i in 0..forecasts.len() {
                            let date = forecasts[i].date.clone();
                            if let Some(&noaa_temp) = noaa_by_date.get(&date) {
                                // Apply configurable warm bias to match Open-Meteo bias
                                let biased_temp = noaa_temp + self.config.noaa_warm_bias_f;
                                forecasts[i].model_temps.insert("noaa".to_string(), biased_temp);

                                // Recalculate mean with NOAA included
                                let temps: Vec<f64> = forecasts[i].model_temps.values().cloned().collect();
                                forecasts[i].high_temp = temps.iter().sum::<f64>() / temps.len() as f64;

                                // Recalculate spread-based std_dev
                                let spread = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                                           - temps.iter().cloned().fold(f64::INFINITY, f64::min);
                                let days_ahead = i as f64 + 1.0;
                                forecasts[i].std_dev = (spread * 0.8).max(2.5) + (days_ahead - 1.0) * 1.0;

                                info!(
                                    "  {} {} | +NOAA={:.1}F | {} models | mean={:.1}",
                                    city.name, date, biased_temp,
                                    forecasts[i].model_temps.len(), forecasts[i].high_temp
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!("NOAA forecast failed for {}: {} (continuing with Open-Meteo only)", city.name, e);
                    }
                }
            }

            // Fetch ensemble data and attach to matching forecasts (Task 2)
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            match self.open_meteo.fetch_ensemble(city).await {
                Ok(ensemble_data) => {
                    for forecast in &mut forecasts {
                        if let Some(members) = ensemble_data.get(&forecast.date) {
                            forecast.ensemble_members = Some(members.clone());
                        }
                    }
                }
                Err(e) => {
                    warn!("Ensemble fetch failed for {}: {} (falling back to normal distribution)", city.name, e);
                }
            }

            all_forecasts.extend(forecasts);

            // Rate limit API calls
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        all_forecasts
    }

    /// Find the best matching forecast for a weather market
    fn find_matching_forecast<'a>(
        &self,
        market: &WeatherMarket,
        forecasts: &'a [CityForecast],
    ) -> Option<&'a CityForecast> {
        let market_city = market.city.as_deref()?;
        let market_date = market.date.as_deref();

        // Find forecast matching city and date
        forecasts.iter().find(|f| {
            let city_match = f.city.to_lowercase() == market_city.to_lowercase();
            let date_match = match market_date {
                Some(d) => f.date == d,
                None => true, // If no date in market, use first available forecast
            };
            city_match && date_match
        }).or_else(|| {
            // Fallback: just match city, use closest date
            forecasts.iter().find(|f| f.city.to_lowercase() == market_city.to_lowercase())
        })
    }

    /// Calculate position size using Kelly criterion
    fn calculate_kelly_size(&self, our_prob: f64, market_price: f64, _edge: f64) -> f64 {
        // Kelly fraction = (p * b - q) / b
        // where p = our probability, b = odds (payout / cost - 1), q = 1 - p
        let b = (1.0 / market_price) - 1.0; // odds
        let kelly_full = (our_prob * b - (1.0 - our_prob)) / b;

        // Fractional Kelly (more conservative)
        let kelly = kelly_full * self.config.kelly_fraction;

        // Clamp to max per bucket
        let bankroll = self.config.kelly_bankroll;
        let size = (kelly * bankroll).max(0.0).min(self.config.max_per_bucket);

        // Don't exceed remaining exposure
        let remaining = self.config.max_total_exposure - self.total_exposure;
        size.min(remaining)
    }

    /// Save trade log to strategy_trades.json (appends only the last trade)
    fn save_trade_log(&self) -> Result<()> {
        // Only save the most recently added trade (last in self.trades)
        // This avoids duplicate entries when called per-trade
        let Some(trade) = self.trades.last() else {
            return Ok(());
        };

        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        all_trades.push(trade.clone());

        let json = serde_json::to_string_pretty(&all_trades)?;
        std::fs::write("strategy_trades.json", json)?;

        Ok(())
    }

    /// Write scan summary to JSONL log file
    fn write_scan_summary(&self, scan: &ScanSummary) {
        info!(
            "SCAN SUMMARY: {} markets, {} evaluated, {} high_disagreement, {} no-forecast | {} buckets, {} trades placed, ${:.2} deployed | exposure: ${:.2}",
            scan.markets_discovered, scan.markets_evaluated,
            scan.markets_high_disagreement, scan.markets_skipped_no_forecast,
            scan.buckets_evaluated, scan.trades_placed,
            scan.total_usd_deployed, self.total_exposure
        );

        if let Ok(json) = serde_json::to_string(scan) {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true).append(true).open("scan_log.jsonl")
            {
                let _ = writeln!(f, "{}", json);
            }
        }
    }

    /// Check fill status for pending orders via CLOB API (Task 3)
    async fn check_fill_status(&mut self) {
        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let mut changed = false;
        let maker_addr = "0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853";

        for trade in all_trades.iter_mut() {
            if trade.dry_run {
                continue;
            }
            // Skip if already fill-checked (regardless of resolved status)
            if trade.fill_status.is_some() {
                continue;
            }
            // Need either order_id or token_id
            let token_id = match trade.token_id.as_deref() {
                Some(t) => t.to_string(),
                None => continue,
            };

            // Check if we have fills for this token
            let url = format!(
                "https://clob.polymarket.com/trades?maker={}&market={}",
                maker_addr, token_id
            );

            match self.http.get(&url).send().await {
                Ok(resp) => {
                    match resp.json::<Vec<serde_json::Value>>().await {
                        Ok(trades_list) => {
                            let status = if !trades_list.is_empty() {
                                "filled"
                            } else {
                                // Check if order is still open
                                if let Some(ref order_id) = trade.order_id {
                                    let order_url = format!(
                                        "https://clob.polymarket.com/order/{}",
                                        order_id
                                    );
                                    match self.http.get(&order_url).send().await {
                                        Ok(resp) => {
                                            match resp.json::<serde_json::Value>().await {
                                                Ok(order) => {
                                                    let status_str = order.get("status")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("unknown");
                                                    match status_str {
                                                        "MATCHED" => "filled",
                                                        "LIVE" => "unfilled",
                                                        "CANCELLED" => "cancelled",
                                                        _ => "unfilled",
                                                    }
                                                }
                                                Err(_) => "unfilled",
                                            }
                                        }
                                        Err(_) => "unfilled",
                                    }
                                } else {
                                    "unfilled"
                                }
                            };
                            trade.fill_status = Some(status.to_string());
                            trade.fill_checked_at = Some(Utc::now().to_rfc3339());
                            if status == "filled" {
                                trade.fill_confirmed = true;
                            }
                            info!("FILL CHECK: {} | {} | {}", trade.city, trade.bucket_label, status);
                            changed = true;
                        }
                        Err(_) => {}
                    }
                }
                Err(_) => {}
            }

            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        if changed {
            if let Ok(json) = serde_json::to_string_pretty(&all_trades) {
                let _ = std::fs::write("strategy_trades.json", json);
            }
        }
    }

    /// Reconcile local trade log with Polymarket activity API for ground-truth fill data.
    /// The CLOB API is ephemeral — orders vanish after settlement — so we use the
    /// data-api activity endpoint which persists indefinitely.
    async fn reconcile_with_api(&mut self) {
        let proxy_wallet = "0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853";

        // Fetch up to 500 activities (5 pages of 100)
        let mut all_activities: Vec<serde_json::Value> = Vec::new();
        let mut offset: u64 = 0;
        let page_limit: u64 = 100;
        let max_pages: u64 = 5;

        for _ in 0..max_pages {
            let url = format!(
                "https://data-api.polymarket.com/activity?user={}&limit={}&offset={}",
                proxy_wallet, page_limit, offset
            );
            match self.http.get(&url).send().await {
                Ok(resp) => {
                    match resp.json::<Vec<serde_json::Value>>().await {
                        Ok(page) => {
                            let count = page.len() as u64;
                            all_activities.extend(page);
                            if count < page_limit {
                                break; // last page
                            }
                            offset += page_limit;
                        }
                        Err(e) => {
                            warn!("API reconcile: failed to parse activity page: {}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!("API reconcile: failed to fetch activity: {}", e);
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if all_activities.is_empty() {
            debug!("API reconcile: no activities found");
            return;
        }

        // Filter for temperature-related TRADE/REDEEM activities
        let temp_activities: Vec<&serde_json::Value> = all_activities.iter()
            .filter(|a| {
                let atype = a.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let slug = a.get("eventSlug").and_then(|v| v.as_str()).unwrap_or("");
                (atype == "TRADE" || atype == "REDEEM") && slug.contains("temperature")
            })
            .collect();

        if temp_activities.is_empty() {
            debug!("API reconcile: no temperature activities found");
            return;
        }

        info!("API reconcile: {} temperature activities from data-api", temp_activities.len());

        // Group by eventSlug → calculate per-event P&L
        let mut event_pnl: HashMap<String, f64> = HashMap::new();
        let mut event_buys: HashMap<String, Vec<&serde_json::Value>> = HashMap::new();

        for act in &temp_activities {
            let slug = act.get("eventSlug").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let atype = act.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let side = act.get("side").and_then(|v| v.as_str()).unwrap_or("");
            let usdc = act.get("usdcSize")
                .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())))
                .unwrap_or(0.0);

            let pnl_entry = event_pnl.entry(slug.clone()).or_insert(0.0);
            match atype {
                "TRADE" if side == "BUY" => {
                    *pnl_entry -= usdc; // cost
                    event_buys.entry(slug).or_default().push(act);
                }
                "TRADE" if side == "SELL" => {
                    *pnl_entry += usdc; // revenue
                }
                "REDEEM" => {
                    *pnl_entry += usdc; // redemption payout
                }
                _ => {}
            }
        }

        // Load local trades
        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let mut changed = false;
        let now_str = Utc::now().to_rfc3339();

        for trade in all_trades.iter_mut() {
            if trade.dry_run {
                continue;
            }
            // Only reconcile trades that haven't been fill-checked yet
            if trade.fill_status.is_some() {
                continue;
            }

            let condition_id = trade.condition_id.as_deref().unwrap_or("");
            let token_id = trade.token_id.as_deref().unwrap_or("");
            if condition_id.is_empty() && token_id.is_empty() {
                continue;
            }

            // Try to match by checking API buy activities for matching asset/conditionId
            for (slug, buys) in &event_buys {
                let mut matched = false;
                for buy in buys {
                    let api_asset = buy.get("asset").and_then(|v| v.as_str()).unwrap_or("");
                    let api_condition = buy.get("conditionId").and_then(|v| v.as_str()).unwrap_or("");
                    let api_usdc = buy.get("usdcSize")
                        .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok())))
                        .unwrap_or(0.0);

                    // Match by token_id (asset) or condition_id
                    let id_match = (!token_id.is_empty() && api_asset == token_id)
                        || (!condition_id.is_empty() && api_condition == condition_id);

                    // Also check approximate cost match (within 50%)
                    let cost_ratio = if trade.cost > 0.0 && api_usdc > 0.0 {
                        (api_usdc / trade.cost).min(trade.cost / api_usdc)
                    } else {
                        0.0
                    };
                    let cost_match = cost_ratio > 0.5;

                    if id_match && cost_match {
                        matched = true;
                        trade.fill_status = Some("filled".to_string());
                        trade.fill_confirmed = true;
                        trade.fill_checked_at = Some(now_str.clone());

                        // Calculate proportional P&L from event totals
                        if let Some(&total_event_pnl) = event_pnl.get(slug) {
                            // Sum all buy costs for this event to get our share
                            let total_event_cost: f64 = buys.iter()
                                .filter_map(|b| b.get("usdcSize")
                                    .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))))
                                .sum();

                            if total_event_cost > 0.0 {
                                let share = trade.cost / total_event_cost;
                                let trade_pnl = total_event_pnl * share;
                                trade.pnl = Some((trade_pnl * 100.0).round() / 100.0);
                                trade.outcome = Some(if trade_pnl >= 0.0 { "WIN".to_string() } else { "LOSS".to_string() });
                            }
                        }

                        info!("API RECONCILE: {} | {} | FILLED | pnl=${:.2}",
                            trade.city, trade.bucket_label,
                            trade.pnl.unwrap_or(0.0));
                        changed = true;
                        break;
                    }
                }
                if matched {
                    break;
                }
            }

            // If no API match found and trade is old (>48h), mark as unfilled
            if trade.fill_status.is_none() {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&trade.timestamp) {
                    let age = Utc::now() - ts.with_timezone(&Utc);
                    if age.num_hours() > 48 {
                        trade.fill_status = Some("unfilled".to_string());
                        trade.fill_checked_at = Some(now_str.clone());
                        trade.outcome = Some("NO_FILL".to_string());
                        info!("API RECONCILE: {} | {} | NO_FILL (>48h, not in API)",
                            trade.city, trade.bucket_label);
                        changed = true;
                    }
                }
            }
        }

        if changed {
            if let Ok(json) = serde_json::to_string_pretty(&all_trades) {
                let _ = std::fs::write("strategy_trades.json", json);
            }
            info!("API reconcile complete: trade log updated");
        }
    }

    /// Run in a loop (with schedule-aware timing aligned to model releases)
    pub async fn run_loop(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        println!("\n== Weather Arbitrage Strategy - {} ==", mode);
        println!("   Model release hours (UTC): {:?}", self.config.scan_schedule.model_release_hours);
        println!("   Fallback interval: {} min", self.config.scan_schedule.fallback_interval_minutes);
        println!("   Post-release delay: {} min", self.config.scan_schedule.post_release_delay_minutes);
        println!("   Min edge: {:.0}%", self.config.min_edge * 100.0);
        println!("   Max per bucket: ${:.0}", self.config.max_per_bucket);
        println!("   Max total exposure: ${:.0}", self.config.max_total_exposure);
        println!("   Kelly fraction: {:.0}%", self.config.kelly_fraction * 100.0);
        println!("   Laddering: {}\n", if self.config.enable_laddering { "ENABLED" } else { "disabled" });

        let startup_msg = format!(
            "Weather Strategy Started ({})\nModel releases: {:?}Z | Fallback: {}min | Edge: {:.0}% | Max: ${:.0} | Laddering: {}",
            mode, self.config.scan_schedule.model_release_hours,
            self.config.scan_schedule.fallback_interval_minutes,
            self.config.min_edge * 100.0, self.config.max_total_exposure,
            if self.config.enable_laddering { "ON" } else { "OFF" }
        );
        self.notifier.send(&startup_msg).await;

        let mut last_weekly_summary_day: Option<u32> = None;

        loop {
            match self.run_once().await {
                Ok(n) => {
                    if n > 0 {
                        println!("  Weather: {} trades placed this cycle", n);
                    }
                }
                Err(e) => {
                    error!("Weather scan error: {}", e);
                    println!("Weather scan error: {}. Retrying...", e);
                }
            }

            // Weekly summary: Sunday midnight UTC (day_of_week = Sun, hour = 0)
            let now = Utc::now();
            if now.weekday() == chrono::Weekday::Sun && now.hour() == 0 {
                let day_of_year = now.ordinal();
                if last_weekly_summary_day != Some(day_of_year) {
                    self.weekly_summary().await;
                    last_weekly_summary_day = Some(day_of_year);
                }
            }

            // Schedule-aware sleep: target post_release_delay minutes after model releases
            let schedule = &self.config.scan_schedule;
            let next = next_scan_time(schedule);
            let wait = (next - Utc::now()).to_std().unwrap_or(
                std::time::Duration::from_secs(schedule.fallback_interval_minutes * 60)
            );
            let fallback = std::time::Duration::from_secs(schedule.fallback_interval_minutes * 60);
            let sleep_duration = wait.min(fallback);
            info!("Next scan in {} minutes (next model release window: {})",
                sleep_duration.as_secs() / 60, next.format("%H:%MZ"));
            tokio::time::sleep(sleep_duration).await;
        }
    }
}
