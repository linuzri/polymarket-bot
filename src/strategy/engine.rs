use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn, error};
use uuid::Uuid;

use super::ai_evaluator::AiEvaluator;
use super::evaluator::{Evaluator, SignalSide};
use super::logger::{TradeEntry, TradeLog};
use super::risk::RiskManager;
use super::scanner::MarketScanner;
use super::config::StrategyConfig;
use crate::notifications::TelegramNotifier;

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
