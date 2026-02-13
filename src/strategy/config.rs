use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::ai_evaluator::AiEvaluatorConfig;
use super::risk::RiskConfig;

const CONFIG_FILE: &str = "strategy_config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoSellConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Sell when profit >= entry_price * take_profit_pct (e.g. 0.50 = 50%)
    #[serde(default = "default_take_profit")]
    pub take_profit_pct: f64,
    /// Sell when loss >= entry_price * stop_loss_pct (e.g. 0.30 = 30%)
    #[serde(default = "default_stop_loss")]
    pub stop_loss_pct: f64,
    /// Re-evaluate with AI to check if edge is gone (expensive)
    #[serde(default)]
    pub check_edge: bool,
}

fn default_true() -> bool { true }
fn default_take_profit() -> f64 { 0.50 }
fn default_stop_loss() -> f64 { 0.30 }

impl Default for AutoSellConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            take_profit_pct: 0.50,
            stop_loss_pct: 0.30,
            check_edge: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub enabled: bool,
    pub scan_interval_secs: u64,
    pub risk: RiskConfig,
    pub dry_run: bool,
    #[serde(default)]
    pub ai_evaluator: AiEvaluatorConfig,
    #[serde(default)]
    pub auto_sell: AutoSellConfig,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_secs: 300,
            risk: RiskConfig::default(),
            dry_run: true,
            ai_evaluator: AiEvaluatorConfig::default(),
            auto_sell: AutoSellConfig::default(),
        }
    }
}

impl StrategyConfig {
    pub fn load() -> Result<Self> {
        let path = Path::new(CONFIG_FILE);
        if path.exists() {
            let data = std::fs::read_to_string(path)
                .context("Failed to read strategy config")?;
            serde_json::from_str(&data)
                .context("Failed to parse strategy config")
        } else {
            // Create default config file
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let data = serde_json::to_string_pretty(self)
            .context("Failed to serialize config")?;
        std::fs::write(CONFIG_FILE, data)
            .context("Failed to write config")?;
        Ok(())
    }
}
