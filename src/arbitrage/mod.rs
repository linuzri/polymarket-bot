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

    /// Run one scan cycle
    async fn run_cycle(&mut self) -> Result<()> {
        let markets = self.fetch_markets().await?;
        println!("Scanned {} markets", markets.len());

        let client = PolymarketClient::new()?;
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

        if opportunities.is_empty() {
            // Silent — no arb found is normal
            return Ok(());
        }

        // Sort by spread (biggest profit first)
        opportunities.sort_by(|a, b| b.spread.partial_cmp(&a.spread).unwrap());

        // Execute best opportunities
        for opp in &opportunities {
            if let Err(e) = self.execute_arb(opp).await {
                error!("Arb execution failed: {}", e);
            }
        }

        Ok(())
    }

    /// Main loop
    pub async fn run(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        println!("\n== Polymarket Arbitrage Scanner - {} ==", mode);
        println!("   Scan interval: {}s | Min spread: {:.1}% | Max size: ${:.0}/side",
            SCAN_INTERVAL_SECS, MIN_PROFIT_PCT * 100.0, MAX_ARB_SIZE);
        println!("   Looking for YES + NO < ${:.3}...\n", 1.0 - MIN_PROFIT_PCT);

        let startup_msg = format!(
            "Arb Scanner Started ({})\nInterval: {}s | Min spread: {:.1}% | Max: ${:.0}/side",
            mode, SCAN_INTERVAL_SECS, MIN_PROFIT_PCT * 100.0, MAX_ARB_SIZE
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
