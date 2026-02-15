use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::{info, warn, error};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders;

/// Minimum profit threshold to execute an arb (after accounting for rounding)
const MIN_PROFIT_PCT: f64 = 0.015; // 1.5% minimum spread
/// Maximum USD per arb trade (each side)
const MAX_ARB_SIZE: f64 = 10.0;
/// Scan interval in seconds
const SCAN_INTERVAL_SECS: u64 = 30;

// --- Sniper constants ---
/// Minimum price to consider "near-resolved"
const SNIPER_MIN_PRICE: f64 = 0.90; // lowered from 0.95 — more opportunities, higher profit
/// Maximum price we'll pay (99.9¢ for 0.001 tick markets, 99¢ for 0.01 tick)
const SNIPER_MAX_PRICE: f64 = 0.999;
/// Maximum USD per sniper trade
const SNIPER_MAX_SIZE: f64 = 25.0;
/// Minimum volume for sniper targets (need liquidity for tight spreads)
const SNIPER_MIN_VOLUME: f64 = 50_000.0; // lowered from 100K — more fast-resolving markets
/// Default max exposure (fallback if balance fetch fails)
const DEFAULT_MAX_SNIPER_EXPOSURE: f64 = 70.0;
/// Reserve buffer — always keep this much USD available (don't invest 100%)
const SNIPER_RESERVE_BUFFER: f64 = 1.0;
/// Maximum days until resolution for sniper targets (skip 2028 presidential etc.)
const SNIPER_MAX_DAYS_TO_RESOLVE: f64 = 365.0;

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub question: String,
    pub slug: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub yes_ask: f64,
    pub no_ask: f64,
    pub spread: f64, // 1.0 - (yes_ask + no_ask)
    pub neg_risk: bool,
    pub volume: f64,
}

#[derive(Debug, Clone)]
pub struct SniperOpportunity {
    pub condition_id: String,
    pub question: String,
    pub slug: String,
    pub token_id: String,
    pub side: String, // "YES" or "NO"
    pub ask_price: f64,
    pub mid_price: f64,
    pub expected_profit_pct: f64, // (1.0 - ask_price) / ask_price
    pub neg_risk: bool,
    pub volume: f64,
    pub tick_size: f64,
    pub days_to_resolve: f64, // estimated days until resolution
    pub score: f64,           // higher = better (profit% / sqrt(days))
}

/// Raw Gamma API market
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarket {
    condition_id: Option<String>,
    question: Option<String>,
    slug: Option<String>,
    #[serde(default)]
    volume: Option<serde_json::Value>,
    outcome_prices: Option<String>,
    clob_token_ids: Option<String>,
    end_date_iso: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    closed: Option<bool>,
    #[serde(default)]
    neg_risk: Option<bool>,
}

pub struct ArbScanner {
    http: reqwest::Client,
    gamma_url: String,
    notifier: TelegramNotifier,
    dry_run: bool,
    trades_executed: u64,
    total_profit: f64,
    sniper_trades: u64,
    sniper_profit: f64,
    sniper_committed: f64, // total USD committed to sniper orders (locks balance)
    max_sniper_exposure: f64, // dynamic limit based on balance
    sniped_markets: std::collections::HashSet<String>, // avoid re-sniping same market
    tick_size_cache: std::collections::HashMap<String, f64>, // condition_id -> tick_size
    cycle_count: u64,
    last_summary_cycle: u64,
}

impl ArbScanner {
    pub fn new(dry_run: bool) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("polymarket-arb/0.1.0")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
            gamma_url: "https://gamma-api.polymarket.com".to_string(),
            notifier: TelegramNotifier::new(),
            dry_run,
            trades_executed: 0,
            total_profit: 0.0,
            sniper_trades: 0,
            sniper_profit: 0.0,
            sniper_committed: 0.0, // reset on restart — orders may have filled or expired
            max_sniper_exposure: DEFAULT_MAX_SNIPER_EXPOSURE,
            sniped_markets: std::collections::HashSet::new(),
            tick_size_cache: std::collections::HashMap::new(),
            cycle_count: 0,
            last_summary_cycle: 0,
        }
    }

    /// Fetch all active markets from Gamma API
    async fn fetch_markets(&self) -> Result<Vec<GammaMarket>> {
        let mut all = Vec::new();

        // Fetch top volume markets
        let url = format!(
            "{}/markets?closed=false&active=true&order=volume&ascending=false&limit=200",
            self.gamma_url
        );

        let markets: Vec<GammaMarket> = self.http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch markets")?
            .json()
            .await
            .context("Failed to parse markets")?;

        all.extend(markets);

        // Also fetch by 24h volume for fast-moving markets
        let url2 = format!(
            "{}/markets?closed=false&active=true&order=volume24hr&ascending=false&limit=100",
            self.gamma_url
        );

        if let Ok(resp) = self.http.get(&url2).send().await {
            if let Ok(fast) = resp.json::<Vec<GammaMarket>>().await {
                let existing: std::collections::HashSet<String> = all.iter()
                    .filter_map(|m| m.condition_id.clone())
                    .collect();
                for m in fast {
                    if let Some(ref cid) = m.condition_id {
                        if !existing.contains(cid) {
                            all.push(m);
                        }
                    }
                }
            }
        }

        Ok(all)
    }

    /// Check a single market for arb opportunity using order book
    async fn check_arb(&self, client: &PolymarketClient, market: &GammaMarket) -> Option<ArbOpportunity> {
        let question = market.question.as_deref()?;
        let slug = market.slug.as_deref()?;

        // Parse token IDs
        let tokens: Vec<String> = market.clob_token_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())?;
        if tokens.len() < 2 {
            return None;
        }

        // Quick pre-filter using Gamma prices (avoid unnecessary order book calls)
        let prices: Vec<f64> = market.outcome_prices
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())?;
        if prices.len() < 2 {
            return None;
        }

        // Pre-filter: only check order books if mid prices suggest possible arb
        // (YES + NO < 0.99 based on mid prices)
        if prices[0] + prices[1] >= 0.99 {
            return None;
        }

        // Fetch actual order books for accurate ask prices
        let yes_book = client.get_order_book(&tokens[0]).await.ok()?;
        let no_book = client.get_order_book(&tokens[1]).await.ok()?;

        // Best ask = lowest sell price (what we can buy at)
        let yes_ask = yes_book.asks.first().map(|a| a.price)?;
        let no_ask = no_book.asks.first().map(|a| a.price)?;

        let total_cost = yes_ask + no_ask;
        let spread = 1.0 - total_cost;

        if spread >= MIN_PROFIT_PCT {
            let volume = match &market.volume {
                Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                Some(serde_json::Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };

            Some(ArbOpportunity {
                question: question.to_string(),
                slug: slug.to_string(),
                yes_token_id: tokens[0].clone(),
                no_token_id: tokens[1].clone(),
                yes_ask,
                no_ask,
                spread,
                neg_risk: market.neg_risk.unwrap_or(true),
                volume,
            })
        } else {
            None
        }
    }

    /// Execute an arb trade: buy YES and NO at the ask prices
    async fn execute_arb(&mut self, opp: &ArbOpportunity) -> Result<()> {
        let size_usd = MAX_ARB_SIZE.min(50.0); // cap per side
        let yes_shares = size_usd / opp.yes_ask;
        let no_shares = size_usd / opp.no_ask;

        // Use the smaller share count so both sides match
        let shares = yes_shares.min(no_shares).min(100.0);
        // Round down to 2 decimal places
        let shares = (shares * 100.0).floor() / 100.0;

        if shares < 1.0 {
            warn!("Arb shares too small: {:.2}", shares);
            return Ok(());
        }

        let yes_cost = shares * opp.yes_ask;
        let no_cost = shares * opp.no_ask;
        let total_cost = yes_cost + no_cost;
        let profit = shares * 1.0 - total_cost; // shares pay $1 each on resolution

        println!("  >> Executing arb:");
        println!("     BUY {:.2} YES @ ${:.4} = ${:.2}", shares, opp.yes_ask, yes_cost);
        println!("     BUY {:.2} NO  @ ${:.4} = ${:.2}", shares, opp.no_ask, no_cost);
        println!("     Total cost: ${:.2} | Guaranteed payout: ${:.2} | Profit: ${:.2}", total_cost, shares, profit);

        if self.dry_run {
            println!("     (DRY RUN - not executing)\n");
            return Ok(());
        }

        // Place YES order first
        let client = PolymarketClient::new()?;
        match orders::place_order(&client, &opp.yes_token_id, orders::Side::Buy, opp.yes_ask, shares, opp.neg_risk, false).await {
            Ok(_) => info!("YES order placed"),
            Err(e) => {
                error!("Failed to place YES order: {}", e);
                self.notifier.notify_error("Arb YES order", &e.to_string()).await;
                return Err(e);
            }
        }

        // Small delay between orders
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Place NO order
        match orders::place_order(&client, &opp.no_token_id, orders::Side::Buy, opp.no_ask, shares, opp.neg_risk, false).await {
            Ok(_) => info!("NO order placed"),
            Err(e) => {
                error!("Failed to place NO order (YES already placed!): {}", e);
                self.notifier.notify_error("Arb NO order FAILED (YES already placed)", &e.to_string()).await;
                return Err(e);
            }
        }

        self.trades_executed += 1;
        self.total_profit += profit;

        // Telegram notification
        let msg = format!(
            "Arb Trade #{}\n\n\"{}\"\n\nBUY {:.2} YES @ ${:.4} = ${:.2}\nBUY {:.2} NO @ ${:.4} = ${:.2}\nTotal: ${:.2} | Payout: ${:.2}\nProfit: ${:.2} ({:.1}%)\n\nSession total: ${:.2} from {} arbs",
            self.trades_executed,
            truncate(&opp.question, 60),
            shares, opp.yes_ask, yes_cost,
            shares, opp.no_ask, no_cost,
            total_cost, shares, profit, opp.spread * 100.0,
            self.total_profit, self.trades_executed
        );
        self.notifier.send(&msg).await;

        Ok(())
    }

    /// Estimate how many days until a market resolves
    fn estimate_resolution_days(end_date: Option<&str>, question: &str) -> f64 {
        // Try end_date_iso first
        if let Some(end) = end_date {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(end) {
                let days = dt.signed_duration_since(chrono::Utc::now()).num_hours() as f64 / 24.0;
                if days > 0.0 { return days; }
                return 0.1;
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(end, "%Y-%m-%dT%H:%M:%S%.fZ") {
                let now = chrono::Utc::now().naive_utc();
                let days = dt.signed_duration_since(now).num_hours() as f64 / 24.0;
                if days > 0.0 { return days; }
                return 0.1;
            }
        }

        // Heuristic from question text
        let q = question.to_lowercase();

        // Very fast: today/tomorrow keywords
        if q.contains("today") || q.contains("tonight") { return 0.5; }

        // Check for specific month mentions relative to now (Feb 2026)
        let now: chrono::DateTime<chrono::Utc> = chrono::Utc::now();
        let current_month = now.format("%B").to_string().to_lowercase(); // "february"
        let current_year = now.format("%Y").to_string(); // "2026"

        // "in February" or "February 2026" = this month (0-28 days)
        if q.contains(&format!("{} {}", &current_month, &current_year)) || q.contains(&format!("in {}", &current_month)) {
            return 14.0;
        }
        if q.contains("february") && q.contains("2026") { return 14.0; }
        if q.contains("by february") || q.contains("before february") { return 14.0; }

        // Specific near-term dates: "by March", "Q1 2026"
        if q.contains("march 2026") || q.contains("by march") { return 30.0; }
        if q.contains("q1 2026") { return 45.0; }
        if q.contains("april 2026") || q.contains("by april") { return 60.0; }

        // Sports seasons (resolve within months)
        if q.contains("2025-26") || q.contains("2025\u{2013}26") { return 120.0; }

        // 2026 without specific month = within the year
        if q.contains("2026") && !q.contains("2027") && !q.contains("2028") { return 180.0; }

        // 2027 = ~1-2 years
        if q.contains("2027") { return 500.0; }

        // 2028 presidential = very far out
        if q.contains("2028") { return 900.0; }

        // Default: unknown, assume moderately far
        180.0
    }

    /// Check a market for sniper opportunity (near-resolved, buy winning side cheap)
    async fn check_sniper(&mut self, client: &PolymarketClient, market: &GammaMarket) -> Option<SniperOpportunity> {
        let question = market.question.as_deref()?;
        let slug = market.slug.as_deref()?;
        let cid = market.condition_id.as_deref()?;

        // Skip already-sniped markets
        if self.sniped_markets.contains(cid) {
            return None;
        }

        // Estimate days to resolution from end_date or question content
        let days_to_resolve = Self::estimate_resolution_days(market.end_date_iso.as_deref(), question);

        // Skip markets too far out (2028 presidential etc.)
        if days_to_resolve > SNIPER_MAX_DAYS_TO_RESOLVE {
            return None;
        }

        // Parse prices
        let prices: Vec<f64> = market.outcome_prices
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())?;
        if prices.len() < 2 {
            return None;
        }

        // Parse volume
        let volume = match &market.volume {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(serde_json::Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        };
        if volume < SNIPER_MIN_VOLUME {
            return None;
        }

        // Parse token IDs
        let tokens: Vec<String> = market.clob_token_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())?;
        if tokens.len() < 2 {
            return None;
        }

        let yes_price = prices[0];
        let no_price = prices[1];

        // Determine which side is near-resolved
        let (side, mid_price, token_idx) = if yes_price >= SNIPER_MIN_PRICE {
            ("YES", yes_price, 0)
        } else if no_price >= SNIPER_MIN_PRICE {
            ("NO", no_price, 1)
        } else {
            return None;
        };

        // Fetch tick size (cached to avoid API call per candidate per cycle)
        let tick_size = if let Some(&cached) = self.tick_size_cache.get(cid) {
            cached
        } else {
            let ts = client.get_tick_size(cid).await.unwrap_or(0.01);
            self.tick_size_cache.insert(cid.to_string(), ts);
            ts
        };
        // Max price depends on tick size: 0.001 tick -> max 0.999, 0.01 tick -> max 0.99
        let effective_max = if tick_size <= 0.001 { SNIPER_MAX_PRICE } else { 0.99 };

        // Check order book for actual ask price
        let book = client.get_order_book(&tokens[token_idx]).await.ok()?;
        let ask_price = book.asks.first().map(|a| a.price)?;

        // Must be within our buy range
        if ask_price < SNIPER_MIN_PRICE || ask_price > effective_max {
            return None;
        }

        let expected_profit_pct = (1.0 - ask_price) / ask_price;

        // Score: profit% / sqrt(days) — favors high profit AND fast resolution
        // A 5% trade resolving in 1 day scores 5.0
        // A 0.1% trade resolving in 365 days scores 0.005
        let score = expected_profit_pct * 100.0 / (days_to_resolve.max(0.1)).sqrt();

        Some(SniperOpportunity {
            condition_id: cid.to_string(),
            question: question.to_string(),
            slug: slug.to_string(),
            token_id: tokens[token_idx].clone(),
            side: side.to_string(),
            ask_price,
            mid_price,
            expected_profit_pct,
            neg_risk: market.neg_risk.unwrap_or(true),
            volume,
            tick_size,
            days_to_resolve,
            score,
        })
    }

    /// Execute a sniper trade: buy the near-certain winning side
    async fn execute_sniper(&mut self, opp: &SniperOpportunity) -> Result<()> {
        // Max exposure check (dynamic based on balance)
        let remaining = self.max_sniper_exposure - self.sniper_committed;
        if remaining < 5.0 {
            info!("Sniper exposure limit reached (${:.0} committed / ${:.0} limit) - skipping", self.sniper_committed, self.max_sniper_exposure);
            return Ok(());
        }
        let trade_size = SNIPER_MAX_SIZE.min(remaining);

        let shares = trade_size / opp.ask_price;
        let shares = (shares * 100.0).floor() / 100.0;

        if shares < 1.0 {
            warn!("Sniper shares too small: {:.2}", shares);
            return Ok(());
        }

        let cost = shares * opp.ask_price;
        let expected_payout = shares * 1.0;
        let expected_profit = expected_payout - cost;

        println!("  >> Executing sniper:");
        println!("     BUY {:.2} {} @ ${:.4} = ${:.2}", shares, opp.side, opp.ask_price, cost);
        println!("     Expected payout: ${:.2} | Expected profit: ${:.2} ({:.1}%)",
            expected_payout, expected_profit, opp.expected_profit_pct * 100.0);

        if self.dry_run {
            println!("     (DRY RUN - not executing)\n");
            return Ok(());
        }

        let client = PolymarketClient::new()?;
        match orders::place_order_with_tick(&client, &opp.token_id, orders::Side::Buy, opp.ask_price, shares, opp.neg_risk, false, opp.tick_size).await {
            Ok(_) => {
                info!("Sniper order placed: {} {} @ ${:.4}", opp.side, shares, opp.ask_price);
                self.sniper_trades += 1;
                self.sniper_profit += expected_profit;
                self.sniper_committed += cost;

                // Track to avoid re-sniping
                self.sniped_markets.insert(opp.condition_id.clone());

                let resolve_str = if opp.days_to_resolve < 1.0 {
                    format!("{:.0}h", opp.days_to_resolve * 24.0)
                } else if opp.days_to_resolve < 30.0 {
                    format!("{:.0} days", opp.days_to_resolve)
                } else {
                    format!("{:.0} months", opp.days_to_resolve / 30.0)
                };
                let msg = format!(
                    "Sniper Trade #{}\n\n\"{}\"\n\nBUY {:.2} {} @ ${:.4} = ${:.2}\nProfit: ${:.2} ({:.1}%)\nResolves in: ~{}\nScore: {:.2}",
                    self.sniper_trades,
                    truncate(&opp.question, 60),
                    shares, opp.side, opp.ask_price, cost,
                    expected_profit, opp.expected_profit_pct * 100.0,
                    resolve_str, opp.score
                );
                self.notifier.send(&msg).await;
            }
            Err(e) => {
                let msg = e.to_string();
                error!("Sniper order failed: {}", msg);
                // Don't spam Telegram for balance errors — expected when fully invested
                if !msg.contains("not enough balance") {
                    self.notifier.notify_error("Sniper order", &msg).await;
                } else {
                    // We're fully invested — set committed to max to stop retrying
                    self.sniper_committed = self.max_sniper_exposure;
                }
                return Err(e);
            }
        }

        Ok(())
    }

    /// Send hourly portfolio summary to Telegram
    async fn send_portfolio_summary(&self) {
        // Fetch open orders count via simple HTTP (no auth needed for portfolio file)
        let portfolio_path = "portfolio_state.json";
        let summary = match std::fs::read_to_string(portfolio_path) {
            Ok(data) => {
                match serde_json::from_str::<serde_json::Value>(&data) {
                    Ok(state) => {
                        let positions = state.get("positions")
                            .and_then(|p| p.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let total_invested: f64 = state.get("positions")
                            .and_then(|p| p.as_array())
                            .map(|arr| arr.iter()
                                .filter_map(|p| p.get("cost_basis").and_then(|v| v.as_f64()))
                                .sum())
                            .unwrap_or(0.0);
                        let resolved = state.get("resolved")
                            .and_then(|r| r.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        let realized_pnl: f64 = state.get("resolved")
                            .and_then(|r| r.as_array())
                            .map(|arr| arr.iter()
                                .filter_map(|p| p.get("pnl").and_then(|v| v.as_f64()))
                                .sum())
                            .unwrap_or(0.0);
                        format!(
                            "Portfolio Summary\n\nOpen positions: {}\nTotal invested: ${:.2}\nResolved: {}\nRealized P/L: ${:.2}\n\nSniper stats (this session):\nTrades: {} | Committed: ${:.0} / ${:.0}\nSniped markets: {}",
                            positions, total_invested, resolved, realized_pnl,
                            self.sniper_trades, self.sniper_committed, self.max_sniper_exposure,
                            self.sniped_markets.len()
                        )
                    }
                    Err(_) => format!("Portfolio Summary\n\nSniper trades: {} | Committed: ${:.0}", self.sniper_trades, self.sniper_committed)
                }
            }
            Err(_) => format!("Portfolio Summary\n\nSniper trades: {} | Committed: ${:.0}", self.sniper_trades, self.sniper_committed)
        };
        self.notifier.send(&summary).await;
    }

    /// Run one scan cycle
    async fn run_cycle(&mut self) -> Result<()> {
        self.cycle_count += 1;
        let markets = self.fetch_markets().await?;
        println!("Scanned {} markets", markets.len());

        let client = PolymarketClient::new()?;

        // Fetch real balance to set dynamic exposure limit
        match client.get_balance().await {
            Ok(balance) => {
                let new_limit = (balance - SNIPER_RESERVE_BUFFER).max(0.0);
                if (new_limit - self.max_sniper_exposure).abs() > 1.0 {
                    info!("Updated sniper exposure limit: ${:.2} -> ${:.2} (balance: ${:.2})",
                        self.max_sniper_exposure, new_limit, balance);
                }
                self.max_sniper_exposure = new_limit;
                // Reset committed tracker if balance suggests orders filled/expired
                if self.sniper_committed > new_limit {
                    self.sniper_committed = 0.0;
                }
            }
            Err(e) => {
                // Use previous limit on error (don't spam logs — balance errors expected when fully invested)
                if self.cycle_count <= 1 {
                    warn!("Failed to fetch balance (using default ${:.0}): {}", self.max_sniper_exposure, e);
                }
            }
        }
        let mut opportunities = Vec::new();

        // Pre-filter markets that might have arb (Gamma mid prices suggest YES+NO < 0.99)
        let candidates: Vec<&GammaMarket> = markets.iter()
            .filter(|m| {
                let prices: Vec<f64> = m.outcome_prices
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())
                    .unwrap_or_default();
                prices.len() >= 2 && prices[0] + prices[1] < 0.99
            })
            .collect();

        if !candidates.is_empty() {
            println!("  {} markets with mid-price spread > 1% - checking order books...", candidates.len());
        }

        for market in &candidates {
            if let Some(opp) = self.check_arb(&client, market).await {
                println!(
                    "  >> ARB FOUND: \"{}\" | YES ${:.4} + NO ${:.4} = ${:.4} | Spread: {:.2}%",
                    truncate(&opp.question, 50),
                    opp.yes_ask, opp.no_ask, opp.yes_ask + opp.no_ask,
                    opp.spread * 100.0
                );
                opportunities.push(opp);
            }
            // Small delay to avoid rate limiting
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if !opportunities.is_empty() {
            // Sort by spread (biggest profit first)
            opportunities.sort_by(|a, b| b.spread.partial_cmp(&a.spread).unwrap());

            // Execute best opportunities
            for opp in &opportunities {
                if let Err(e) = self.execute_arb(opp).await {
                    error!("Arb execution failed: {}", e);
                }
            }
        }

        // --- Sniper: check for near-resolved markets ---
        let sniper_candidates: Vec<&GammaMarket> = markets.iter()
            .filter(|m| {
                let prices: Vec<f64> = m.outcome_prices
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                    .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())
                    .unwrap_or_default();
                prices.len() >= 2 && (prices[0] >= SNIPER_MIN_PRICE || prices[1] >= SNIPER_MIN_PRICE)
            })
            .collect();

        if !sniper_candidates.is_empty() {
            let mut sniper_opps = Vec::new();
            for market in &sniper_candidates {
                if let Some(opp) = self.check_sniper(&client, market).await {
                    println!(
                        "  >> SNIPER: \"{}\" | {} @ ${:.4} | +{:.1}% | {:.0}d | score:{:.2}",
                        truncate(&opp.question, 45),
                        opp.side, opp.ask_price,
                        opp.expected_profit_pct * 100.0,
                        opp.days_to_resolve,
                        opp.score
                    );
                    sniper_opps.push(opp);
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }

            // Sort by score descending (profit% / sqrt(days) — favors fast + profitable)
            sniper_opps.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

            // Execute top sniper opportunities
            for opp in sniper_opps.iter().take(3) {
                if let Err(e) = self.execute_sniper(opp).await {
                    error!("Sniper execution failed: {}", e);
                }
            }

            println!("  Sniper: {} trades placed (${:.0} committed / ${:.0} limit) | {} candidates found",
                self.sniper_trades, self.sniper_committed, self.max_sniper_exposure, sniper_opps.len());
        }

        // Hourly portfolio summary (~120 cycles at 30s = 1 hour)
        if self.cycle_count - self.last_summary_cycle >= 120 {
            self.last_summary_cycle = self.cycle_count;
            self.send_portfolio_summary().await;
        }

        Ok(())
    }

    /// Main loop
    pub async fn run(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        println!("\n== Polymarket Arb + Sniper Scanner - {} ==", mode);
        println!("   Scan interval: {}s", SCAN_INTERVAL_SECS);
        println!("   Arb: min spread {:.1}% | max ${:.0}/side", MIN_PROFIT_PCT * 100.0, MAX_ARB_SIZE);
        println!("   Sniper: buy {:.0}-{:.0}% certainty | max ${:.0} | min vol ${:.0}K\n",
            SNIPER_MIN_PRICE * 100.0, SNIPER_MAX_PRICE * 100.0, SNIPER_MAX_SIZE, SNIPER_MIN_VOLUME / 1000.0);

        let startup_msg = format!(
            "Arb + Sniper Scanner Started ({})\nInterval: {}s\nArb: min {:.1}% spread, ${:.0}/side\nSniper: buy {:.0}-{:.0}% certainty, ${:.0} max, ${:.0}K min vol",
            mode, SCAN_INTERVAL_SECS, MIN_PROFIT_PCT * 100.0, MAX_ARB_SIZE,
            SNIPER_MIN_PRICE * 100.0, SNIPER_MAX_PRICE * 100.0, SNIPER_MAX_SIZE, SNIPER_MIN_VOLUME / 1000.0
        );
        self.notifier.send(&startup_msg).await;

        loop {
            match self.run_cycle().await {
                Ok(_) => {},
                Err(e) => {
                    error!("Arb scan error: {}", e);
                    println!("Scan error: {}. Retrying...", e);
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(SCAN_INTERVAL_SECS)).await;
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = (0..=max.saturating_sub(3))
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        format!("{}...", &s[..end])
    }
}
