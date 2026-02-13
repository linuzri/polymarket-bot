use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn, error};
use uuid::Uuid;

use super::ai_evaluator::AiEvaluator;
use super::config::AutoSellConfig;
use super::evaluator::{Evaluator, SignalSide};
use super::logger::{TradeEntry, TradeLog};
use super::risk::RiskManager;
use super::scanner::MarketScanner;
use super::config::StrategyConfig;
use crate::notifications::TelegramNotifier;
use crate::portfolio::PortfolioState;

pub struct StrategyEngine {
    config: StrategyConfig,
    scanner: MarketScanner,
    evaluator: Evaluator,
    ai_evaluator: Option<AiEvaluator>,
    risk_manager: RiskManager,
    trade_log: TradeLog,
    dry_run: bool,
    notifier: TelegramNotifier,
}

impl StrategyEngine {
    pub fn new(config: StrategyConfig, dry_run_override: bool) -> Result<Self> {
        let dry_run = dry_run_override || config.dry_run;
        let risk = config.risk.clone();

        // Try to create AI evaluator if enabled and API key available
        let ai_evaluator = if config.ai_evaluator.enabled {
            match std::env::var("ANTHROPIC_API_KEY") {
                Ok(key) if !key.is_empty() => {
                    info!("🧠 AI evaluator enabled (model: {})", config.ai_evaluator.model);
                    println!("🧠 AI evaluator enabled (model: {})", config.ai_evaluator.model);
                    Some(AiEvaluator::new(key, risk.min_edge, config.ai_evaluator.clone()))
                }
                _ => {
                    warn!("⚠️  ANTHROPIC_API_KEY not set — falling back to heuristic evaluator");
                    println!("⚠️  ANTHROPIC_API_KEY not set — falling back to heuristic evaluator");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            scanner: MarketScanner::new(risk.min_volume, risk.min_hours_to_close),
            evaluator: Evaluator::new(risk.min_edge),
            ai_evaluator,
            risk_manager: RiskManager::new(risk),
            trade_log: TradeLog::load()?,
            dry_run,
            config,
            notifier: TelegramNotifier::new(),
        })
    }

    /// Check portfolio for resolutions and send alerts
    async fn check_portfolio_resolutions(&self) -> Result<()> {
        let mut state = crate::portfolio::PortfolioState::load()?;
        crate::portfolio::sync_from_trade_log(&mut state)?;
        crate::portfolio::update_prices(&mut state, &crate::api::client::PolymarketClient::new()?).await?;
        let resolved = crate::portfolio::check_resolutions(&mut state, &crate::api::client::PolymarketClient::new()?).await?;
        if !resolved.is_empty() {
            info!("Detected {} resolved market(s)", resolved.len());
            crate::portfolio::alert_resolutions(&resolved, &self.notifier).await;
        }
        state.save()?;
        Ok(())
    }

    /// Check open positions for auto-sell triggers (take profit, stop loss)
    async fn check_auto_sell(&self) -> Result<()> {
        let auto_sell = &self.config.auto_sell;
        if !auto_sell.enabled {
            return Ok(());
        }

        let client = crate::api::client::PolymarketClient::new()?;
        let mut state = PortfolioState::load()?;
        crate::portfolio::sync_from_trade_log(&mut state)?;

        if state.positions.is_empty() {
            return Ok(());
        }

        // Update current prices first
        crate::portfolio::update_prices(&mut state, &client).await?;

        let take_profit_pct = auto_sell.take_profit_pct;
        let stop_loss_pct = auto_sell.stop_loss_pct;

        // Collect positions to sell (can't borrow mutably while iterating)
        let mut to_sell: Vec<(String, String, f64, f64, String)> = Vec::new(); // (key, reason, sell_price, shares, side)

        for (key, pos) in &state.positions {
            let entry = pos.avg_entry_price;
            let current = pos.current_price;

            // Take profit check
            let tp_target = entry + (take_profit_pct * entry);
            if current >= tp_target {
                let reason = format!(
                    "Take profit: price {:.4} >= target {:.4} ({:.0}% gain)",
                    current, tp_target, ((current - entry) / entry) * 100.0
                );
                info!("AUTO-SELL [TP]: {} - {}", pos.market_question, reason);
                to_sell.push((key.clone(), reason, current, pos.shares, pos.side.clone()));
                continue;
            }

            // Stop loss check
            let sl_target = entry - (stop_loss_pct * entry);
            if current <= sl_target {
                let reason = format!(
                    "Stop loss: price {:.4} <= target {:.4} ({:.0}% loss)",
                    current, sl_target, ((entry - current) / entry) * 100.0
                );
                info!("AUTO-SELL [SL]: {} - {}", pos.market_question, reason);
                to_sell.push((key.clone(), reason, current, pos.shares, pos.side.clone()));
                continue;
            }
        }

        if to_sell.is_empty() {
            return Ok(());
        }

        println!("\n  --- AUTO-SELL CHECK ---");
        println!("  Found {} position(s) to sell\n", to_sell.len());

        for (key, reason, sell_price, shares, side) in &to_sell {
            let pos = match state.positions.get(key) {
                Some(p) => p.clone(),
                None => continue,
            };

            // Get token ID for the sell order
            let token_id = if let Some(ref slug) = Some(&pos.market_slug).filter(|s| !s.is_empty()) {
                match client.get_market(slug).await {
                    Ok(market) => {
                        let tokens = market.tokens.as_ref();
                        let idx = if pos.side == "YES" { 0 } else { 1 };
                        tokens.and_then(|t| t.get(idx).cloned())
                    }
                    Err(e) => {
                        warn!("Failed to get market for sell: {}", e);
                        None
                    }
                }
            } else {
                None
            };

            let Some(token_id) = token_id else {
                warn!("Cannot sell {}: no token ID found", pos.market_question);
                continue;
            };

            // Get best bid price from order book
            let actual_sell_price = match client.get_order_book(&token_id).await {
                Ok(book) => book.bids.first().map(|b| b.price).unwrap_or(*sell_price),
                Err(_) => *sell_price,
            };

            let neg_risk = client.get_neg_risk(&pos.market_slug).await.unwrap_or(true);
            let pnl = (actual_sell_price - pos.avg_entry_price) * shares;

            println!("  SELL {} \"{}\"", side, pos.market_question);
            println!("     Entry: ${:.4} -> Sell: ${:.4} | {:.2} shares | P/L: ${:.2}", pos.avg_entry_price, actual_sell_price, shares, pnl);
            println!("     Reason: {}", reason);

            if !self.dry_run {
                match crate::orders::place_order(&client, &token_id, crate::orders::Side::Sell, actual_sell_price, *shares, neg_risk, false).await {
                    Ok(_) => {
                        info!("Auto-sell executed for {}", pos.market_question);

                        // Remove from portfolio
                        state.positions.remove(key);

                        // Also remove from trade log open_positions
                        self.trade_log_remove_position(&pos.condition_id);

                        // Add to resolved
                        state.resolved.push(crate::portfolio::ResolvedPosition {
                            condition_id: pos.condition_id.clone(),
                            token_id: key.clone(),
                            market_question: pos.market_question.clone(),
                            side: pos.side.clone(),
                            shares: *shares,
                            cost_basis: pos.cost_basis,
                            avg_entry_price: pos.avg_entry_price,
                            resolution_price: actual_sell_price,
                            realized_pnl: pnl,
                            opened_at: pos.opened_at,
                            resolved_at: Utc::now(),
                            outcome: if pnl >= 0.0 { "SOLD-PROFIT".to_string() } else { "SOLD-LOSS".to_string() },
                        });

                        self.notifier.notify_sell(
                            &pos.market_question, &pos.side,
                            pos.avg_entry_price, actual_sell_price, *shares, pnl,
                            reason, false,
                        ).await;
                    }
                    Err(e) => {
                        error!("Auto-sell failed for {}: {}", pos.market_question, e);
                        self.notifier.notify_error(
                            &format!("Auto-sell {}", pos.market_question),
                            &e.to_string(),
                        ).await;
                    }
                }
            } else {
                // Dry run — just notify
                println!("     (dry run - not executing)\n");
                self.notifier.notify_sell(
                    &pos.market_question, &pos.side,
                    pos.avg_entry_price, actual_sell_price, *shares, pnl,
                    reason, true,
                ).await;
            }
        }

        state.last_updated = Utc::now();
        state.save()?;
        Ok(())
    }

    /// Remove a position from the trade log's open_positions tracker
    fn trade_log_remove_position(&self, condition_id: &str) {
        if let Ok(mut log) = TradeLog::load() {
            log.open_positions.remove(condition_id);
            // Mark matching trades as closed
            for trade in &mut log.trades {
                if trade.condition_id == condition_id && !trade.closed {
                    trade.closed = true;
                }
            }
            let _ = log.save();
        }
    }

    /// Run one cycle of the strategy
    pub async fn run_cycle(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        println!("\n{}", "=".repeat(60));
        println!("🤖 Strategy Engine Cycle — {} | {}", mode, Utc::now().format("%Y-%m-%d %H:%M:%S UTC"));
        println!("{}", "=".repeat(60));

        // 1. Scan markets
        let candidates = self.scanner.scan(100).await?;
        if candidates.is_empty() {
            println!("   No candidates found. Sleeping...");
            return Ok(());
        }

        // 2. Evaluate each candidate
        let mut signals = if let Some(ref ai) = self.ai_evaluator {
            println!("🧠 Running AI evaluation on up to {} markets...\n", candidates.len().min(20));
            ai.evaluate_batch(&candidates).await
        } else {
            let mut sigs = Vec::new();
            for candidate in &candidates {
                if let Some(signal) = self.evaluator.evaluate(candidate) {
                    sigs.push(signal);
                }
            }
            sigs
        };

        println!("📊 Signals: {} markets with potential edge\n", signals.len());

        if signals.is_empty() {
            println!("   No edge detected this cycle.");
            return Ok(());
        }

        // Sort: fast-resolving markets first, then by edge descending
        let now = Utc::now();
        signals.sort_by(|a, b| {
            let hours_a = a.market.end_date.map(|d| (d - now).num_hours()).unwrap_or(9999);
            let hours_b = b.market.end_date.map(|d| (d - now).num_hours()).unwrap_or(9999);
            let fast_a = hours_a < 48;
            let fast_b = hours_b < 48;
            match (fast_a, fast_b) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => b.edge.partial_cmp(&a.edge).unwrap_or(std::cmp::Ordering::Equal),
            }
        });

        // 3. Get bankroll
        let bankroll = if self.dry_run {
            1000.0 // simulated
        } else {
            // In live mode, we'd fetch real balance
            // For now, use config or default
            99.0
        };

        // 4. Risk check and size each trade
        for signal in &signals {
            let icon = match signal.side {
                SignalSide::Yes => "📈",
                SignalSide::No => "📉",
            };

            let market_price = match signal.side {
                SignalSide::Yes => signal.market.yes_price,
                SignalSide::No => signal.market.no_price,
            };

            if let Some(sized) = self.risk_manager.check(signal, bankroll, &self.trade_log) {
                let shares = sized.size_usd / sized.price;
                let action_label = if self.dry_run { "(dry run)" } else { "→ EXECUTING" };

                println!("  {} \"{}\"", icon, signal.market.question);
                println!("     {} at ${:.2} | Our est: {:.0}% | Edge: {:.0}% | Confidence: {:.0}%",
                    signal.side, market_price, signal.estimated_probability * 100.0,
                    signal.edge * 100.0, signal.confidence * 100.0);
                println!("     Size: ${:.2} ({:.2} shares) | Reason: {}",
                    sized.size_usd, shares, signal.reason);
                println!("     Action: BUY {} {}\n", signal.side, action_label);

                // Send signal notification
                self.notifier.notify_signal(
                    &signal.market.question,
                    &signal.side.to_string(),
                    signal.edge,
                    signal.confidence,
                    sized.size_usd,
                    &signal.reason,
                ).await;

                // Execute or log
                if !self.dry_run {
                    match self.execute_trade(&sized.token_id, sized.price, shares, signal.market.neg_risk).await {
                        Ok(_) => {
                            info!("Trade executed for {}", signal.market.slug);
                            self.notifier.notify_trade(
                                &signal.market.question,
                                &signal.side.to_string(),
                                sized.price, sized.size_usd, shares, false,
                            ).await;
                        }
                        Err(e) => {
                            error!("Trade failed for {}: {}", signal.market.slug, e);
                            self.notifier.notify_error(
                                &format!("Trade for {}", signal.market.slug),
                                &e.to_string(),
                            ).await;
                        }
                    }
                } else {
                    self.notifier.notify_trade(
                        &signal.market.question,
                        &signal.side.to_string(),
                        sized.price, sized.size_usd, shares, true,
                    ).await;
                }

                // Log the trade
                let entry = TradeEntry {
                    id: Uuid::new_v4().to_string(),
                    timestamp: Utc::now(),
                    condition_id: signal.market.condition_id.clone(),
                    market_slug: signal.market.slug.clone(),
                    market_question: signal.market.question.clone(),
                    side: signal.side.to_string(),
                    action: "BUY".to_string(),
                    price: sized.price,
                    size_usd: sized.size_usd,
                    shares,
                    edge: signal.edge,
                    confidence: signal.confidence,
                    reason: signal.reason.clone(),
                    dry_run: self.dry_run,
                    pnl: None,
                    closed: false,
                };
                self.trade_log.log_trade(entry)?;
            } else {
                // Signal rejected by risk manager — show briefly
                println!("  ⏭️  \"{}\" — {} edge {:.0}% (rejected by risk limits)",
                    truncate(&signal.market.question, 50), signal.side, signal.edge * 100.0);
            }
        }

        // Summary
        println!("\n  Portfolio: {} open positions | ${:.2} exposure",
            self.trade_log.open_position_count(), self.trade_log.total_exposure());

        // Check for resolved markets in portfolio
        if let Err(e) = self.check_portfolio_resolutions().await {
            warn!("Portfolio resolution check failed: {}", e);
        }

        // Check open positions for auto-sell triggers
        if let Err(e) = self.check_auto_sell().await {
            warn!("Auto-sell check failed: {}", e);
        }

        Ok(())
    }

    /// Execute a real trade
    async fn execute_trade(&self, token_id: &str, price: f64, size: f64, neg_risk: bool) -> Result<()> {
        // Use the existing order infrastructure
        let client = crate::api::client::PolymarketClient::new()?;
        crate::orders::place_order(&client, token_id, crate::orders::Side::Buy, price, size, neg_risk, false).await?;
        Ok(())
    }

    /// Main loop
    pub async fn run(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "⚠️  LIVE TRADING" };
        println!("\n🚀 Polymarket Strategy Engine Starting — {}", mode);
        println!("   Scan interval: {}s | Max trade: ${:.2} | Max exposure: ${:.2}",
            self.config.scan_interval_secs, self.config.risk.max_trade_size, self.config.risk.max_total_exposure);
        println!("   Min edge: {:.0}% | Kelly fraction: {:.0}%\n",
            self.config.risk.min_edge * 100.0, self.config.risk.kelly_fraction * 100.0);

        loop {
            match self.run_cycle().await {
                Ok(_) => {},
                Err(e) => {
                    error!("Strategy cycle error: {}", e);
                    println!("Cycle error: {}. Retrying...", e);
                    self.notifier.notify_error("Strategy cycle", &e.to_string()).await;
                }
            }

            println!("\n⏳ Sleeping {}s until next scan...\n", self.config.scan_interval_secs);
            tokio::time::sleep(std::time::Duration::from_secs(self.config.scan_interval_secs)).await;
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else { format!("{}...", &s[..max.saturating_sub(3)]) }
}
