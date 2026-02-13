use serde::{Deserialize, Serialize};
use super::evaluator::{Signal, SignalSide};
use super::logger::TradeLog;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_trade_size: f64,
    pub max_positions: usize,
    pub max_total_exposure: f64,
    pub min_edge: f64,
    pub min_volume: f64,
    pub min_hours_to_close: f64,
    pub kelly_fraction: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_trade_size: 5.0,
            max_positions: 10,
            max_total_exposure: 20.0,
            min_edge: 0.10,
            min_volume: 10000.0,
            min_hours_to_close: 24.0,
            kelly_fraction: 0.25,
        }
    }
}

pub struct RiskManager {
    pub config: RiskConfig,
}

/// Result of a risk check
#[derive(Debug)]
pub struct SizedTrade {
    pub signal: Signal,
    pub size_usd: f64,
    pub token_id: String,
    pub price: f64,
}

impl RiskManager {
    pub fn new(config: RiskConfig) -> Self {
        Self { config }
    }

    /// Check a signal against risk limits and size the trade
    pub fn check(&self, signal: &Signal, bankroll: f64, trade_log: &TradeLog) -> Option<SizedTrade> {
        // Check position count
        let open_positions = trade_log.open_position_count();
        if open_positions >= self.config.max_positions {
            tracing::debug!("Risk: max positions reached ({}/{})", open_positions, self.config.max_positions);
            return None;
        }

        // Check total exposure
        let current_exposure = trade_log.total_exposure();
        if current_exposure >= self.config.max_total_exposure {
            tracing::debug!("Risk: max exposure reached (${:.2}/${:.2})", current_exposure, self.config.max_total_exposure);
            return None;
        }

        // Check if we already have a position in this market
        if trade_log.has_position(&signal.market.condition_id) {
            tracing::debug!("Risk: already have position in {}", signal.market.slug);
            return None;
        }

        // Check minimum edge
        if signal.edge < self.config.min_edge {
            return None;
        }

        // Get price and token for the signal side
        let (price, token_id) = match signal.side {
            SignalSide::Yes => (signal.market.yes_price, signal.market.yes_token_id.clone()),
            SignalSide::No => (signal.market.no_price, signal.market.no_token_id.clone()),
        };

        // Kelly criterion sizing
        // f* = (p * b - q) / b where b = (1/price - 1), p = estimated_prob, q = 1-p
        // Simplified: kelly = edge / (1 - price)
        let kelly_full = if price < 0.99 {
            signal.edge / (1.0 - price)
        } else {
            0.01 // tiny size for near-certain markets
        };
        let kelly_size = kelly_full * self.config.kelly_fraction * bankroll;

        // Cap by max trade size and remaining exposure
        let remaining_exposure = self.config.max_total_exposure - current_exposure;
        let size_usd = kelly_size
            .min(self.config.max_trade_size)
            .min(remaining_exposure)
            .min(bankroll * 0.1); // never more than 10% of bankroll

        // Minimum viable trade
        if size_usd < 0.50 {
            tracing::debug!("Risk: trade too small (${:.2})", size_usd);
            return None;
        }

        Some(SizedTrade {
            signal: signal.clone(),
            size_usd,
            token_id,
            price,
        })
    }
}
