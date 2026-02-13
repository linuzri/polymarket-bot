use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::ai_evaluator::AiEvaluatorConfig;
use super::risk::RiskConfig;

const CONFIG_FILE: &str = "strategy_config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub enabled: bool,
    pub scan_interval_secs: u64,
    pub risk: RiskConfig,
    pub dry_run: bool,
    #[serde(default)]
    pub ai_evaluator: AiEvaluatorConfig,
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_secs: 300,
            risk: RiskConfig::default(),
            dry_run: true,
            ai_evaluator: AiEvaluatorConfig::default(),
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
