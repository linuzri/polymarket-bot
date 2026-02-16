pub mod noaa;
pub mod open_meteo;
pub mod forecast;
pub mod markets;
pub mod strategy;

use serde::{Deserialize, Serialize};

/// Weather configuration loaded from config.toml
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WeatherConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_scan_interval")]
    pub scan_interval_secs: u64,
    #[serde(default = "default_min_edge")]
    pub min_edge: f64,
    #[serde(default = "default_max_per_bucket")]
    pub max_per_bucket: f64,
    #[serde(default = "default_max_total_exposure")]
    pub max_total_exposure: f64,
    #[serde(default = "default_kelly_fraction")]
    pub kelly_fraction: f64,
    #[serde(default = "default_cities_us")]
    pub cities_us: Vec<String>,
    #[serde(default = "default_cities_intl")]
    pub cities_intl: Vec<String>,
    /// Minimum degrees between forecast and bucket threshold to place a bet.
    /// Prevents borderline bets where a 1-2° forecast shift kills the position.
    /// In °F for US cities, °C for international cities.
    #[serde(default = "default_forecast_buffer")]
    pub forecast_buffer_f: f64,
    #[serde(default = "default_forecast_buffer_c")]
    pub forecast_buffer_c: f64,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            scan_interval_secs: 1800,
            min_edge: 0.15,
            max_per_bucket: 10.0,
            max_total_exposure: 50.0,
            kelly_fraction: 0.25,
            cities_us: default_cities_us(),
            cities_intl: default_cities_intl(),
            forecast_buffer_f: 3.0,
            forecast_buffer_c: 2.0,
        }
    }
}

fn default_true() -> bool { true }
fn default_scan_interval() -> u64 { 1800 }
fn default_min_edge() -> f64 { 0.15 }
fn default_max_per_bucket() -> f64 { 10.0 }
fn default_max_total_exposure() -> f64 { 50.0 }
fn default_kelly_fraction() -> f64 { 0.25 }
fn default_cities_us() -> Vec<String> {
    vec!["nyc", "chicago", "miami", "atlanta", "seattle", "dallas"]
        .into_iter().map(String::from).collect()
}
fn default_forecast_buffer() -> f64 { 3.0 }
fn default_forecast_buffer_c() -> f64 { 2.0 }
fn default_cities_intl() -> Vec<String> {
    vec!["london", "seoul", "paris", "toronto"]
        .into_iter().map(String::from).collect()
}

/// City with coordinates and temperature unit
#[derive(Debug, Clone)]
pub struct City {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub unit: TempUnit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempUnit {
    Fahrenheit,
    Celsius,
}

impl TempUnit {
    pub fn symbol(&self) -> &'static str {
        match self {
            TempUnit::Fahrenheit => "°F",
            TempUnit::Celsius => "°C",
        }
    }
}

/// Get city definitions from config names
pub fn get_cities(config: &WeatherConfig) -> Vec<City> {
    let mut cities = Vec::new();

    for name in &config.cities_us {
        if let Some(city) = us_city(name) {
            cities.push(city);
        }
    }
    for name in &config.cities_intl {
        if let Some(city) = intl_city(name) {
            cities.push(city);
        }
    }

    cities
}

fn us_city(name: &str) -> Option<City> {
    let (lat, lon) = match name.to_lowercase().as_str() {
        "nyc" | "new york" => (40.7128, -74.0060),
        "chicago" => (41.8781, -87.6298),
        "miami" => (25.7617, -80.1918),
        "atlanta" => (33.7490, -84.3880),
        "seattle" => (47.6062, -122.3321),
        "dallas" => (32.7767, -96.7970),
        _ => return None,
    };
    Some(City { name: name.to_lowercase(), lat, lon, unit: TempUnit::Fahrenheit })
}

fn intl_city(name: &str) -> Option<City> {
    let (lat, lon) = match name.to_lowercase().as_str() {
        "london" => (51.5074, -0.1278),
        "seoul" => (37.5665, 126.9780),
        "paris" => (48.8566, 2.3522),
        "toronto" => (43.6532, -79.3832),
        _ => return None,
    };
    Some(City { name: name.to_lowercase(), lat, lon, unit: TempUnit::Celsius })
}

/// Forecast result for a single city/date
#[derive(Debug, Clone)]
pub struct CityForecast {
    pub city: String,
    pub date: String,
    pub high_temp: f64,
    pub unit: TempUnit,
    /// Standard deviation of forecast uncertainty
    pub std_dev: f64,
}

/// Convert Celsius to Fahrenheit
pub fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Convert Fahrenheit to Celsius
pub fn f_to_c(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}
