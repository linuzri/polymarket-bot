use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const LOG_FILE: &str = "strategy_trades.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEntry {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub condition_id: String,
    pub market_slug: String,
    pub market_question: String,
    pub side: String,        // "YES" or "NO"
    pub action: String,      // "BUY"
    pub price: f64,
    pub size_usd: f64,
    pub shares: f64,
    pub edge: f64,
    pub confidence: f64,
    pub reason: String,
    pub dry_run: bool,
    pub pnl: Option<f64>,
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeLog {
    pub trades: Vec<TradeEntry>,
    /// Track exposure per condition_id
    #[serde(default)]
    pub open_positions: HashMap<String, f64>,
}

impl TradeLog {
    pub fn load() -> Result<Self> {
        let path = Path::new(LOG_FILE);
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .context("Failed to read trade log")?;
            serde_json::from_str(&data)
                .context("Failed to parse trade log")
        } else {
            Ok(Self {
                trades: Vec::new(),
                open_positions: HashMap::new(),
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(self)
            .context("Failed to serialize trade log")?;
        std::fs::write(LOG_FILE, data)
            .context("Failed to write trade log")?;
        Ok(())
    }

    pub fn log_trade(&mut self, entry: TradeEntry) -> Result<()> {
        if !entry.dry_run {
            *self.open_positions.entry(entry.condition_id.clone()).or_insert(0.0) += entry.size_usd;
        }
        self.trades.push(entry);
        self.save()
    }

    pub fn open_position_count(&self) -> usize {
        self.open_positions.len()
    }

    pub fn total_exposure(&self) -> f64 {
        self.open_positions.values().sum()
    }

    pub fn has_position(&self, condition_id: &str) -> bool {
        self.open_positions.contains_key(condition_id)
    }
}
