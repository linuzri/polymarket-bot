//! v7 Task 3: Automated position exit on price deterioration.

use anyhow::Result;
use chrono::Utc;
use tracing::{info, warn};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders::{self, Side};
use super::strategy::WeatherTrade;

const EXIT_PRICE_RATIO_THRESHOLD: f64 = 0.50;
const MIN_HOURS_OLD: f64 = 10.0;
const MIN_EXIT_VALUE_USD: f64 = 0.50;

pub async fn check_and_exit_deteriorated_positions(
    client: &PolymarketClient,
    notifier: &TelegramNotifier,
    dry_run: bool,
) -> Result<()> {
    let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => return Ok(()),
    };

    let now = Utc::now();
    let mut any_changed = false;

    for trade in all_trades.iter_mut() {
        if trade.resolved || trade.redeemed || trade.auto_exited || trade.dry_run {
            continue;
        }
        if !trade.filled && !trade.fill_confirmed {
            continue;
        }

        let token_id = match &trade.token_id {
            Some(id) if !id.is_empty() => id.clone(),
            _ => continue,
        };

        let trade_time = match trade.timestamp.parse::<chrono::DateTime<chrono::FixedOffset>>() {
            Ok(t) => t.with_timezone(&Utc),
            Err(_) => {
                match chrono::NaiveDateTime::parse_from_str(&trade.timestamp, "%Y-%m-%dT%H:%M:%S%.f") {
                    Ok(ndt) => ndt.and_utc(),
                    Err(_) => continue,
                }
            }
        };

        let hours_since_trade = (now - trade_time).num_minutes() as f64 / 60.0;
        if hours_since_trade < MIN_HOURS_OLD {
            continue;
        }

        let current_price = match client.get_price(&token_id).await {
            Ok(p) => p,
            Err(e) => {
                warn!("Position monitor: could not fetch price for {} ({}): {}",
                    trade.market_question, trade.bucket_label, e);
                continue;
            }
        };

        let cost_per_share = trade.price;
        if cost_per_share <= 0.0 {
            continue;
        }

        let price_ratio = current_price / cost_per_share;
        let current_value = current_price * trade.shares;

        let should_exit = price_ratio < EXIT_PRICE_RATIO_THRESHOLD
            && current_value >= MIN_EXIT_VALUE_USD;

        if should_exit {
            warn!(
                "EXIT TRIGGER: {} {} | cost={:.3} current={:.3} ratio={:.2} value=${:.2} hrs_old={:.1}",
                trade.market_question, trade.bucket_label,
                cost_per_share, current_price, price_ratio, current_value, hours_since_trade
            );

            if dry_run {
                info!(
                    "EXIT DRY RUN: would sell {:.2} shares of {} {} at {:.3}",
                    trade.shares, trade.market_question, trade.bucket_label, current_price
                );
                continue;
            }

            let neg_risk = trade.neg_risk;
            match orders::place_order(
                client,
                &token_id,
                Side::Sell,
                current_price,
                trade.shares,
                neg_risk,
                false,
            ).await {
                Ok(_result) => {
                    let proceeds = current_price * trade.shares;
                    let cost = trade.cost;
                    let pnl = proceeds - cost;
                    let recovery_pct = if cost > 0.0 { (proceeds / cost) * 100.0 } else { 0.0 };

                    info!(
                        "EXIT EXECUTED: {} {} | recovered {:.2} from {:.2} cost ({:.0} pct)",
                        trade.market_question, trade.bucket_label,
                        proceeds, cost, recovery_pct
                    );

                    let reason = format!(
                        "Price dropped to {:.0} pct of cost ({:.1}h old)",
                        price_ratio * 100.0, hours_since_trade
                    );
                    notifier.notify_sell(
                        &format!("{} {}", trade.market_question, trade.bucket_label),
                        "YES",
                        cost_per_share,
                        current_price,
                        trade.shares,
                        pnl,
                        &reason,
                        false,
                    ).await;

                    trade.auto_exited = true;
                    trade.resolved = true;
                    trade.outcome = Some("AUTO_EXIT".to_string());
                    trade.pnl = Some(pnl);
                    any_changed = true;
                }
                Err(e) => {
                    warn!("EXIT FAILED for {} {}: {}", trade.market_question, trade.bucket_label, e);
                }
            }
        }
    }

    if any_changed {
        if let Ok(json) = serde_json::to_string_pretty(&all_trades) {
            let _ = std::fs::write("strategy_trades.json", json);
            info!("Position monitor: saved updated trades after auto-exit(s)");
        }
    }

    Ok(())
}
