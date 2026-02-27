use anyhow::Result;
use chrono::{Utc, Timelike, Datelike};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tracing::{info, warn, error, debug};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders;

use super::forecast::{self, TempBucket};
use super::markets::{self, WeatherMarket};
use super::noaa::NoaaClient;
use super::open_meteo::OpenMeteoClient;
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
    /// Probability calculation method: "ensemble", "consensus", or "normal_dist"
    #[serde(default)]
    pub probability_source: Option<String>,
}

// ===== Strategy helper functions =====

/// Task 4: Clamp probability to [0.02, 0.95] before edge calculation.
fn clamp_probability(p: f64) -> f64 {
    p.max(0.02).min(0.95)
}

/// Task 5: Position size reduction factor for suspiciously large edges.
/// Linear scale from 1.0 at 30% edge down to 0.50 at 50%+ edge.
fn extreme_edge_size_factor(edge: f64) -> f64 {
    if edge <= 0.30 {
        1.0
    } else if edge >= 0.50 {
        0.50
    } else {
        1.0 - (edge - 0.30) / 0.20 * 0.50
    }
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
            .filter(|t| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                    let days_ago = (Utc::now() - ts.with_timezone(&Utc)).num_days();
                    days_ago <= 4 // weather markets can be up to 2 days out; 4-day window is safe
                } else {
                    false
                }
            })
            .map(|t| t.cost)
            .sum()
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
                            // Could not determine outcome — mark resolved but unknown
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
        // For now, return None — can be enhanced later with WU API key
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

    /// Weekly P&L summary — runs Sunday midnight UTC
    async fn weekly_summary(&self) {
        let all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => return,
        };

        let now = Utc::now();
        let week_ago = now - chrono::Duration::days(7);

        let recent: Vec<&WeatherTrade> = all_trades.iter()
            .filter(|t| !t.dry_run && t.resolved)
            .filter(|t| {
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                    ts.with_timezone(&Utc) >= week_ago
                } else {
                    false
                }
            })
            .collect();

        if recent.is_empty() {
            self.notifier.send("📊 WEEKLY SUMMARY\nNo resolved trades this week.").await;
            return;
        }

        let total = recent.len();
        let wins: Vec<&&WeatherTrade> = recent.iter().filter(|t| t.outcome.as_deref() == Some("WIN")).collect();
        let losses: Vec<&&WeatherTrade> = recent.iter().filter(|t| t.outcome.as_deref() == Some("LOSS")).collect();
        let no_fills: Vec<&&WeatherTrade> = recent.iter().filter(|t| t.outcome.as_deref() == Some("NO_FILL")).collect();
        let win_count = wins.len();
        let loss_count = losses.len();

        let total_pnl: f64 = recent.iter()
            .filter_map(|t| t.pnl)
            .sum();

        let win_rate = if win_count + loss_count > 0 {
            (win_count as f64 / (win_count + loss_count) as f64) * 100.0
        } else {
            0.0
        };

        // Best and worst trades
        let best = recent.iter()
            .filter(|t| t.pnl.is_some())
            .max_by(|a, b| a.pnl.unwrap_or(0.0).partial_cmp(&b.pnl.unwrap_or(0.0)).unwrap());
        let worst = recent.iter()
            .filter(|t| t.pnl.is_some())
            .min_by(|a, b| a.pnl.unwrap_or(0.0).partial_cmp(&b.pnl.unwrap_or(0.0)).unwrap());

        // Average our_prob on wins vs losses
        let avg_prob_wins = if !wins.is_empty() {
            wins.iter().map(|t| t.our_probability).sum::<f64>() / wins.len() as f64
        } else {
            0.0
        };
        let avg_prob_losses = if !losses.is_empty() {
            losses.iter().map(|t| t.our_probability).sum::<f64>() / losses.len() as f64
        } else {
            0.0
        };

        let mut msg = format!(
            "📊 WEEKLY SUMMARY\nTrades: {} | Wins: {} | Losses: {} | No-Fill: {} | Win rate: {:.0}%\nP&L: {:+.2}",
            total, win_count, loss_count, no_fills.len(), win_rate, total_pnl
        );

        if let Some(b) = best {
            msg.push_str(&format!("\nBest: {} {} ({:+.2})", b.city, b.bucket_label, b.pnl.unwrap_or(0.0)));
        }
        if let Some(w) = worst {
            msg.push_str(&format!("\nWorst: {} {} ({:+.2})", w.city, w.bucket_label, w.pnl.unwrap_or(0.0)));
        }

        if win_count > 0 || loss_count > 0 {
            msg.push_str(&format!(
                "\nAvg our_prob on wins: {:.2} | losses: {:.2}",
                avg_prob_wins, avg_prob_losses
            ));
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

        info!("Weather strategy scan starting ({})", mode);

        // Step 1: Discover weather markets
        let weather_markets = markets::discover_weather_markets(&self.http).await?;
        if weather_markets.is_empty() {
            info!("No weather markets found on Polymarket");
            return Ok(0);
        }
        info!("Found {} weather markets", weather_markets.len());

        // Step 2: Fetch forecasts for relevant cities
        let cities = get_cities(&self.config);
        let forecasts = self.fetch_all_forecasts(&cities).await;
        if forecasts.is_empty() {
            warn!("No forecasts fetched — skipping weather strategy");
            return Ok(0);
        }
        info!("Fetched forecasts for {} cities", forecasts.len());

        // Step 3: Match markets to forecasts and find edges
        let mut trades_placed = 0u32;
        let client = PolymarketClient::new()?;

        for market in &weather_markets {
            if self.total_exposure >= self.config.max_total_exposure {
                info!("Total weather exposure limit reached (${:.2})", self.total_exposure);
                break;
            }

            // Find matching forecast
            let forecast = match self.find_matching_forecast(market, &forecasts) {
                Some(f) => f.clone(),
                None => {
                    debug!("No matching forecast for market: {}", market.question);
                    continue;
                }
            };

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

            // Calculate probabilities — prefer ensemble when available (Task 2)
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

            // Task 3: Model disagreement circuit breaker — skip market if OM vs NOAA differ too much
            if models_disagree_too_much(&adjusted_forecast, market.unit) {
                let noaa_val = adjusted_forecast.model_temps.get("noaa").copied().unwrap_or(0.0);
                let om_vals: Vec<f64> = adjusted_forecast.model_temps.iter()
                    .filter(|(k, _)| k.as_str() != "noaa")
                    .map(|(_, &v)| v).collect();
                let om_mean_val = if !om_vals.is_empty() {
                    om_vals.iter().sum::<f64>() / om_vals.len() as f64
                } else { 0.0 };
                warn!(
                    "SKIP MARKET (model disagreement): {} | OM={:.1} NOAA={:.1} diff={:.1}{}",
                    market.question, om_mean_val, noaa_val,
                    (om_mean_val - noaa_val).abs(),
                    if market.unit == TempUnit::Fahrenheit { "F" } else { "C" }
                );
                continue;
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

                // Minimum market price filter — skip buckets priced below threshold
                // When market prices <5¢, our model is unreliable in the tails
                if market_price < self.config.min_market_price {
                    debug!("SKIP: {} market price {:.3} below minimum {:.3}",
                        bucket.label, market_price, self.config.min_market_price);
                    continue;
                }

                // Per-position deduplication: skip if we already have a position in this exact market+bucket
                let position_key = format!("{}|{}", market.question, bucket.label);
                if self.placed_this_session.contains(&position_key) {
                    debug!("SKIP: Already have position in {} | {}", market.question, bucket.label);
                    continue;
                }

                // Double-check against trade log file (catches positions from previous sessions
                // that may not be in placed_this_session due to resolved/parsing issues)
                let file_keys = Self::load_open_position_keys();
                if file_keys.contains(&position_key) {
                    warn!("DOUBLE-CHECK SKIP: {} already in trade log", position_key);
                    self.placed_this_session.insert(position_key.clone());
                    continue;
                }

                // Forecast buffer check: skip bets where forecast is too close to bucket threshold.
                // A 1-2° shift in forecast can flip the outcome — avoid borderline bets.
                let buffer = match market.unit {
                    super::TempUnit::Fahrenheit => self.config.forecast_buffer_f,
                    super::TempUnit::Celsius => self.config.forecast_buffer_c,
                };
                let forecast_temp = forecast.high_temp;
                let near_threshold = if bucket.temp_bucket.max_temp.is_finite() {
                    // "X or lower" bucket — forecast must be well below max
                    (forecast_temp - bucket.temp_bucket.max_temp).abs() < buffer
                } else if bucket.temp_bucket.min_temp.is_finite() {
                    // "X or higher" bucket — forecast must be well above min
                    (forecast_temp - bucket.temp_bucket.min_temp).abs() < buffer
                } else {
                    false
                };
                if near_threshold {
                    debug!(
                        "BUFFER SKIP: {} | forecast={:.1} too close to bucket threshold (buffer={:.1})",
                        bucket.label, forecast_temp, buffer
                    );
                    continue;
                }

                // Task 2: NOAA cross-validation — adjust probability when NOAA disagrees
                let our_prob = cross_validate_with_noaa(
                    our_prob,
                    noaa_temp_diag,
                    &bucket.temp_bucket,
                    market.unit,
                );
                // Task 4: Clamp probability to [0.02, 0.95]
                let our_prob = clamp_probability(our_prob);

                // Minimum probability filter — skip low-confidence predictions
                if our_prob < self.config.min_our_probability {
                    debug!("SKIP: {} our_prob {:.3} below minimum {:.3}",
                        bucket.label, our_prob, self.config.min_our_probability);
                    continue;
                }

                // Edge = our probability - market price
                let edge = our_prob - market_price;

                // Task 6: Narrow bucket ensemble check — require >= 5 members for tight ranges
                if narrow_bucket_insufficient_ensemble(&bucket.temp_bucket, market.unit, ensemble_count_diag) {
                    debug!(
                        "NARROW ENSEMBLE SKIP: {} | {} | range too tight with only {} ensemble members",
                        market.question, bucket.label, ensemble_count_diag
                    );
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
                    let raw_kelly_size = self.calculate_kelly_size(our_prob, market_price, edge);

                    // Task 5: Extreme edge warning — reduce position for suspiciously large edges
                    let kelly_size = if edge > 0.30 {
                        let factor = extreme_edge_size_factor(edge);
                        let scaled = raw_kelly_size * factor;
                        warn!(
                            "EXTREME EDGE {:.1}%: {} | {} | size factor={:.0}% -> ${:.2}",
                            edge * 100.0, market.question, bucket.label,
                            factor * 100.0, scaled
                        );
                        scaled
                    } else {
                        raw_kelly_size
                    };

                    if kelly_size < 0.50 {
                        debug!("Kelly size too small (${:.2}) -- skipping", kelly_size);
                        continue;
                    }

                    // Place limit order at our fair value price
                    // Weather markets have wide spreads — we act as makers, not takers
                    // Bid slightly below our probability to ensure positive EV
                    let order_price = (our_prob * 0.85 * 100.0).round() / 100.0; // 85% of our fair value, rounded to cents
                    let order_price = order_price.max(0.01).min(0.95); // clamp to valid range

                    // Ensure we still have edge at our order price
                    if our_prob - order_price < 0.04 {
                        debug!("Edge too thin at order price ${:.2} vs prob {:.2}", order_price, our_prob);
                        continue;
                    }

                    let shares = kelly_size / order_price;
                    let shares = (shares * 100.0).floor() / 100.0;

                    // Polymarket minimum order size is typically 5 shares
                    if shares < 5.0 {
                        debug!("Shares below minimum ({})", shares);
                        continue;
                    }

                    let cost = shares * order_price;

                    println!("  >> WEATHER TRADE: {} | {}", market.question, bucket.label);
                    println!("     Our P={:.3} | Mid={:.3} | Edge={:.3} | Kelly=${:.2}",
                        our_prob, market_price, edge, kelly_size);
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
                                // Capture order ID from CLOB response
                                captured_order_id = result.get("orderID")
                                    .or_else(|| result.get("id"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                self.total_exposure += cost;
                                trades_placed += 1;
                                self.placed_this_session.insert(position_key.clone());
                            }
                            Err(e) => {
                                error!("Weather order failed: {}", e);
                                self.notifier.notify_error("Weather order", &e.to_string()).await;
                                continue;
                            }
                        }
                    } else {
                        println!("     (DRY RUN — not executing)");
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

                // Sort by edge descending — highest-edge cheap buckets first
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

                    if shares < 5.0 {
                        debug!("LADDER SKIP: shares {:.2} below minimum", shares);
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
