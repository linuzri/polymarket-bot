use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::{info, warn};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;

const STATE_FILE: &str = "portfolio_state.json";

/// A single tracked position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub condition_id: String,
    pub token_id: String,
    pub market_slug: String,
    pub market_question: String,
    pub side: String,           // "YES" or "NO"
    pub shares: f64,
    pub cost_basis: f64,        // total USD spent
    pub avg_entry_price: f64,
    pub current_price: f64,
    pub opened_at: DateTime<Utc>,
}

impl Position {
    pub fn unrealized_pnl(&self) -> f64 {
        (self.current_price - self.avg_entry_price) * self.shares
    }

    pub fn current_value(&self) -> f64 {
        self.current_price * self.shares
    }
}

/// A resolved (closed) position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPosition {
    pub condition_id: String,
    pub token_id: String,
    pub market_question: String,
    pub side: String,
    pub shares: f64,
    pub cost_basis: f64,
    pub avg_entry_price: f64,
    pub resolution_price: f64,  // 0.0 or 1.0 typically
    pub realized_pnl: f64,
    pub opened_at: DateTime<Utc>,
    pub resolved_at: DateTime<Utc>,
    pub outcome: String,        // "WON" or "LOST"
}

/// Persisted portfolio state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioState {
    pub positions: HashMap<String, Position>,        // keyed by token_id
    pub resolved: Vec<ResolvedPosition>,
    #[serde(default)]
    pub alerted_resolutions: Vec<String>,            // condition_ids already alerted
    #[serde(default)]
    pub synced_trade_ids: Vec<String>,               // trade IDs already synced to portfolio
    pub last_updated: DateTime<Utc>,
}

impl PortfolioState {
    pub fn load() -> Result<Self> {
        let path = Path::new(STATE_FILE);
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .context("Failed to read portfolio state")?;
            serde_json::from_str(&data)
                .context("Failed to parse portfolio state")
        } else {
            Ok(Self {
                positions: HashMap::new(),
                resolved: Vec::new(),
                alerted_resolutions: Vec::new(),
                synced_trade_ids: Vec::new(),
                last_updated: Utc::now(),
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(self)
            .context("Failed to serialize portfolio state")?;
        std::fs::write(STATE_FILE, data)
            .context("Failed to write portfolio state")?;
        Ok(())
    }

    pub fn total_invested(&self) -> f64 {
        self.positions.values().map(|p| p.cost_basis).sum()
    }

    pub fn total_current_value(&self) -> f64 {
        self.positions.values().map(|p| p.current_value()).sum()
    }

    pub fn total_unrealized_pnl(&self) -> f64 {
        self.positions.values().map(|p| p.unrealized_pnl()).sum()
    }

    pub fn total_realized_pnl(&self) -> f64 {
        self.resolved.iter().map(|r| r.realized_pnl).sum()
    }
}

/// Sync positions from the strategy trade log into portfolio state.
/// This picks up trades that the engine made.
pub fn sync_from_trade_log(state: &mut PortfolioState) -> Result<()> {
    let log_path = Path::new("strategy_trades.json");
    if !log_path.exists() {
        return Ok(());
    }

    let data = std::fs::read_to_string(log_path)?;
    let log: serde_json::Value = serde_json::from_str(&data)?;

    let trades = log.get("trades").and_then(|t| t.as_array());
    let Some(trades) = trades else { return Ok(()) };

    for trade in trades {
        let dry_run = trade.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(true);
        if dry_run {
            continue;
        }
        let closed = trade.get("closed").and_then(|v| v.as_bool()).unwrap_or(false);
        if closed {
            continue;
        }

        // Skip trades already synced
        let trade_id = trade.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if !trade_id.is_empty() && state.synced_trade_ids.contains(&trade_id.to_string()) {
            continue;
        }

        let condition_id = trade.get("condition_id").and_then(|v| v.as_str()).unwrap_or_default();
        let market_slug = trade.get("market_slug").and_then(|v| v.as_str()).unwrap_or_default();
        let market_question = trade.get("market_question").and_then(|v| v.as_str()).unwrap_or_default();
        let side = trade.get("side").and_then(|v| v.as_str()).unwrap_or_default();
        let price = trade.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let shares = trade.get("shares").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let size_usd = trade.get("size_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let timestamp = trade.get("timestamp").and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
            .unwrap_or_else(Utc::now);

        if condition_id.is_empty() || shares <= 0.0 {
            continue;
        }

        // Use condition_id as key since we may not have token_id in trade log
        let key = format!("{}_{}", condition_id, side.to_lowercase());

        // Track this trade as synced
        if !trade_id.is_empty() {
            state.synced_trade_ids.push(trade_id.to_string());
        }

        if state.positions.contains_key(&key) {
            // Update existing position (add shares from NEW trade)
            let pos = state.positions.get_mut(&key).unwrap();
            let total_cost = pos.cost_basis + size_usd;
            let total_shares = pos.shares + shares;
            pos.avg_entry_price = total_cost / total_shares;
            pos.shares = total_shares;
            pos.cost_basis = total_cost;
        } else if !state.alerted_resolutions.contains(&key) {
            // New position
            state.positions.insert(key.clone(), Position {
                condition_id: condition_id.to_string(),
                token_id: key.clone(),
                market_slug: market_slug.to_string(),
                market_question: market_question.to_string(),
                side: side.to_uppercase(),
                shares,
                cost_basis: size_usd,
                avg_entry_price: price,
                current_price: price,
                opened_at: timestamp,
            });
        }
    }

    Ok(())
}

/// Fetch current prices for all open positions
pub async fn update_prices(state: &mut PortfolioState, client: &PolymarketClient) -> Result<()> {
    for pos in state.positions.values_mut() {
        // Try to get market info via slug
        if let Some(ref slug) = Some(&pos.market_slug).filter(|s| !s.is_empty()) {
            match client.get_market(slug).await {
                Ok(market) => {
                    let price = match pos.side.as_str() {
                        "YES" => market.yes_price,
                        "NO" => market.no_price,
                        _ => market.yes_price,
                    };
                    pos.current_price = price;
                }
                Err(e) => {
                    warn!("Failed to fetch price for {}: {}", slug, e);
                }
            }
        }
    }
    state.last_updated = Utc::now();
    Ok(())
}

/// Check for resolved markets and move them from positions to resolved.
/// Returns list of newly resolved positions for alerting.
pub async fn check_resolutions(
    state: &mut PortfolioState,
    client: &PolymarketClient,
) -> Result<Vec<ResolvedPosition>> {
    let mut newly_resolved = Vec::new();
    let mut to_remove = Vec::new();

    for (key, pos) in &state.positions {
        if state.alerted_resolutions.contains(key) {
            continue;
        }

        if let Some(ref slug) = Some(&pos.market_slug).filter(|s| !s.is_empty()) {
            // Fetch from Gamma API to check closed status
            let url = format!("https://gamma-api.polymarket.com/markets?slug={}", slug);
            let http = reqwest::Client::new();
            match http.get(&url).send().await {
                Ok(resp) => {
                    if let Ok(markets) = resp.json::<Vec<serde_json::Value>>().await {
                        if let Some(market) = markets.first() {
                            let is_closed = market.get("closed")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);

                            if is_closed {
                                // Determine resolution price
                                let resolution_price = if let Some(prices_str) = market.get("outcomePrices").and_then(|v| v.as_str()) {
                                    let prices: Vec<String> = serde_json::from_str(prices_str).unwrap_or_default();
                                    let idx = if pos.side == "YES" { 0 } else { 1 };
                                    prices.get(idx)
                                        .and_then(|p| p.parse::<f64>().ok())
                                        .unwrap_or(0.0)
                                } else {
                                    // If price is very close to 0 or 1, use that
                                    let p = pos.current_price;
                                    if p > 0.95 { 1.0 } else if p < 0.05 { 0.0 } else { p }
                                };

                                let realized_pnl = (resolution_price - pos.avg_entry_price) * pos.shares;
                                let outcome = if realized_pnl >= 0.0 { "WON" } else { "LOST" };

                                let resolved = ResolvedPosition {
                                    condition_id: pos.condition_id.clone(),
                                    token_id: pos.token_id.clone(),
                                    market_question: pos.market_question.clone(),
                                    side: pos.side.clone(),
                                    shares: pos.shares,
                                    cost_basis: pos.cost_basis,
                                    avg_entry_price: pos.avg_entry_price,
                                    resolution_price,
                                    realized_pnl,
                                    opened_at: pos.opened_at,
                                    resolved_at: Utc::now(),
                                    outcome: outcome.to_string(),
                                };

                                newly_resolved.push(resolved);
                                to_remove.push(key.clone());
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to check resolution for {}: {}", slug, e);
                }
            }
        }
    }

    // Move resolved positions
    for key in &to_remove {
        state.positions.remove(key);
        state.alerted_resolutions.push(key.clone());
    }
    for r in &newly_resolved {
        state.resolved.push(r.clone());
    }

    Ok(newly_resolved)
}

/// Send Telegram alerts for newly resolved positions
pub async fn alert_resolutions(resolved: &[ResolvedPosition], notifier: &TelegramNotifier) {
    for r in resolved {
        let pnl_sign = if r.realized_pnl >= 0.0 { "+" } else { "" };
        let msg = format!(
            "<b>Market Resolved - {}</b>\n\
             Market: {}\n\
             Side: {} | Entry: ${:.4} | Resolution: ${:.4}\n\
             Shares: {:.2} | Cost: ${:.2}\n\
             P/L: {}${:.2} | Outcome: {}",
            r.outcome,
            html_escape(&r.market_question),
            r.side,
            r.avg_entry_price,
            r.resolution_price,
            r.shares,
            r.cost_basis,
            pnl_sign,
            r.realized_pnl,
            r.outcome,
        );
        notifier.send(&msg).await;
    }
}

/// Print portfolio summary to stdout
pub fn print_summary(state: &PortfolioState) {
    println!("\n{}", "=".repeat(70));
    println!("  PORTFOLIO SUMMARY  |  {}", state.last_updated.format("%Y-%m-%d %H:%M UTC"));
    println!("{}", "=".repeat(70));

    // Open positions
    println!("\n  OPEN POSITIONS ({}):", state.positions.len());
    if state.positions.is_empty() {
        println!("    No open positions");
    } else {
        println!("  {:<38} {:>5} {:>7} {:>7} {:>9}",
            "Market", "Side", "Entry", "Now", "Unrl P/L");
        println!("  {}", "-".repeat(68));
        for pos in state.positions.values() {
            let pnl = pos.unrealized_pnl();
            let pnl_str = if pnl >= 0.0 {
                format!("+${:.2}", pnl)
            } else {
                format!("-${:.2}", pnl.abs())
            };
            println!("  {:<38} {:>5} {:>6.4} {:>6.4} {:>9}",
                truncate(&pos.market_question, 36),
                pos.side,
                pos.avg_entry_price,
                pos.current_price,
                pnl_str,
            );
            println!("    {:.2} shares | Cost: ${:.2} | Value: ${:.2}",
                pos.shares, pos.cost_basis, pos.current_value());
        }
    }

    // Resolved positions
    println!("\n  RESOLVED POSITIONS ({}):", state.resolved.len());
    if state.resolved.is_empty() {
        println!("    No resolved positions");
    } else {
        println!("  {:<38} {:>5} {:>7} {:>7} {:>9}",
            "Market", "Side", "Entry", "Resol", "P/L");
        println!("  {}", "-".repeat(68));
        for r in &state.resolved {
            let pnl_str = if r.realized_pnl >= 0.0 {
                format!("+${:.2}", r.realized_pnl)
            } else {
                format!("-${:.2}", r.realized_pnl.abs())
            };
            println!("  {:<38} {:>5} {:>6.4} {:>6.4} {:>9} [{}]",
                truncate(&r.market_question, 36),
                r.side,
                r.avg_entry_price,
                r.resolution_price,
                pnl_str,
                r.outcome,
            );
        }
    }

    // Totals
    let total_invested = state.total_invested();
    let total_value = state.total_current_value();
    let unrealized = state.total_unrealized_pnl();
    let realized = state.total_realized_pnl();
    let total_pnl = unrealized + realized;

    println!("\n  {}", "-".repeat(68));
    println!("  Total Invested:    ${:.2}", total_invested);
    println!("  Current Value:     ${:.2}", total_value);
    println!("  Unrealized P/L:    ${:.2}", unrealized);
    println!("  Realized P/L:      ${:.2}", realized);
    let pnl_sign = if total_pnl >= 0.0 { "+" } else { "" };
    println!("  Total P/L:         {}${:.2}", pnl_sign, total_pnl);
    println!("{}\n", "=".repeat(70));
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max.saturating_sub(3)]) }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
