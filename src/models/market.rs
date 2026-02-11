use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize a value that might be a string or a number as f64
fn deserialize_string_f64<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNum {
        Num(f64),
        Str(String),
        Null,
    }
    match StringOrNum::deserialize(deserializer)? {
        StringOrNum::Num(n) => Ok(Some(n)),
        StringOrNum::Str(s) => Ok(s.parse::<f64>().ok()),
        StringOrNum::Null => Ok(None),
    }
}

/// Deserialize a JSON-encoded string array of floats (e.g. "[\"0.968\", \"0.032\"]")
fn deserialize_string_prices<'de, D>(deserializer: D) -> Result<Option<Vec<f64>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) => {
            // Parse the JSON string containing an array of string numbers
            let parsed: Vec<String> = serde_json::from_str(&s).unwrap_or_default();
            let prices: Vec<f64> = parsed
                .iter()
                .filter_map(|p| p.parse::<f64>().ok())
                .collect();
            if prices.is_empty() {
                Ok(None)
            } else {
                Ok(Some(prices))
            }
        }
    }
}

/// Deserialize a JSON-encoded string array of strings
fn deserialize_string_array<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(s) => {
            let parsed: Vec<String> = serde_json::from_str(&s).unwrap_or_default();
            if parsed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(parsed))
            }
        }
    }
}

/// Raw market data from Gamma API
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaMarket {
    pub condition_id: Option<String>,
    pub question: Option<String>,
    pub description: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_f64")]
    pub volume: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_string_prices")]
    pub outcome_prices: Option<Vec<f64>>,
    pub end_date_iso: Option<String>,
    pub slug: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_array")]
    pub clob_token_ids: Option<Vec<String>>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub closed: Option<bool>,
}

/// Cleaned market data
#[derive(Debug, Clone, Serialize)]
pub struct Market {
    pub condition_id: String,
    pub question: String,
    pub description: Option<String>,
    pub volume: f64,
    pub yes_price: f64,
    pub no_price: f64,
    pub end_date: Option<String>,
    pub slug: Option<String>,
    pub tokens: Option<Vec<String>>,
}

/// Order book entry
#[derive(Debug, Clone, Serialize)]
pub struct OrderLevel {
    pub price: f64,
    pub size: f64,
}

/// Order book
#[derive(Debug, Clone, Serialize)]
pub struct OrderBook {
    pub bids: Vec<OrderLevel>,
    pub asks: Vec<OrderLevel>,
    pub spread: f64,
    pub mid_price: f64,
}

/// Raw order book from CLOB API
#[derive(Debug, Deserialize)]
pub struct OrderBookResponse {
    pub market: Option<String>,
    pub asset_id: Option<String>,
    pub bids: Option<Vec<OrderBookEntry>>,
    pub asks: Option<Vec<OrderBookEntry>>,
}

#[derive(Debug, Deserialize)]
pub struct OrderBookEntry {
    pub price: String,
    pub size: String,
}

impl From<OrderBookResponse> for OrderBook {
    fn from(resp: OrderBookResponse) -> Self {
        let bids: Vec<OrderLevel> = resp
            .bids
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                Some(OrderLevel {
                    price: e.price.parse().ok()?,
                    size: e.size.parse().ok()?,
                })
            })
            .collect();

        let asks: Vec<OrderLevel> = resp
            .asks
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                Some(OrderLevel {
                    price: e.price.parse().ok()?,
                    size: e.size.parse().ok()?,
                })
            })
            .collect();

        let best_bid = bids.first().map(|b| b.price).unwrap_or(0.0);
        let best_ask = asks.first().map(|a| a.price).unwrap_or(1.0);
        let spread = best_ask - best_bid;
        let mid_price = (best_bid + best_ask) / 2.0;

        OrderBook {
            bids,
            asks,
            spread,
            mid_price,
        }
    }
}
