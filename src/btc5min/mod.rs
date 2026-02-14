use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{info, warn, error};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders;

// --- Config ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Btc5minConfig {
    pub enabled: bool,
    pub trade_size: f64,
    pub min_confidence: f64,
    pub dry_run: bool,
}

impl Default for Btc5minConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            trade_size: 2.0,
            min_confidence: 0.55,
            dry_run: true,
        }
    }
}

// --- Prediction from Python ---

#[derive(Debug, Clone, Deserialize)]
pub struct PredictionResult {
    pub signal: Option<String>,
    pub confidence: Option<f64>,
    pub models: Option<std::collections::HashMap<String, String>>,
    pub model_confidences: Option<std::collections::HashMap<String, f64>>,
    pub error: Option<String>,
}

// --- Market discovery ---

#[derive(Debug, Clone)]
pub struct Btc5minMarket {
    pub slug: String,
    pub question: String,
    pub condition_id: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub start_timestamp: i64,
    pub neg_risk: bool,
}

// --- Results tracking ---

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TradeRecord {
    pub timestamp: String,
    pub market_slug: String,
    pub signal: String,
    pub confidence: f64,
    pub side: String, // "Up" or "Down"
    pub amount_usd: f64,
    pub price: f64,
    pub shares: f64,
    pub dry_run: bool,
    pub resolved: Option<String>,  // "win", "loss", or null
    pub models: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ResultsTracker {
    pub trades: Vec<TradeRecord>,
    pub total_trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub skipped: u32,
}

impl ResultsTracker {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
                Err(_) => Self::default(),
            }
        } else {
            Self::default()
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        Ok(())
    }

    fn path() -> PathBuf {
        PathBuf::from("btc5min_results.json")
    }

    pub fn add_trade(&mut self, record: TradeRecord) {
        self.total_trades += 1;
        self.trades.push(record);
    }

    pub fn record_result(&mut self, slug: &str, won: bool) {
        if won {
            self.wins += 1;
        } else {
            self.losses += 1;
        }
        // Update the last matching trade
        for trade in self.trades.iter_mut().rev() {
            if trade.market_slug == slug && trade.resolved.is_none() {
                trade.resolved = Some(if won { "win".to_string() } else { "loss".to_string() });
                break;
            }
        }
    }

    pub fn win_rate(&self) -> f64 {
        let total = self.wins + self.losses;
        if total == 0 { 0.0 } else { self.wins as f64 / total as f64 }
    }
}

// --- Core functions ---

/// Get the next 5-min window start timestamp (aligned to 5-min intervals, in ET)
pub fn next_5min_timestamp() -> i64 {
    let now = chrono::Utc::now().timestamp();
    // Round up to next 5-minute boundary
    let interval = 300i64;
    let next = ((now / interval) + 1) * interval;
    next
}

/// Try to find a BTC 5-min market by trying several timestamps
pub async fn find_btc5min_market(client: &PolymarketClient) -> Result<Option<Btc5minMarket>> {
    let now = chrono::Utc::now().timestamp();
    let interval = 300i64;

    // Try current window, next window, and the one after
    let base = (now / interval) * interval;
    let timestamps = vec![base, base + interval, base + 2 * interval, base - interval];

    for ts in timestamps {
        let slug = format!("btc-updown-5m-{}", ts);
        info!("Trying market slug: {}", slug);

        match fetch_btc5min_event(&slug).await {
            Ok(Some(market)) => {
                info!("Found market: {} ({})", market.question, market.slug);
                return Ok(Some(market));
            }
            Ok(None) => continue,
            Err(e) => {
                warn!("Error fetching {}: {}", slug, e);
                continue;
            }
        }
    }

    Ok(None)
}

/// Fetch a BTC 5-min event from Gamma API
async fn fetch_btc5min_event(slug: &str) -> Result<Option<Btc5minMarket>> {
    let url = format!(
        "https://gamma-api.polymarket.com/events?slug={}",
        slug
    );

    let http = reqwest::Client::builder()
        .user_agent("polymarket-bot/0.1.0")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let resp = http.get(&url).send().await?;
    let body: serde_json::Value = resp.json().await?;

    let events = match body.as_array() {
        Some(arr) => arr,
        None => return Ok(None),
    };

    if events.is_empty() {
        return Ok(None);
    }

    let event = &events[0];
    let markets = event.get("markets").and_then(|m| m.as_array());
    let markets = match markets {
        Some(m) if !m.is_empty() => m,
        _ => return Ok(None),
    };

    // BTC up/down markets have 2 outcomes in one market
    let market = &markets[0];

    let condition_id = market.get("conditionId")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let question = market.get("question")
        .or_else(|| event.get("title"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let neg_risk = event.get("negRisk")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // Get token IDs - outcomes array or clobTokenIds
    let mut up_token = String::new();
    let mut down_token = String::new();

    // Try from market's clobTokenIds
    if let Some(tokens_str) = market.get("clobTokenIds").and_then(|v| v.as_str()) {
        if let Ok(tokens) = serde_json::from_str::<Vec<String>>(tokens_str) {
            if tokens.len() >= 2 {
                up_token = tokens[0].clone();
                down_token = tokens[1].clone();
            }
        }
    }

    // Try from outcomes in event markets
    if up_token.is_empty() {
        // Look for "Up" and "Down" in the markets array
        for m in markets {
            let group_slug = m.get("groupItemTitle")
                .or_else(|| m.get("outcome"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();

            let token_ids_str = m.get("clobTokenIds")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");
            let token_ids: Vec<String> = serde_json::from_str(token_ids_str).unwrap_or_default();

            if group_slug.contains("up") && !token_ids.is_empty() {
                up_token = token_ids[0].clone();
            } else if group_slug.contains("down") && !token_ids.is_empty() {
                down_token = token_ids[0].clone();
            }
        }
    }

    if up_token.is_empty() || down_token.is_empty() {
        warn!("Could not find Up/Down token IDs for {}", slug);
        // If we have exactly 2 markets, assume first=Up, second=Down
        if markets.len() >= 2 {
            for (i, m) in markets.iter().enumerate() {
                let tokens_str = m.get("clobTokenIds")
                    .and_then(|v| v.as_str())
                    .unwrap_or("[]");
                let tokens: Vec<String> = serde_json::from_str(tokens_str).unwrap_or_default();
                if !tokens.is_empty() {
                    if i == 0 { up_token = tokens[0].clone(); }
                    if i == 1 { down_token = tokens[0].clone(); }
                }
            }
        }
    }

    if up_token.is_empty() || down_token.is_empty() {
        warn!("Still no token IDs for {} -- dumping event JSON", slug);
        warn!("Event: {}", serde_json::to_string_pretty(event).unwrap_or_default());
        return Ok(None);
    }

    // Parse timestamp from slug
    let start_ts: i64 = slug.strip_prefix("btc-updown-5m-")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    Ok(Some(Btc5minMarket {
        slug: slug.to_string(),
        question,
        condition_id,
        up_token_id: up_token,
        down_token_id: down_token,
        start_timestamp: start_ts,
        neg_risk,
    }))
}

/// Call btc_predict.py and parse the result
pub async fn get_prediction() -> Result<PredictionResult> {
    let script_path = PathBuf::from("btc_predict.py");
    if !script_path.exists() {
        anyhow::bail!("btc_predict.py not found in current directory");
    }

    let output = tokio::process::Command::new("python")
        .arg(&script_path)
        .output()
        .await
        .context("Failed to run btc_predict.py")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !stderr.is_empty() {
        info!("btc_predict.py stderr: {}", stderr.chars().take(500).collect::<String>());
    }

    let stdout_trimmed = stdout.trim();
    if stdout_trimmed.is_empty() {
        anyhow::bail!("btc_predict.py returned empty output. stderr: {}", stderr);
    }

    let result: PredictionResult = serde_json::from_str(stdout_trimmed)
        .context(format!("Failed to parse prediction JSON: {}", stdout_trimmed))?;

    Ok(result)
}

/// Place a trade on a BTC 5-min market
pub async fn place_btc5min_trade(
    client: &PolymarketClient,
    market: &Btc5minMarket,
    signal: &str,
    confidence: f64,
    config: &Btc5minConfig,
) -> Result<Option<(String, f64, f64)>> {
    let (side_name, token_id) = match signal {
        "BUY" => ("Up", &market.up_token_id),
        "SELL" => ("Down", &market.down_token_id),
        _ => return Ok(None),
    };

    if confidence < config.min_confidence {
        info!("Confidence {:.1}% below threshold {:.1}% -- skipping",
              confidence * 100.0, config.min_confidence * 100.0);
        return Ok(None);
    }

    // Get best ask price
    let book = client.get_order_book(token_id).await
        .context("Failed to get order book")?;
    let price = book.asks.first()
        .map(|a| a.price)
        .unwrap_or(0.50);

    if price <= 0.0 || price >= 1.0 {
        warn!("Unusual price {:.4} for {} -- skipping", price, side_name);
        return Ok(None);
    }

    let size = config.trade_size / price;

    info!("Placing {} order: {} {} shares @ ${:.4} (${:.2})",
          if config.dry_run { "DRY RUN" } else { "LIVE" },
          side_name, size, price, config.trade_size);

    let result = orders::place_order(
        client,
        token_id,
        orders::Side::Buy,
        price,
        size,
        market.neg_risk,
        config.dry_run,
    ).await;

    match result {
        Ok(_) => {
            info!("Order placed successfully for {}", side_name);
            Ok(Some((side_name.to_string(), price, size)))
        }
        Err(e) => {
            error!("Order failed: {}", e);
            Err(e)
        }
    }
}

/// Check resolutions for unresolved trades
pub async fn check_resolutions(
    tracker: &mut ResultsTracker,
    notifier: &TelegramNotifier,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut any_updated = false;

    for trade in tracker.trades.iter_mut() {
        if trade.resolved.is_some() {
            continue;
        }

        // Parse timestamp from slug (e.g. btc-updown-5m-1771028700)
        let start_ts: i64 = trade.market_slug
            .strip_prefix("btc-updown-5m-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Wait at least 6 minutes after market start for resolution
        if start_ts == 0 || now < start_ts + 360 {
            continue;
        }

        // Fetch market from Gamma API to check resolution
        let url = format!(
            "https://gamma-api.polymarket.com/events?slug={}",
            trade.market_slug
        );

        let http = reqwest::Client::builder()
            .user_agent("polymarket-bot/0.1.0")
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        let resp = match http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to check resolution for {}: {}", trade.market_slug, e);
                continue;
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(b) => b,
            Err(_) => continue,
        };

        let events = match body.as_array() {
            Some(arr) if !arr.is_empty() => arr,
            _ => continue,
        };

        let event = &events[0];
        let markets = event.get("markets").and_then(|m| m.as_array());
        let markets = match markets {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };

        // Check if market is resolved by looking at outcomePrices
        // Polymarket sets outcomePrices to ["1","0"] or ["0","1"] when resolved
        // (the "resolved" field may be null even when settled)
        let mut winner: Option<String> = None;

        let m = &markets[0];
        if let Some(outcomes_str) = m.get("outcomePrices").and_then(|v| v.as_str()) {
            if let Ok(prices) = serde_json::from_str::<Vec<String>>(outcomes_str) {
                if prices.len() >= 2 {
                    let p0: f64 = prices[0].parse().unwrap_or(0.5);
                    let p1: f64 = prices[1].parse().unwrap_or(0.5);
                    // Only count as resolved if one outcome is clearly 1.0 and other 0.0
                    if (p0 - 1.0).abs() < 0.01 && p1.abs() < 0.01 {
                        // First outcome (Up/Yes) won
                        winner = Some("Up".to_string());
                    } else if p0.abs() < 0.01 && (p1 - 1.0).abs() < 0.01 {
                        // Second outcome (Down/No) won
                        winner = Some("Down".to_string());
                    }
                    // If prices are still mid-range (e.g. 0.55/0.45), market is not yet resolved
                }
            }
        }

        if let Some(ref w) = winner {
            let won = trade.side == *w;
            trade.resolved = Some(if won { "win".to_string() } else { "loss".to_string() });
            if won {
                tracker.wins += 1;
            } else {
                tracker.losses += 1;
            }
            any_updated = true;

            let emoji = if won { "✅" } else { "❌" };
            let total = tracker.wins + tracker.losses;
            let wr = if total > 0 { tracker.wins as f64 / total as f64 * 100.0 } else { 0.0 };
            info!("{} {} | Predicted: {} | Actual: {} | Record: {}-{} ({:.0}% WR)",
                  emoji, trade.market_slug, trade.side, w, tracker.wins, tracker.losses, wr);

            notifier.send(&format!(
                "{} <b>BTC 5min Result</b>\n\
                 Market: {}\n\
                 Predicted: {} | Actual: {}\n\
                 Record: {}-{}-{} (WR: {:.0}%)",
                emoji, trade.market_slug, trade.side, w,
                tracker.wins, tracker.losses, tracker.skipped, wr
            )).await;
        }
    }

    if any_updated {
        tracker.save()?;
    }

    Ok(())
}

/// Run one cycle: find market, predict, trade
pub async fn run_cycle(
    client: &PolymarketClient,
    config: &Btc5minConfig,
    notifier: &TelegramNotifier,
    tracker: &mut ResultsTracker,
) -> Result<()> {
    info!("=== BTC 5-min cycle start ===");

    // 1. Find the next market
    let market = match find_btc5min_market(client).await? {
        Some(m) => m,
        None => {
            info!("No BTC 5-min market found right now");
            return Ok(());
        }
    };

    info!("Market: {} | Up: {}... | Down: {}...",
          market.question,
          &market.up_token_id[..market.up_token_id.len().min(12)],
          &market.down_token_id[..market.down_token_id.len().min(12)]);

    // Check if we already traded this market
    let already_traded = tracker.trades.iter().any(|t| t.market_slug == market.slug);
    if already_traded {
        info!("Already traded {} -- skipping", market.slug);
        return Ok(());
    }

    // 2. Get prediction
    let prediction = match get_prediction().await {
        Ok(p) => p,
        Err(e) => {
            warn!("Prediction failed: {}", e);
            notifier.notify_error("BTC 5-min prediction", &format!("{}", e)).await;
            return Ok(());
        }
    };

    if let Some(err) = &prediction.error {
        warn!("Prediction error: {}", err);
        notifier.notify_error("BTC 5-min prediction", err).await;
        return Ok(());
    }

    let signal = prediction.signal.as_deref().unwrap_or("HOLD");
    let confidence = prediction.confidence.unwrap_or(0.0);
    let models = prediction.models.clone().unwrap_or_default();

    info!("Prediction: {} (confidence: {:.1}%) | RF:{} XGB:{} LGB:{}",
          signal, confidence * 100.0,
          models.get("rf").map(|s| s.as_str()).unwrap_or("?"),
          models.get("xgb").map(|s| s.as_str()).unwrap_or("?"),
          models.get("lgb").map(|s| s.as_str()).unwrap_or("?"));

    // 3. Trade decision
    if signal == "HOLD" || confidence < config.min_confidence {
        info!("Signal: {} conf: {:.1}% -- skipping trade", signal, confidence * 100.0);
        tracker.skipped += 1;
        tracker.save()?;
        return Ok(());
    }

    // 4. Place trade
    match place_btc5min_trade(client, &market, signal, confidence, config).await? {
        Some((side, price, shares)) => {
            let record = TradeRecord {
                timestamp: chrono::Utc::now().to_rfc3339(),
                market_slug: market.slug.clone(),
                signal: signal.to_string(),
                confidence,
                side: side.clone(),
                amount_usd: config.trade_size,
                price,
                shares,
                dry_run: config.dry_run,
                resolved: None,
                models: models.clone(),
            };
            tracker.add_trade(record);
            tracker.save()?;

            let mode = if config.dry_run { "DRY RUN" } else { "LIVE" };
            let msg = format!(
                "<b>BTC 5min Trade ({})</b>\n\
                 Market: {}\n\
                 Signal: {} -> Buy {} @ ${:.4}\n\
                 Confidence: {:.1}% | Size: ${:.2}\n\
                 Models: RF:{} XGB:{} LGB:{}\n\
                 Record: {}-{}-{} (WR: {:.0}%)",
                mode,
                market.question,
                signal, side, price,
                confidence * 100.0, config.trade_size,
                models.get("rf").map(|s| s.as_str()).unwrap_or("?"),
                models.get("xgb").map(|s| s.as_str()).unwrap_or("?"),
                models.get("lgb").map(|s| s.as_str()).unwrap_or("?"),
                tracker.wins, tracker.losses, tracker.skipped,
                tracker.win_rate() * 100.0,
            );
            notifier.send(&msg).await;
        }
        None => {
            info!("Trade skipped (below threshold or error)");
            tracker.skipped += 1;
            tracker.save()?;
        }
    }

    Ok(())
}

/// Main run loop
pub async fn run_loop(config: Btc5minConfig) -> Result<()> {
    info!("Starting BTC 5-min trading loop");
    info!("Config: trade_size=${:.2}, min_confidence={:.0}%, dry_run={}",
          config.trade_size, config.min_confidence * 100.0, config.dry_run);

    let client = PolymarketClient::new()?;
    let notifier = TelegramNotifier::new();
    let mut tracker = ResultsTracker::load();

    info!("Loaded tracker: {} trades, {} wins, {} losses, {} skipped",
          tracker.total_trades, tracker.wins, tracker.losses, tracker.skipped);

    notifier.send(&format!(
        "<b>BTC 5min Bot Started</b>\n\
         Trade size: ${:.2} | Min confidence: {:.0}%\n\
         Dry run: {} | Record: {}-{}-{}",
        config.trade_size, config.min_confidence * 100.0,
        config.dry_run, tracker.wins, tracker.losses, tracker.skipped
    )).await;

    loop {
        // Check resolutions for past trades first
        if let Err(e) = check_resolutions(&mut tracker, &notifier).await {
            warn!("Resolution check error: {}", e);
        }

        match run_cycle(&client, &config, &notifier, &mut tracker).await {
            Ok(()) => {}
            Err(e) => {
                error!("Cycle error: {}", e);
                notifier.notify_error("BTC 5-min cycle", &format!("{}", e)).await;
            }
        }

        // Sleep 60 seconds between checks
        info!("Sleeping 60s until next check...");
        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    }
}

/// Load btc5min config from strategy_config.json
pub fn load_config() -> Result<Btc5minConfig> {
    let config_path = PathBuf::from("strategy_config.json");
    if !config_path.exists() {
        info!("strategy_config.json not found, using defaults");
        return Ok(Btc5minConfig::default());
    }

    let contents = std::fs::read_to_string(&config_path)?;
    let json: serde_json::Value = serde_json::from_str(&contents)?;

    if let Some(btc5min) = json.get("btc5min") {
        let config: Btc5minConfig = serde_json::from_value(btc5min.clone())?;
        Ok(config)
    } else {
        info!("No btc5min section in strategy_config.json, using defaults");
        Ok(Btc5minConfig::default())
    }
}
