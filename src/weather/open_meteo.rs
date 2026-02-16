use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, warn};

use super::{City, CityForecast, TempUnit};

/// Open-Meteo ensemble forecast client
/// Uses ensemble-api.open-meteo.com for multi-model probability distributions
pub struct OpenMeteoClient {
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct EnsembleResponse {
    daily: Option<EnsembleDaily>,
}

#[derive(Debug, Deserialize)]
struct EnsembleDaily {
    time: Vec<String>,
    #[serde(default)]
    temperature_2m_max: Vec<f64>,
}

/// Standard (non-ensemble) forecast response
#[derive(Debug, Deserialize)]
struct StandardResponse {
    daily: Option<StandardDaily>,
}

#[derive(Debug, Deserialize)]
struct StandardDaily {
    time: Vec<String>,
    #[serde(default)]
    temperature_2m_max: Vec<f64>,
}

impl OpenMeteoClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("polymarket-weather-bot/1.0")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap();
        Self { http }
    }

    /// Fetch ensemble forecast for a city (international cities use °C)
    pub async fn fetch_forecast(&self, city: &City) -> Result<Vec<CityForecast>> {
        // Try ensemble API first for spread data
        let url = format!(
            "https://ensemble-api.open-meteo.com/v1/ensemble?latitude={:.4}&longitude={:.4}&daily=temperature_2m_max&forecast_days=4&models=icon_seamless",
            city.lat, city.lon
        );

        debug!("Open-Meteo ensemble request for {}: {}", city.name, url);
        let resp = self.http.get(&url).send().await;

        match resp {
            Ok(r) if r.status().is_success() => {
                if let Ok(data) = r.json::<EnsembleResponse>().await {
                    if let Some(daily) = data.daily {
                        return Ok(self.parse_daily(&city.name, &daily.time, &daily.temperature_2m_max, city.unit));
                    }
                }
            }
            Ok(r) => {
                debug!("Ensemble API returned {}, falling back to standard", r.status());
            }
            Err(e) => {
                debug!("Ensemble API failed: {}, falling back to standard", e);
            }
        }

        // Fallback: standard forecast API
        let unit_param = match city.unit {
            TempUnit::Fahrenheit => "&temperature_unit=fahrenheit",
            TempUnit::Celsius => "",
        };
        let fallback_url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&daily=temperature_2m_max&forecast_days=4{}",
            city.lat, city.lon, unit_param
        );

        debug!("Open-Meteo standard request for {}: {}", city.name, fallback_url);
        let data: StandardResponse = self.http
            .get(&fallback_url)
            .send()
            .await
            .context("Open-Meteo request failed")?
            .json()
            .await
            .context("Failed to parse Open-Meteo response")?;

        let daily = data.daily.context("No daily data in Open-Meteo response")?;
        Ok(self.parse_daily(&city.name, &daily.time, &daily.temperature_2m_max, city.unit))
    }

    fn parse_daily(&self, city_name: &str, times: &[String], temps: &[f64], unit: TempUnit) -> Vec<CityForecast> {
        let mut results = Vec::new();

        for (i, (date, &temp)) in times.iter().zip(temps.iter()).enumerate() {
            let days_ahead = i as f64 + 1.0;
            // Open-Meteo returns °C by default; convert if city needs °F
            let (high_temp, std_dev) = match unit {
                TempUnit::Celsius => {
                    // σ ≈ 1.5°C for day 1, growing ~0.8°C per day
                    (temp, 1.5 + (days_ahead - 1.0) * 0.8)
                }
                TempUnit::Fahrenheit => {
                    // If data is in °C, convert; σ in °F
                    let temp_f = super::c_to_f(temp);
                    (temp_f, 2.5 + (days_ahead - 1.0) * 1.5)
                }
            };

            results.push(CityForecast {
                city: city_name.to_string(),
                date: date.clone(),
                high_temp,
                unit,
                std_dev,
            });

            if results.len() >= 4 {
                break;
            }
        }

        if results.is_empty() {
            warn!("No forecast data found for {}", city_name);
        }

        results
    }
}
