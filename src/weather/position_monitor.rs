//! v7 Task 3: Automated position exit on price deterioration.

use anyhow::Result;
use chrono::{Datelike, TimeZone, Utc};
use tracing::{info, warn};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders::{self, Side};
use super::strategy::WeatherTrade;

/// Compute end-of-local-day (23:59 local time) for a given city, returned as UTC DateTime.
/// Falls back to UTC end-of-day if city timezone is unknown.
fn local_end_of_day_utc(date: chrono::NaiveDate, city_name: &str) -> chrono::DateTime<Utc> {
    let tz_str = match city_name {
        "nyc"           => "America/New_York",
        "chicago"       => "America/Chicago",
        "miami"         => "America/New_York",
        "atlanta"       => "America/New_York",
        "seattle"       => "America/Los_Angeles",
        "dallas"        => "America/Chicago",
        "london"        => "Europe/London",
        "seoul"         => "Asia/Seoul",
        "paris"         => "Europe/Paris",
        "toronto"       => "America/Toronto",
        "buenos-aires"  => "America/Argentina/Buenos_Aires",
        "ankara"        => "Europe/Istanbul",
        "wellington"    => "Pacific/Auckland",
        "tokyo"         => "Asia/Tokyo",
        "sydney"        => "Australia/Sydney",
        "singapore"     => "Asia/Singapore",
        "dubai"         => "Asia/Dubai",
        "berlin"        => "Europe/Berlin",
        _               => "UTC",
    };

    let tz: chrono_tz::Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let local_end = tz
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 23, 59, 0)
        .single();

    match local_end {
        Some(dt) => dt.with_timezone(&Utc),
        None => date.and_hms_opt(23, 59, 0).unwrap().and_utc(),
    }
}

const EXIT_PRICE_RATIO_THRESHOLD: f64 = 0.50;
/// Only exit within this many hours of resolution
const EXIT_HOURS_TO_RESOLUTION: f64 = 14.0;
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

        // Parse market resolution date to compute hours remaining
        let resolution_date = match &trade.market_date {
            Some(d) => match chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                Ok(date) => date,
                Err(_) => continue,
            },
            None => continue, // No market_date = old trade, skip
        };
        // Weather markets resolve end-of-local-day for each city's timezone
        let resolution_time = local_end_of_day_utc(resolution_date, &trade.city);
        let hours_to_resolution = (resolution_time - now).num_minutes() as f64 / 60.0;
        // Skip if already resolved or too far from resolution
        if hours_to_resolution < 0.0 || hours_to_resolution > EXIT_HOURS_TO_RESOLUTION {
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
                "EXIT TRIGGER: {} {} | cost={:.3} current={:.3} ratio={:.2} value=${:.2} hrs_left={:.1}",
                trade.market_question, trade.bucket_label,
                cost_per_share, current_price, price_ratio, current_value, hours_to_resolution
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
                        "Price dropped to {:.0} pct of cost ({:.1}h to resolution)",
                        price_ratio * 100.0, hours_to_resolution
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
