use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use tracing::{debug, warn, info};

use super::{City, CityForecast, TempUnit};

/// Open-Meteo multi-model forecast client
/// Fetches 4 models (best_match, GFS, ICON, ECMWF) in a single API call
pub struct OpenMeteoClient {
    http: reqwest::Client,
}

/// Multi-model response from api.open-meteo.com
#[derive(Debug, Deserialize)]
struct MultiModelResponse {
    daily: Option<serde_json::Value>,
}

/// Standard (non-ensemble) forecast response (fallback)
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

const MODEL_KEYS: &[(&str, &str)] = &[
    ("best_match", "temperature_2m_max"),
    ("gfs_seamless", "temperature_2m_max_gfs_seamless"),
    ("icon_seamless", "temperature_2m_max_icon_seamless"),
    ("ecmwf_ifs025", "temperature_2m_max_ecmwf_ifs025"),
];

impl OpenMeteoClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent("polymarket-weather-bot/1.0")
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap();
        Self { http }
    }

    /// Fetch multi-model forecast for a city
    pub async fn fetch_forecast(&self, city: &City) -> Result<Vec<CityForecast>> {
        // Try multi-model API first
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&daily=temperature_2m_max&forecast_days=2&models=gfs_seamless,icon_seamless,ecmwf_ifs025",
            city.lat, city.lon
        );

        debug!("Open-Meteo multi-model request for {}: {}", city.name, url);
        let resp = self.http.get(&url).send().await;

        match resp {
            Ok(r) if r.status().is_success() => {
                if let Ok(data) = r.json::<MultiModelResponse>().await {
                    if let Some(daily) = data.daily {
                        let result = self.parse_multi_model(&city.name, &daily, city.unit);
                        if !result.is_empty() {
                            return Ok(result);
                        }
                    }
                }
                warn!("Multi-model parse failed for {}, falling back", city.name);
            }
            Ok(r) => {
                debug!("Multi-model API returned {}, falling back", r.status());
            }
            Err(e) => {
                debug!("Multi-model API failed: {}, falling back", e);
            }
        }

        // Fallback: standard single-model forecast
        let unit_param = match city.unit {
            TempUnit::Fahrenheit => "&temperature_unit=fahrenheit",
            TempUnit::Celsius => "",
        };
        let fallback_url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&daily=temperature_2m_max&forecast_days=2{}",
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
        Ok(self.parse_daily_single(&city.name, &daily.time, &daily.temperature_2m_max, city.unit))
    }

    /// Parse multi-model response: extract per-model temps for each day
    fn parse_multi_model(&self, city_name: &str, daily: &serde_json::Value, unit: TempUnit) -> Vec<CityForecast> {
        let times = match daily.get("time").and_then(|v| v.as_array()) {
            Some(t) => t.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>(),
            None => return Vec::new(),
        };

        let bias = match unit {
            TempUnit::Fahrenheit => 1.0,  // +1.0 degF station warm bias
            TempUnit::Celsius => 0.5,     // +0.5 degC station warm bias
        };

        let mut results = Vec::new();

        for (i, date) in times.iter().enumerate() {
            if i >= 4 { break; }

            let mut model_temps: HashMap<String, f64> = HashMap::new();

            for &(model_name, json_key) in MODEL_KEYS {
                if let Some(arr) = daily.get(json_key).and_then(|v| v.as_array()) {
                    if let Some(temp_c) = arr.get(i).and_then(|v| v.as_f64()) {
                        let temp = match unit {
                            TempUnit::Fahrenheit => super::c_to_f(temp_c) + bias,
                            TempUnit::Celsius => temp_c + bias,
                        };
                        model_temps.insert(model_name.to_string(), temp);
                    }
                }
            }

            if model_temps.is_empty() {
                continue;
            }

            // Use mean of all models as high_temp
            let temps: Vec<f64> = model_temps.values().cloned().collect();
            let mean_temp = temps.iter().sum::<f64>() / temps.len() as f64;

            // Dynamic sigma from model spread (min 1.5C / 2.5F)
            let spread = temps.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                       - temps.iter().cloned().fold(f64::INFINITY, f64::min);
            let days_ahead = i as f64 + 1.0;
            let std_dev = match unit {
                TempUnit::Celsius => (spread * 0.8).max(1.5) + (days_ahead - 1.0) * 0.5,
                TempUnit::Fahrenheit => (spread * 0.8).max(2.5) + (days_ahead - 1.0) * 1.0,
            };

            let n_models = model_temps.len();
            info!(
                "  {} {} | {} models | mean={:.1} | spread={:.1} | sigma={:.1}",
                city_name, date, n_models, mean_temp, spread, std_dev
            );

            results.push(CityForecast {
                city: city_name.to_string(),
                date: date.clone(),
                high_temp: mean_temp,
                unit,
                std_dev,
                model_temps,
            });
        }

        if results.is_empty() {
            warn!("No multi-model forecast data found for {}", city_name);
        }

        results
    }

    /// Fallback: parse single-model response (backward compat)
    fn parse_daily_single(&self, city_name: &str, times: &[String], temps: &[f64], unit: TempUnit) -> Vec<CityForecast> {
        let mut results = Vec::new();

        for (i, (date, &temp)) in times.iter().zip(temps.iter()).enumerate() {
            let days_ahead = i as f64 + 1.0;
            let (high_temp, std_dev) = match unit {
                TempUnit::Celsius => {
                    (temp, 2.0 + (days_ahead - 1.0) * 1.0)
                }
                TempUnit::Fahrenheit => {
                    let temp_f = super::c_to_f(temp);
                    (temp_f, 3.5 + (days_ahead - 1.0) * 2.0)
                }
            };

            results.push(CityForecast {
                city: city_name.to_string(),
                date: date.clone(),
                high_temp,
                unit,
                std_dev,
                model_temps: HashMap::new(),
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
