use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::{debug, info};

use super::{City, TempUnit};

/// Current weather observation from Open-Meteo
#[derive(Debug, Deserialize)]
struct CurrentWeatherResponse {
    current: Option<CurrentData>,
}

#[derive(Debug, Deserialize)]
struct CurrentData {
    temperature_2m: Option<f64>,
    time: Option<String>,
}

/// Fetch current temperature observation for a city
/// Returns (temperature, observation_time) or None
pub async fn fetch_current_temp(http: &reqwest::Client, city: &City) -> Result<Option<(f64, String)>> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.4}&longitude={:.4}&current=temperature_2m",
        city.lat, city.lon
    );

    debug!("Fetching current observation for {}", city.name);
    let resp: CurrentWeatherResponse = http.get(&url).send().await
        .context("Current weather request failed")?
        .json().await
        .context("Failed to parse current weather")?;

    if let Some(current) = resp.current {
        if let Some(temp_c) = current.temperature_2m {
            let temp = match city.unit {
                TempUnit::Fahrenheit => super::c_to_f(temp_c),
                TempUnit::Celsius => temp_c,
            };
            let time = current.time.unwrap_or_default();
            info!("  {} current observation: {:.1}{} at {}", city.name, temp, city.unit.symbol(), time);
            return Ok(Some((temp, time)));
        }
    }

    Ok(None)
}
