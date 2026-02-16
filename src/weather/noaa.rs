use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, warn};

use super::{City, CityForecast, TempUnit};

/// NOAA Weather API client (api.weather.gov)
/// No API key required — just a User-Agent header
pub struct NoaaClient {
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct PointsResponse {
    properties: PointsProperties,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PointsProperties {
    forecast: Option<String>,
    forecast_hourly: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ForecastResponse {
    properties: ForecastProperties,
}

#[derive(Debug, Deserialize)]
struct ForecastProperties {
    periods: Vec<ForecastPeriod>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ForecastPeriod {
    name: String,
    temperature: f64,
    temperature_unit: String,
    is_daytime: bool,
    start_time: String,
}

impl NoaaClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("polymarket-weather-bot/1.0 (contact@example.com)")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap();
        Self { http }
    }

    /// Fetch daily high temperature forecast for a US city
    pub async fn fetch_forecast(&self, city: &City) -> Result<Vec<CityForecast>> {
        // Step 1: Get forecast URL from points endpoint
        let points_url = format!(
            "https://api.weather.gov/points/{:.4},{:.4}",
            city.lat, city.lon
        );

        debug!("NOAA points request: {}", points_url);
        let points: PointsResponse = self.http
            .get(&points_url)
            .send()
            .await
            .context("NOAA points request failed")?
            .json()
            .await
            .context("Failed to parse NOAA points response")?;

        let forecast_url = points.properties.forecast
            .context("No forecast URL in NOAA response")?;

        // Step 2: Fetch the forecast
        debug!("NOAA forecast request: {}", forecast_url);
        let forecast: ForecastResponse = self.http
            .get(&forecast_url)
            .send()
            .await
            .context("NOAA forecast request failed")?
            .json()
            .await
            .context("Failed to parse NOAA forecast response")?;

        // Step 3: Extract daytime high temperatures
        let mut results = Vec::new();
        for period in &forecast.properties.periods {
            if !period.is_daytime {
                continue;
            }

            let temp = period.temperature;
            let unit = if period.temperature_unit == "F" {
                TempUnit::Fahrenheit
            } else {
                TempUnit::Celsius
            };

            // Extract date from start_time (ISO 8601)
            let date = period.start_time
                .split('T')
                .next()
                .unwrap_or("")
                .to_string();

            // Estimate days ahead for std_dev calculation
            let days_ahead = results.len() as f64 + 1.0;
            // NOAA forecast error: ~3-4°F for day 1, ~5-7°F for day 2+
            // Increased from 2.0+1.5x after Miami 81°F miss (1.5°F shift killed position)
            let std_dev = 3.5 + (days_ahead - 1.0) * 2.0;

            results.push(CityForecast {
                city: city.name.clone(),
                date,
                high_temp: temp,
                unit,
                std_dev,
            });

            // Only need first few days
            if results.len() >= 4 {
                break;
            }
        }

        if results.is_empty() {
            warn!("No daytime forecasts found for {}", city.name);
        }

        Ok(results)
    }
}
