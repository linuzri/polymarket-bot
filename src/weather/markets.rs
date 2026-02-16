use anyhow::Result;
use chrono::Datelike;
use serde::Deserialize;
use tracing::{debug, info, warn};

use super::forecast::TempBucket;
use super::TempUnit;

/// A weather market from Polymarket with parsed temperature buckets
#[derive(Debug, Clone)]
pub struct WeatherMarket {
    pub condition_id: String,
    pub question: String,
    pub slug: String,
    pub city: Option<String>,
    pub date: Option<String>,
    pub buckets: Vec<MarketBucket>,
    pub neg_risk: bool,
    pub unit: TempUnit,
}

/// A single outcome/bucket within a weather market
#[derive(Debug, Clone)]
pub struct MarketBucket {
    pub token_id: String,
    pub label: String,
    pub temp_bucket: TempBucket,
    pub yes_price: f64,
    pub ask_price: Option<f64>,
}

/// Raw Gamma API market response for weather
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaWeatherMarket {
    condition_id: Option<String>,
    question: Option<String>,
    slug: Option<String>,
    outcome_prices: Option<String>,
    clob_token_ids: Option<String>,
    #[serde(default)]
    neg_risk: Option<bool>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    closed: Option<bool>,
    tags: Option<Vec<String>>,
    description: Option<String>,
    end_date_iso: Option<String>,
}

/// Gamma event containing multiple weather markets (temperature buckets)
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaWeatherEvent {
    title: Option<String>,
    slug: Option<String>,
    #[serde(default)]
    markets: Vec<GammaWeatherMarket>,
}

/// Cities tracked for weather markets and whether they use Fahrenheit
const WEATHER_CITIES: &[(&str, bool)] = &[
    // US cities (Fahrenheit)
    ("nyc", true),
    ("chicago", true),
    ("miami", true),
    ("atlanta", true),
    ("seattle", true),
    ("dallas", true),
    // International cities (Celsius)
    ("london", false),
    ("seoul", false),
    ("paris", false),
    ("toronto", false),
    ("buenos-aires", false),
    ("ankara", false),
    ("wellington", false),
];

/// Generate the Polymarket event slug for a city and date
fn weather_slug(city: &str, date: chrono::NaiveDate) -> String {
    let month = date.format("%B").to_string().to_lowercase();
    let day = date.day();
    let year = date.year();
    format!("highest-temperature-in-{}-on-{}-{}-{}", city, month, day, year)
}

/// Discover weather markets from Polymarket's Gamma API using known slug patterns
pub async fn discover_weather_markets(http: &reqwest::Client) -> Result<Vec<WeatherMarket>> {
    use chrono::Utc;

    let today = Utc::now().date_naive();
    let tomorrow = today + chrono::Duration::days(1);
    let dates = [today, tomorrow];

    let mut weather_markets = Vec::new();

    for &(city, is_fahrenheit) in WEATHER_CITIES {
        for &date in &dates {
            let slug = weather_slug(city, date);
            let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);

            debug!("Fetching weather market: {}", url);
            let resp = match http.get(&url).send().await {
                Ok(r) => r,
                Err(e) => {
                    warn!("Failed to fetch {}: {}", slug, e);
                    continue;
                }
            };

            let events: Vec<GammaWeatherEvent> = match resp.json().await {
                Ok(e) => e,
                Err(e) => {
                    debug!("No event for slug {}: {}", slug, e);
                    continue;
                }
            };

            for event in &events {
                if let Some(mut wm) = parse_weather_event(event) {
                    // Override unit based on our city knowledge
                    wm.unit = if is_fahrenheit { TempUnit::Fahrenheit } else { TempUnit::Celsius };
                    if wm.city.is_none() {
                        wm.city = Some(city.to_string());
                    }
                    if wm.date.is_none() {
                        wm.date = Some(date.format("%Y-%m-%d").to_string());
                    }
                    info!("Found weather market: {} ({} buckets)", wm.question, wm.buckets.len());
                    weather_markets.push(wm);
                }
            }
        }
    }

    // Deduplicate by condition_id
    weather_markets.sort_by(|a, b| a.condition_id.cmp(&b.condition_id));
    weather_markets.dedup_by(|a, b| a.condition_id == b.condition_id);

    info!("Discovered {} weather markets", weather_markets.len());
    Ok(weather_markets)
}

fn parse_weather_event(event: &GammaWeatherEvent) -> Option<WeatherMarket> {
    let title = event.title.as_deref()?;
    let slug = event.slug.as_deref().unwrap_or("").to_string();

    if event.markets.is_empty() {
        return None;
    }

    // Determine unit from the event title or first market question
    let sample_text = format!("{} {}", title, event.markets[0].question.as_deref().unwrap_or(""));
    let unit = if sample_text.contains("°F") || sample_text.contains("Fahrenheit") {
        TempUnit::Fahrenheit
    } else {
        TempUnit::Celsius
    };

    // Try to extract city name from title
    let city = extract_city(title);
    let date = extract_date(title);

    // Parse each market as a temperature bucket
    let mut buckets = Vec::new();
    let mut first_cid = String::new();
    let neg_risk = event.markets.first()
        .and_then(|m| m.neg_risk)
        .unwrap_or(true);

    for market in &event.markets {
        let cid = market.condition_id.as_deref().unwrap_or("");
        let question = market.question.as_deref().unwrap_or("");

        if first_cid.is_empty() {
            first_cid = cid.to_string();
        }

        // Parse token IDs
        let tokens: Vec<String> = market.clob_token_ids
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();

        if tokens.is_empty() {
            continue;
        }

        // Parse prices
        let prices: Vec<f64> = market.outcome_prices
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .map(|v| v.iter().filter_map(|p| p.parse::<f64>().ok()).collect())
            .unwrap_or_default();

        let yes_price = prices.first().copied().unwrap_or(0.0);

        // Parse temperature bucket from question
        if let Some(temp_bucket) = parse_temp_bucket(question, unit) {
            buckets.push(MarketBucket {
                token_id: tokens[0].clone(), // YES token
                label: temp_bucket.label.clone(),
                temp_bucket,
                yes_price,
                ask_price: None, // Filled later from order book
            });
        }
    }

    if buckets.is_empty() {
        return None;
    }

    Some(WeatherMarket {
        condition_id: first_cid,
        question: title.to_string(),
        slug,
        city,
        date,
        buckets,
        neg_risk,
        unit,
    })
}

/// Parse temperature bucket from a market question
/// Examples: "38-39°F", "52°F or higher", "9°C", "7°C or higher", "37°F or lower"
fn parse_temp_bucket(question: &str, default_unit: TempUnit) -> Option<TempBucket> {
    let q = question.trim();

    // Detect unit
    let unit = if q.contains("°F") || q.contains("Fahrenheit") {
        TempUnit::Fahrenheit
    } else if q.contains("°C") || q.contains("Celsius") {
        TempUnit::Celsius
    } else {
        default_unit
    };

    let symbol = unit.symbol();

    // Pattern: "X-Y°F" or "X-Y°C" (range bucket)
    if let Some(range) = extract_range(q) {
        return Some(TempBucket::new(
            range.0,
            range.1,
            format!("{}-{}{}", range.0 as i32, range.1 as i32, symbol),
        ));
    }

    // Pattern: "X°F or higher" / "X°C or higher"
    if q.to_lowercase().contains("or higher") || q.to_lowercase().contains("or more") || q.to_lowercase().contains("or above") {
        if let Some(temp) = extract_single_temp(q) {
            return Some(TempBucket::new(
                temp,
                f64::INFINITY,
                format!("{}{} or higher", temp as i32, symbol),
            ));
        }
    }

    // Pattern: "X°F or lower" / "X°C or lower"
    if q.to_lowercase().contains("or lower") || q.to_lowercase().contains("or less") || q.to_lowercase().contains("or below") {
        if let Some(temp) = extract_single_temp(q) {
            return Some(TempBucket::new(
                f64::NEG_INFINITY,
                temp,
                format!("{}{} or lower", temp as i32, symbol),
            ));
        }
    }

    // Pattern: single temp "X°F" or "X°C" (exact bucket, treat as X to X)
    if let Some(temp) = extract_single_temp(q) {
        return Some(TempBucket::new(
            temp,
            temp,
            format!("{}{}", temp as i32, symbol),
        ));
    }

    None
}

/// Extract a temperature range like "38-39" from text
fn extract_range(text: &str) -> Option<(f64, f64)> {
    // Look for pattern like "38-39" or "38–39" (en dash)
    let re_pattern = text.replace('–', "-"); // normalize en dash
    for word in re_pattern.split_whitespace() {
        let clean = word.replace("°F", "").replace("°C", "").replace(',', "");
        if let Some(dash_pos) = clean.find('-') {
            // Make sure it's not a negative sign at position 0
            if dash_pos > 0 {
                let left = &clean[..dash_pos];
                let right = &clean[dash_pos + 1..];
                if let (Ok(min), Ok(max)) = (left.parse::<f64>(), right.parse::<f64>()) {
                    return Some((min, max));
                }
            }
        }
    }
    None
}

/// Extract a single temperature number from text
fn extract_single_temp(text: &str) -> Option<f64> {
    for word in text.split_whitespace() {
        let clean = word.replace("°F", "").replace("°C", "").replace(',', "").replace('(', "").replace(')', "");
        if let Ok(temp) = clean.parse::<f64>() {
            // Sanity check: reasonable temperature range
            if temp > -60.0 && temp < 150.0 {
                return Some(temp);
            }
        }
    }
    None
}

/// Try to extract city name from market title
fn extract_city(title: &str) -> Option<String> {
    let title_lower = title.to_lowercase();
    let cities = [
        ("new york", "nyc"), ("nyc", "nyc"), ("manhattan", "nyc"),
        ("chicago", "chicago"), ("miami", "miami"), ("atlanta", "atlanta"),
        ("seattle", "seattle"), ("dallas", "dallas"),
        ("london", "london"), ("seoul", "seoul"), ("paris", "paris"), ("toronto", "toronto"),
    ];

    for (pattern, name) in &cities {
        if title_lower.contains(pattern) {
            return Some(name.to_string());
        }
    }
    None
}

/// Try to extract date from market title (e.g., "February 17", "Feb 17, 2026")
fn extract_date(title: &str) -> Option<String> {
    // Simple extraction: look for month + day patterns
    let months = [
        "january", "february", "march", "april", "may", "june",
        "july", "august", "september", "october", "november", "december",
    ];

    let title_lower = title.to_lowercase();
    for (i, month) in months.iter().enumerate() {
        if let Some(pos) = title_lower.find(month) {
            // Look for a number after the month name
            let after = &title[pos + month.len()..];
            for word in after.split_whitespace() {
                let clean = word.replace(',', "");
                if let Ok(day) = clean.parse::<u32>() {
                    if (1..=31).contains(&day) {
                        // Use current year as default
                        let year = chrono::Utc::now().format("%Y").to_string();
                        return Some(format!("{}-{:02}-{:02}", year, i + 1, day));
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_range_bucket() {
        let bucket = parse_temp_bucket("38-39°F", TempUnit::Fahrenheit).unwrap();
        assert_eq!(bucket.min_temp, 38.0);
        assert_eq!(bucket.max_temp, 39.0);
    }

    #[test]
    fn test_parse_or_higher() {
        let bucket = parse_temp_bucket("52°F or higher", TempUnit::Fahrenheit).unwrap();
        assert_eq!(bucket.min_temp, 52.0);
        assert!(bucket.max_temp.is_infinite());
    }

    #[test]
    fn test_parse_or_lower() {
        let bucket = parse_temp_bucket("37°F or lower", TempUnit::Fahrenheit).unwrap();
        assert!(bucket.min_temp.is_infinite() && bucket.min_temp < 0.0);
        assert_eq!(bucket.max_temp, 37.0);
    }

    #[test]
    fn test_parse_celsius() {
        let bucket = parse_temp_bucket("9°C", TempUnit::Celsius).unwrap();
        assert_eq!(bucket.min_temp, 9.0);
        assert_eq!(bucket.max_temp, 9.0);
    }

    #[test]
    fn test_extract_city() {
        assert_eq!(extract_city("New York City highest temperature"), Some("nyc".to_string()));
        assert_eq!(extract_city("Chicago weather"), Some("chicago".to_string()));
        assert_eq!(extract_city("Seoul temperature forecast"), Some("seoul".to_string()));
    }
}
