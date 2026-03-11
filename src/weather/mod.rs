pub mod noaa;
pub mod open_meteo;
pub mod forecast;
pub mod markets;
pub mod observations;
pub mod outcomes;
pub mod strategy;
pub mod calibration;
pub mod position_monitor;

use std::collections::HashMap;
use std::time::Duration;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Fetch an HTTP URL with retry and exponential backoff.
/// Shared utility for all weather API calls (Open-Meteo, NOAA, Ensemble, observations).
pub async fn fetch_with_retry(
    http: &reqwest::Client,
    url: &str,
    max_retries: u32,
    label: &str,
) -> Result<reqwest::Response> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match http.get(url).timeout(Duration::from_secs(15)).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    return Ok(resp);
                }
                let status = resp.status();
                if attempt >= max_retries {
                    anyhow::bail!("{} returned {} after {} attempts", label, status, max_retries);
                }
                warn!("{} returned {} (attempt {}/{})", label, status, attempt, max_retries);
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
            Err(e) => {
                if attempt >= max_retries {
                    anyhow::bail!("{} failed after {} attempts: {}", label, max_retries, e);
                }
                warn!("{} request failed (attempt {}/{}): {}", label, attempt, max_retries, e);
                tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
            }
        }
    }
}

/// Scan schedule configuration aligned with weather model releases
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScanSchedule {
    #[serde(default = "default_model_release_hours")]
    pub model_release_hours: Vec<u32>,
    #[serde(default = "default_fallback_interval")]
    pub fallback_interval_minutes: u64,
    #[serde(default = "default_post_release_delay")]
    pub post_release_delay_minutes: u64,
}

fn default_model_release_hours() -> Vec<u32> {
    vec![3, 5, 9, 11, 15, 17, 21, 23]
}
fn default_fallback_interval() -> u64 { 120 }
fn default_post_release_delay() -> u64 { 15 }

impl Default for ScanSchedule {
    fn default() -> Self {
        ScanSchedule {
            model_release_hours: default_model_release_hours(),
            fallback_interval_minutes: default_fallback_interval(),
            post_release_delay_minutes: default_post_release_delay(),
        }
    }
}

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
    #[serde(default = "default_kelly_bankroll")]
    pub kelly_bankroll: f64,
    #[serde(default = "default_noaa_warm_bias_f")]
    pub noaa_warm_bias_f: f64,
    #[serde(default = "default_open_meteo_bias_f")]
    pub open_meteo_bias_f: f64,
    #[serde(default = "default_open_meteo_bias_c")]
    pub open_meteo_bias_c: f64,
    #[serde(default = "default_min_market_price")]
    pub min_market_price: f64,
    /// Higher edge required for narrow/single-temperature buckets (e.g. "18°C" exactly)
    /// Ensemble tends to overestimate probability on tight ranges
    #[serde(default = "default_min_edge_narrow")]
    pub min_edge_narrow: f64,
    /// Minimum model probability to place a trade — filters out lottery tickets
    /// where ensemble says 25-50% but overestimates tail probability
    #[serde(default = "default_min_our_probability")]
    pub min_our_probability: f64,
    #[serde(default)]
    pub scan_schedule: ScanSchedule,
    #[serde(default = "default_false")]
    pub enable_laddering: bool,
    #[serde(default = "default_ladder_amount")]
    pub ladder_amount_per_bucket: f64,
    #[serde(default = "default_ladder_max_buckets")]
    pub ladder_max_buckets: usize,
    #[serde(default = "default_ladder_min_prob")]
    pub ladder_min_model_prob: f64,
    #[serde(default = "default_ladder_max_price")]
    pub ladder_max_market_price: f64,
    /// Hard cap on USDC per individual temperature bucket, regardless of Kelly output.
    #[serde(default = "default_max_per_bucket_hard_cap")]
    pub max_per_bucket_hard_cap: f64,
    /// Kill switch: skip all narrow/exact-temperature buckets entirely.
    /// Overrides min_edge_narrow — use when exact bets are consistently losing.
    #[serde(default = "default_false")]
    pub skip_narrow_bets: bool,
    /// Max total buys per market slug across all sessions (persisted in strategy_trades.json).
    /// Prevents over-concentrating in a single market. Default 10 = effectively no cap.
    #[serde(default = "default_max_buys_per_market")]
    pub max_buys_per_market: usize,
    /// Probability shrinkage factor: shrunk_prob = (1-factor) * ensemble_prob + factor * base_rate
    /// Default 0.3 = 30% shrinkage toward uniform prior
    #[serde(default = "default_probability_shrinkage")]
    pub probability_shrinkage: f64,
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
            kelly_bankroll: 100.0,
            noaa_warm_bias_f: 1.0,
            open_meteo_bias_f: 0.0,
            open_meteo_bias_c: 0.0,
            min_market_price: 0.05,
            min_edge_narrow: 0.25,
            min_our_probability: 0.60,
            scan_schedule: ScanSchedule::default(),
            enable_laddering: false,
            ladder_amount_per_bucket: 2.0,
            ladder_max_buckets: 5,
            ladder_min_model_prob: 0.05,
            ladder_max_market_price: 0.15,
            max_per_bucket_hard_cap: 4.0,
            skip_narrow_bets: false,
            max_buys_per_market: 10,
            probability_shrinkage: 0.3,
        }
    }
}

fn default_false() -> bool { false }
fn default_true() -> bool { true }
fn default_scan_interval() -> u64 { 1800 }
fn default_min_edge() -> f64 { 0.15 }
fn default_max_per_bucket() -> f64 { 10.0 }
fn default_max_total_exposure() -> f64 { 50.0 }
fn default_kelly_fraction() -> f64 { 0.25 }
fn default_kelly_bankroll() -> f64 { 100.0 }
fn default_noaa_warm_bias_f() -> f64 { 1.0 }
fn default_open_meteo_bias_f() -> f64 { 0.0 }
fn default_open_meteo_bias_c() -> f64 { 0.0 }
fn default_min_market_price() -> f64 { 0.05 }
fn default_min_edge_narrow() -> f64 { 0.25 }
fn default_min_our_probability() -> f64 { 0.60 }
fn default_ladder_amount() -> f64 { 2.0 }
fn default_ladder_max_buckets() -> usize { 5 }
fn default_ladder_min_prob() -> f64 { 0.05 }
fn default_ladder_max_price() -> f64 { 0.15 }
fn default_max_per_bucket_hard_cap() -> f64 { 4.0 }
fn default_max_buys_per_market() -> usize { 10 }
fn default_probability_shrinkage() -> f64 { 0.3 }
fn default_cities_us() -> Vec<String> {
    vec!["nyc", "chicago", "miami", "atlanta", "seattle", "dallas"]
        .into_iter().map(String::from).collect()
}
fn default_forecast_buffer() -> f64 { 3.0 }
fn default_forecast_buffer_c() -> f64 { 2.0 }
fn default_cities_intl() -> Vec<String> {
    vec!["london", "seoul", "paris", "toronto", "buenos-aires", "ankara"]
        .into_iter().map(String::from).collect()
}

/// City with coordinates and temperature unit
#[derive(Debug, Clone)]
pub struct City {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub unit: TempUnit,
    pub wunderground_station: Option<String>,
    /// IANA timezone string for correct daily max temperature calculation
    pub timezone: String,
    /// Whether this city is in the Southern Hemisphere (for seasonal bias correction).
    pub southern_hemisphere: bool,
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
    let (lat, lon, station, tz) = match name.to_lowercase().as_str() {
        "nyc" | "new york"                  => (40.7128,  -74.0060, "KLGA", "America/New_York"),
        "chicago"                           => (41.9742,  -87.9073, "KORD", "America/Chicago"),
        "miami"                             => (25.7617,  -80.1918, "KMIA", "America/New_York"),
        "atlanta"                           => (33.7490,  -84.3880, "KATL", "America/New_York"),
        "seattle"                           => (47.6062, -122.3321, "KSEA", "America/Los_Angeles"),
        "dallas"                            => (32.8471,  -96.8518, "KDAL", "America/Chicago"),
        "los-angeles" | "los angeles"       => (34.0522, -118.2437, "KLAX", "America/Los_Angeles"),
        "denver"                            => (39.7392, -104.9903, "KDEN", "America/Denver"),
        "phoenix"                           => (33.4484, -112.0740, "KPHX", "America/Phoenix"),
        "houston"                           => (29.7604,  -95.3698, "KIAH", "America/Chicago"),
        "boston"                            => (42.3601,  -71.0589, "KBOS", "America/New_York"),
        "minneapolis"                       => (44.9778,  -93.2650, "KMSP", "America/Chicago"),
        "philadelphia"                      => (39.9526,  -75.1652, "KPHL", "America/New_York"),
        "san-francisco" | "san francisco"   => (37.7749, -122.4194, "KSFO", "America/Los_Angeles"),
        "las-vegas" | "las vegas"           => (36.1699, -115.1398, "KLAS", "America/Los_Angeles"),
        "tampa"                             => (27.9506,  -82.4572, "KTPA", "America/New_York"),
        "detroit"                           => (42.3314,  -83.0458, "KDTW", "America/Detroit"),
        "austin"                            => (30.2672,  -97.7431, "KAUS", "America/Chicago"),
        "charlotte"                         => (35.2271,  -80.8431, "KCLT", "America/New_York"),
        "nashville"                         => (36.1627,  -86.7816, "KBNA", "America/Chicago"),
        "new-orleans" | "new orleans"       => (29.9511,  -90.0715, "KMSY", "America/Chicago"),
        "oklahoma-city" | "oklahoma city"   => (35.4676,  -97.5164, "KOKC", "America/Chicago"),
        "washington-dc" | "washington dc"   => (38.9072,  -77.0369, "KDCA", "America/New_York"),
        "aurora"                            => (39.7294, -104.8319, "KDEN", "America/Denver"),
        _ => return None,
    };
    Some(City {
        name: name.to_lowercase(),
        lat, lon,
        unit: TempUnit::Fahrenheit,
        wunderground_station: Some(station.to_string()),
        timezone: tz.to_string(),
        southern_hemisphere: false,
    })
}

fn intl_city(name: &str) -> Option<City> {
    let (lat, lon, station, tz, sh) = match name.to_lowercase().as_str() {
        "london"                        => (51.5074,   -0.1278, Some("EGLC"), "Europe/London", false),
        "seoul"                         => (37.5665,  126.9780, Some("RKSS"), "Asia/Seoul", false),
        "paris"                         => (48.8566,    2.3522, Some("LFPG"), "Europe/Paris", false),
        "toronto"                       => (43.6532,  -79.3832, Some("CYYZ"), "America/Toronto", false),
        "buenos-aires" | "buenos aires" => (-34.6037, -58.3816, Some("SAEZ"), "America/Argentina/Buenos_Aires", true),
        "ankara"                        => (39.9334,   32.8597, Some("LTAC"), "Europe/Istanbul", false),
        "wellington"                    => (-41.2924,  174.7787, Some("NZWN"), "Pacific/Auckland", true),
        "tokyo"                         => (35.6762,  139.6503, Some("RJTT"), "Asia/Tokyo", false),
        "sydney"                        => (-33.8688,  151.2093, Some("YSSY"), "Australia/Sydney", true),
        "singapore"                     => (1.3521,   103.8198, Some("WSSS"), "Asia/Singapore", false),
        "dubai"                         => (25.2048,   55.2708, Some("OMDB"), "Asia/Dubai", false),
        "berlin"                        => (52.5200,   13.4050, Some("EDDB"), "Europe/Berlin", false),
        "sao-paulo" | "sao paulo"       => (-23.5505,  -46.6333, Some("SBGR"), "America/Sao_Paulo", true),
        _ => return None,
    };
    Some(City {
        name: name.to_lowercase(),
        lat, lon,
        unit: TempUnit::Celsius,
        wunderground_station: station.map(String::from),
        timezone: tz.to_string(),
        southern_hemisphere: sh,
    })
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
    /// Per-model temperatures (e.g. "best_match" -> 42.5, "gfs_seamless" -> 43.1)
    pub model_temps: HashMap<String, f64>,
    /// Ensemble member temperatures (if available, used for non-parametric probability)
    pub ensemble_members: Option<Vec<f64>>,
}

/// Convert Celsius to Fahrenheit
pub fn c_to_f(c: f64) -> f64 {
    c * 9.0 / 5.0 + 32.0
}

/// Convert Fahrenheit to Celsius
pub fn f_to_c(f: f64) -> f64 {
    (f - 32.0) * 5.0 / 9.0
}
