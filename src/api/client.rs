use anyhow::{Context, Result};
use reqwest::Client;
use tracing::{debug, info};

use super::endpoints;
use crate::models::market::{GammaMarket, Market, OrderBook, OrderBookResponse};

/// Polymarket API client
pub struct PolymarketClient {
    http: Client,
    gamma_url: String,
    clob_url: String,
}

impl PolymarketClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent("polymarket-bot/0.1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            http,
            gamma_url: endpoints::GAMMA_API.to_string(),
            clob_url: endpoints::CLOB_API.to_string(),
        })
    }

    /// Fetch markets from Gamma API
    pub async fn get_markets(&self, query: Option<&str>, limit: usize) -> Result<Vec<Market>> {
        let mut url = format!(
            "{}{}?closed=false&limit={}&order=volume&ascending=false&active=true",
            self.gamma_url,
            endpoints::MARKETS,
            limit.min(100)
        );

        if let Some(q) = query {
            url.push_str(&format!("&tag={}", q));
        }

        debug!("Fetching markets: {}", url);

        let response: Vec<GammaMarket> = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch markets")?
            .json()
            .await
            .context("Failed to parse markets response")?;

        let markets: Vec<Market> = response
            .into_iter()
            .map(|gm| Market {
                condition_id: gm.condition_id.unwrap_or_default(),
                question: gm.question.unwrap_or_default(),
                description: gm.description,
                volume: gm.volume.unwrap_or(0.0),
                yes_price: gm.outcome_prices.as_ref()
                    .and_then(|p| p.first().cloned())
                    .unwrap_or(0.5),
                no_price: gm.outcome_prices.as_ref()
                    .and_then(|p| p.get(1).cloned())
                    .unwrap_or(0.5),
                end_date: gm.end_date_iso,
                slug: gm.slug,
                tokens: gm.clob_token_ids,
            })
            .collect();

        info!("Fetched {} markets", markets.len());
        Ok(markets)
    }

    /// Fetch a single market by slug
    pub async fn get_market(&self, slug: &str) -> Result<Market> {
        let url = format!(
            "{}{}?slug={}",
            self.gamma_url, endpoints::MARKETS, slug
        );

        debug!("Fetching market: {}", url);

        let markets: Vec<GammaMarket> = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch market")?
            .json()
            .await
            .context("Failed to parse market response")?;

        let gm = markets.into_iter().next()
            .context(format!("Market not found: {}", slug))?;

        Ok(Market {
            condition_id: gm.condition_id.unwrap_or_default(),
            question: gm.question.unwrap_or_default(),
            description: gm.description,
            volume: gm.volume.unwrap_or(0.0),
            yes_price: gm.outcome_prices.as_ref()
                .and_then(|p| p.first().cloned())
                .unwrap_or(0.5),
            no_price: gm.outcome_prices.as_ref()
                .and_then(|p| p.get(1).cloned())
                .unwrap_or(0.5),
            end_date: gm.end_date_iso,
            slug: gm.slug,
            tokens: gm.clob_token_ids,
        })
    }

    /// Fetch order book from CLOB API
    pub async fn get_order_book(&self, token_id: &str) -> Result<OrderBook> {
        let url = format!(
            "{}{}?token_id={}",
            self.clob_url,
            endpoints::ORDER_BOOK,
            token_id
        );

        debug!("Fetching order book: {}", url);

        let response: OrderBookResponse = self
            .http
            .get(&url)
            .send()
            .await
            .context("Failed to fetch order book")?
            .json()
            .await
            .context("Failed to parse order book response")?;

        Ok(response.into())
    }

    /// Get mid price for a token
    pub async fn get_price(&self, token_id: &str) -> Result<f64> {
        let url = format!(
            "{}{}?token_id={}",
            self.clob_url,
            endpoints::PRICE,
            token_id
        );

        let response: serde_json::Value = self
            .http
            .get(&url)
            .send()
            .await?
            .json()
            .await?;

        let price = response
            .get("price")
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse::<f64>().ok())
            .unwrap_or(0.0);

        Ok(price)
    }
}
