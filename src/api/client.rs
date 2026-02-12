use anyhow::{Context, Result};
use reqwest::Client;
use tracing::{debug, info, warn};

use super::endpoints;
use crate::auth::ClobAuth;
use crate::models::market::{GammaMarket, Market, OrderBook, OrderBookResponse};

/// Polymarket API client
pub struct PolymarketClient {
    http: Client,
    gamma_url: String,
    clob_url: String,
    auth: Option<ClobAuth>,
}

impl PolymarketClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .user_agent("polymarket-bot/0.1.0")
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .context("Failed to create HTTP client")?;

        let auth = match ClobAuth::from_env() {
            Ok(a) => {
                info!("Loaded CLOB API credentials for {}", a.wallet_address);
                Some(a)
            }
            Err(e) => {
                warn!("CLOB auth not configured: {}. Authenticated endpoints unavailable.", e);
                None
            }
        };

        Ok(Self {
            http,
            gamma_url: endpoints::GAMMA_API.to_string(),
            clob_url: endpoints::CLOB_API.to_string(),
            auth,
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

    // ── Authenticated endpoints ──

    fn require_auth(&self) -> Result<&ClobAuth> {
        self.auth.as_ref()
            .context("CLOB auth not configured. Set POLY_API_KEY, POLY_API_SECRET, POLY_PASSPHRASE, and POLY_WALLET_ADDRESS in .env")
    }

    /// Authenticated GET request to CLOB API
    /// `sign_path` is the path used for HMAC signing (no query params)
    /// `full_path` is the full URL path including query params
    async fn auth_get_full(&self, sign_path: &str, full_path: &str) -> Result<serde_json::Value> {
        let auth = self.require_auth()?;
        let headers = auth.sign_request("GET", sign_path, None)?;
        let url = format!("{}{}", self.clob_url, full_path);

        debug!("Auth GET: {}", url);

        let mut req = self.http.get(&url)
            .header("Accept", "*/*")
            .header("Content-Type", "application/json")
            .header("Connection", "keep-alive")
;
        for (k, v) in &headers {
            req = req.header(k, v);
        }

        let resp = req.send().await.context("Auth GET request failed")?;
        let status = resp.status();
        let body = resp.text().await.context("Failed to read response body")?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                anyhow::bail!(
                    "CLOB API authentication failed (401). Your API key may be expired or invalid.\n\
                     Try re-deriving your API key using the Polymarket Python client:\n\
                     `client.create_or_derive_api_creds()` and update .env with the new credentials.\n\
                     Raw response: {}", body
                );
            }
            anyhow::bail!("CLOB API error ({}): {}", status, body);
        }

        debug!("Response body: {}", &body[..body.len().min(500)]);
        serde_json::from_str(&body).with_context(|| format!("Failed to parse JSON response: {}", &body[..body.len().min(200)]))
    }

    /// Simple auth_get where sign path == request path
    async fn auth_get(&self, path: &str) -> Result<serde_json::Value> {
        self.auth_get_full(path, path).await
    }

    /// Fetch API keys (serves as profile/account verification)
    pub async fn get_profile(&self) -> Result<serde_json::Value> {
        self.auth_get("/auth/api-keys").await
    }

    /// Fetch USDC balance allowance
    pub async fn get_balance(&self) -> Result<f64> {
        let auth = self.require_auth()?;
        let sig_type = auth.signature_type;
        // Sign with just the path, but request with query params
        let full_path = format!(
            "/balance-allowance?asset_type=COLLATERAL&signature_type={}",
            sig_type
        );
        let data = self.auth_get_full("/balance-allowance", &full_path).await?;
        // Response: {"balance": "100270276", "allowances": {...}}
        // Balance is in raw units (6 decimals for USDC)
        if let Some(bal_str) = data.get("balance").and_then(|b| b.as_str()) {
            let raw: f64 = bal_str.parse().context("Failed to parse balance")?;
            return Ok(raw / 1_000_000.0); // USDC has 6 decimals
        }
        if let Some(bal) = data.get("balance").and_then(|b| b.as_f64()) {
            return Ok(bal / 1_000_000.0);
        }
        anyhow::bail!("Unexpected balance response: {}", data)
    }

    /// Fetch full balance and allowance info
    pub async fn get_balance_allowance(&self) -> Result<serde_json::Value> {
        let auth = self.require_auth()?;
        let sig_type = auth.signature_type;
        let full_path = format!(
            "/balance-allowance?asset_type=COLLATERAL&signature_type={}",
            sig_type
        );
        self.auth_get_full("/balance-allowance", &full_path).await
    }

    /// Fetch open orders
    pub async fn get_positions(&self) -> Result<Vec<serde_json::Value>> {
        let data = self.auth_get_full("/data/orders", "/data/orders").await?;
        if let Some(arr) = data.as_array() {
            Ok(arr.clone())
        } else {
            Ok(vec![data])
        }
    }
}
