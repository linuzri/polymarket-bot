use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tracing::{debug, info};

/// A candidate market that passed initial filters
#[derive(Debug, Clone)]
pub struct CandidateMarket {
    pub condition_id: String,
    pub question: String,
    pub description: Option<String>,
    pub slug: String,
    pub volume: f64,
    pub yes_price: f64,
    pub no_price: f64,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub end_date: Option<DateTime<Utc>>,
    pub neg_risk: bool,
    pub category: Option<String>,
}

/// Raw Gamma API market for scanning (includes extra fields)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaScanMarket {
    condition_id: Option<String>,
    question: Option<String>,
    description: Option<String>,
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
    #[serde(default)]
    group_item_title: Option<String>,
    #[serde(default)]
    events: Option<Vec<serde_json::Value>>,
}

pub struct MarketScanner {
    http: reqwest::Client,
    gamma_url: String,
    min_volume: f64,
    min_hours_to_close: f64,
}

impl MarketScanner {
    pub fn new(min_volume: f64, min_hours_to_close: f64) -> Self {
        Self {
            http: reqwest::Client::builder()
                .user_agent("polymarket-bot/0.1.0")
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap(),
            gamma_url: "https://gamma-api.polymarket.com".to_string(),
            min_volume,
            min_hours_to_close,
        }
    }

    /// Scan for candidate markets (combines top volume + fast-resolving)
    pub async fn scan(&self, limit: usize) -> Result<Vec<CandidateMarket>> {
        // Fetch 1: Top volume markets
        let url1 = format!(
            "{}/markets?closed=false&active=true&order=volume&ascending=false&limit={}",
            self.gamma_url,
            limit.min(200)
        );

        debug!("Scanning top volume markets: {}", url1);

        let mut raw: Vec<GammaScanMarket> = self.http
            .get(&url1)
            .send()
            .await
            .context("Failed to fetch markets for scanning")?
            .json()
            .await
            .context("Failed to parse scan response")?;

        // Fetch 2: Recent/fast-resolving markets (sorted by 24h volume)
        let url2 = format!(
            "{}/markets?closed=false&active=true&order=volume24hr&ascending=false&limit=100",
            self.gamma_url
        );

        if let Ok(resp) = self.http.get(&url2).send().await {
            if let Ok(fast_markets) = resp.json::<Vec<GammaScanMarket>>().await {
                // Add markets not already in the list (by condition_id)
                let existing: std::collections::HashSet<String> = raw.iter()
                    .filter_map(|m| m.condition_id.clone())
                    .collect();
                let before = raw.len();
                for m in fast_markets {
                    if let Some(ref cid) = m.condition_id {
                        if !existing.contains(cid) {
                            raw.push(m);
                        }
                    }
                }
                info!("Fetched {} additional fast-resolving markets", raw.len() - before);
            }
        }

        info!("Fetched {} total raw markets", raw.len());

        let now = Utc::now();
        let mut candidates = Vec::new();

        for m in raw {
            // Must have required fields
            let condition_id = match m.condition_id {
                Some(ref id) if !id.is_empty() => id.clone(),
                _ => continue,
            };
            let question = match m.question {
                Some(ref q) if !q.is_empty() => q.clone(),
                _ => continue,
            };
            let slug = match m.slug {
                Some(ref s) if !s.is_empty() => s.clone(),
                _ => continue,
            };

            // Must be active and not closed
            if m.active == Some(false) || m.closed == Some(true) {
                continue;
            }

            // Parse volume
            let volume = match &m.volume {
                Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                Some(serde_json::Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };
            if volume < self.min_volume {
                continue;
            }

            // Parse outcome prices
            let prices: Vec<f64> = m.outcome_prices
                .as_deref()
                .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
                .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())
                .unwrap_or_default();
            if prices.len() < 2 {
                continue;
            }
            let yes_price = prices[0];
            let no_price = prices[1];

            // Parse token IDs
            let tokens: Vec<String> = m.clob_token_ids
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            if tokens.len() < 2 {
                continue;
            }

            // Parse end date and check time to close
            let end_date = m.end_date_iso.as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));

            if let Some(end) = end_date {
                let hours_to_close = (end - now).num_minutes() as f64 / 60.0;
                if hours_to_close < self.min_hours_to_close {
                    continue;
                }
            }

            candidates.push(CandidateMarket {
                condition_id,
                question,
                description: m.description,
                slug,
                volume,
                yes_price,
                no_price,
                yes_token_id: tokens[0].clone(),
                no_token_id: tokens[1].clone(),
                end_date,
                neg_risk: m.neg_risk.unwrap_or(true),
                category: m.group_item_title,
            });
        }

        info!("📊 {} candidates passed filters (vol>${:.0}K, >{:.0}h to close)",
            candidates.len(), self.min_volume / 1000.0, self.min_hours_to_close);

        Ok(candidates)
    }
}
