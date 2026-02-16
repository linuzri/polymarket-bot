use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{info, warn, error, debug};

use crate::api::client::PolymarketClient;
use crate::notifications::TelegramNotifier;
use crate::orders;

use super::forecast::{self, TempBucket};
use super::markets::{self, WeatherMarket};
use super::noaa::NoaaClient;
use super::open_meteo::OpenMeteoClient;
use super::{City, CityForecast, TempUnit, WeatherConfig, get_cities};

/// Trade log entry
#[derive(Debug, Serialize, Deserialize)]
pub struct WeatherTrade {
    pub timestamp: String,
    pub market_question: String,
    pub bucket_label: String,
    pub city: String,
    pub our_probability: f64,
    pub market_price: f64,
    pub edge: f64,
    pub side: String,
    pub shares: f64,
    pub price: f64,
    pub cost: f64,
    pub dry_run: bool,
}

/// Weather strategy runner
pub struct WeatherStrategy {
    config: WeatherConfig,
    noaa: NoaaClient,
    open_meteo: OpenMeteoClient,
    notifier: TelegramNotifier,
    http: reqwest::Client,
    dry_run: bool,
    total_exposure: f64,
    trades: Vec<WeatherTrade>,
}

impl WeatherStrategy {
    pub fn new(config: WeatherConfig, dry_run: bool) -> Self {
        Self {
            config,
            noaa: NoaaClient::new(),
            open_meteo: OpenMeteoClient::new(),
            notifier: TelegramNotifier::new(),
            http: reqwest::Client::builder()
                .user_agent("polymarket-weather-bot/1.0")
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap(),
            dry_run,
            total_exposure: 0.0,
            trades: Vec::new(),
        }
    }

    /// Run a single scan cycle
    pub async fn run_once(&mut self) -> Result<u32> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        info!("Weather strategy scan starting ({})", mode);

        // Step 1: Discover weather markets
        let weather_markets = markets::discover_weather_markets(&self.http).await?;
        if weather_markets.is_empty() {
            info!("No weather markets found on Polymarket");
            return Ok(0);
        }
        info!("Found {} weather markets", weather_markets.len());

        // Step 2: Fetch forecasts for relevant cities
        let cities = get_cities(&self.config);
        let forecasts = self.fetch_all_forecasts(&cities).await;
        if forecasts.is_empty() {
            warn!("No forecasts fetched — skipping weather strategy");
            return Ok(0);
        }
        info!("Fetched forecasts for {} cities", forecasts.len());

        // Step 3: Match markets to forecasts and find edges
        let mut trades_placed = 0u32;
        let client = PolymarketClient::new()?;

        for market in &weather_markets {
            if self.total_exposure >= self.config.max_total_exposure {
                info!("Total weather exposure limit reached (${:.2})", self.total_exposure);
                break;
            }

            // Find matching forecast
            let forecast = match self.find_matching_forecast(market, &forecasts) {
                Some(f) => f,
                None => {
                    debug!("No matching forecast for market: {}", market.question);
                    continue;
                }
            };

            // Calculate probabilities for each bucket
            let probs = forecast::calculate_probabilities(
                &forecast,
                &market.buckets.iter().map(|b| b.temp_bucket.clone()).collect::<Vec<_>>(),
            );

            // Evaluate each bucket for edge
            for bucket in &market.buckets {
                if self.total_exposure >= self.config.max_total_exposure {
                    break;
                }

                let our_prob = match probs.get(&bucket.label) {
                    Some(&p) => p,
                    None => continue,
                };

                let market_price = bucket.yes_price;
                if market_price <= 0.0 || market_price >= 1.0 {
                    continue;
                }

                // Edge = our probability - market price
                let edge = our_prob - market_price;

                if edge >= self.config.min_edge {
                    info!(
                        "EDGE FOUND: {} | {} | our={:.2} vs mkt={:.2} | edge={:.2}",
                        market.question, bucket.label, our_prob, market_price, edge
                    );

                    // Kelly criterion position sizing
                    let kelly_size = self.calculate_kelly_size(our_prob, market_price, edge);
                    if kelly_size < 0.50 {
                        debug!("Kelly size too small (${:.2}) — skipping", kelly_size);
                        continue;
                    }

                    // Place limit order at our fair value price
                    // Weather markets have wide spreads — we act as makers, not takers
                    // Bid slightly below our probability to ensure positive EV
                    let order_price = (our_prob * 0.85 * 100.0).round() / 100.0; // 85% of our fair value, rounded to cents
                    let order_price = order_price.max(0.01).min(0.95); // clamp to valid range

                    // Ensure we still have edge at our order price
                    if our_prob - order_price < 0.05 {
                        debug!("Edge too thin at order price ${:.2} vs prob {:.2}", order_price, our_prob);
                        continue;
                    }

                    let shares = kelly_size / order_price;
                    let shares = (shares * 100.0).floor() / 100.0;

                    // Polymarket minimum order size is typically 5 shares
                    if shares < 5.0 {
                        debug!("Shares below minimum ({})", shares);
                        continue;
                    }

                    let cost = shares * order_price;

                    println!("  >> WEATHER TRADE: {} | {}", market.question, bucket.label);
                    println!("     Our P={:.3} | Mid={:.3} | Edge={:.3} | Kelly=${:.2}",
                        our_prob, market_price, edge, kelly_size);
                    println!("     LIMIT BUY {:.2} YES @ ${:.4} = ${:.2}", shares, order_price, cost);

                    if !self.dry_run {
                        match orders::place_order(
                            &client,
                            &bucket.token_id,
                            orders::Side::Buy,
                            order_price,
                            shares,
                            market.neg_risk,
                            false,
                        ).await {
                            Ok(_) => {
                                info!("Weather order placed: {} @ ${:.4}", bucket.label, order_price);
                                self.total_exposure += cost;
                                trades_placed += 1;
                            }
                            Err(e) => {
                                error!("Weather order failed: {}", e);
                                self.notifier.notify_error("Weather order", &e.to_string()).await;
                                continue;
                            }
                        }
                    } else {
                        println!("     (DRY RUN — not executing)");
                        self.total_exposure += cost;
                        trades_placed += 1;
                    }

                    // Log trade
                    let trade = WeatherTrade {
                        timestamp: Utc::now().to_rfc3339(),
                        market_question: market.question.clone(),
                        bucket_label: bucket.label.clone(),
                        city: forecast.city.clone(),
                        our_probability: our_prob,
                        market_price,
                        edge,
                        side: "BUY_YES".to_string(),
                        shares,
                        price: order_price,
                        cost,
                        dry_run: self.dry_run,
                    };

                    // Telegram notification
                    let msg = format!(
                        "Weather Trade\n\n{}\nBucket: {}\nCity: {}\n\nOur P: {:.1}% | Market: {:.1}%\nEdge: {:.1}%\n\nBUY {:.2} YES @ ${:.4} = ${:.2}{}",
                        market.question, bucket.label, forecast.city,
                        our_prob * 100.0, market_price * 100.0, edge * 100.0,
                        shares, order_price, cost,
                        if self.dry_run { "\n(DRY RUN)" } else { "" }
                    );
                    self.notifier.send(&msg).await;

                    self.trades.push(trade);

                    // Rate limit between orders
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }

        // Save trade log
        self.save_trade_log()?;

        if trades_placed > 0 {
            info!("Weather strategy: {} trades placed, ${:.2} total exposure", trades_placed, self.total_exposure);
        } else {
            info!("Weather strategy: no edges found this cycle");
        }

        Ok(trades_placed)
    }

    /// Fetch forecasts from all sources for all configured cities
    async fn fetch_all_forecasts(&self, cities: &[City]) -> Vec<CityForecast> {
        let mut all_forecasts = Vec::new();

        for city in cities {
            let forecasts = match city.unit {
                TempUnit::Fahrenheit => {
                    // US cities: use NOAA
                    match self.noaa.fetch_forecast(city).await {
                        Ok(f) => f,
                        Err(e) => {
                            warn!("NOAA forecast failed for {}: {}, trying Open-Meteo", city.name, e);
                            // Fallback to Open-Meteo
                            self.open_meteo.fetch_forecast(city).await.unwrap_or_default()
                        }
                    }
                }
                TempUnit::Celsius => {
                    // International cities: use Open-Meteo
                    match self.open_meteo.fetch_forecast(city).await {
                        Ok(f) => f,
                        Err(e) => {
                            warn!("Open-Meteo forecast failed for {}: {}", city.name, e);
                            Vec::new()
                        }
                    }
                }
            };

            all_forecasts.extend(forecasts);

            // Rate limit API calls
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        }

        all_forecasts
    }

    /// Find the best matching forecast for a weather market
    fn find_matching_forecast<'a>(
        &self,
        market: &WeatherMarket,
        forecasts: &'a [CityForecast],
    ) -> Option<&'a CityForecast> {
        let market_city = market.city.as_deref()?;
        let market_date = market.date.as_deref();

        // Find forecast matching city and date
        forecasts.iter().find(|f| {
            let city_match = f.city.to_lowercase() == market_city.to_lowercase();
            let date_match = match market_date {
                Some(d) => f.date == d,
                None => true, // If no date in market, use first available forecast
            };
            city_match && date_match
        }).or_else(|| {
            // Fallback: just match city, use closest date
            forecasts.iter().find(|f| f.city.to_lowercase() == market_city.to_lowercase())
        })
    }

    /// Calculate position size using Kelly criterion
    fn calculate_kelly_size(&self, our_prob: f64, market_price: f64, _edge: f64) -> f64 {
        // Kelly fraction = (p * b - q) / b
        // where p = our probability, b = odds (payout / cost - 1), q = 1 - p
        let b = (1.0 / market_price) - 1.0; // odds
        let kelly_full = (our_prob * b - (1.0 - our_prob)) / b;

        // Fractional Kelly (more conservative)
        let kelly = kelly_full * self.config.kelly_fraction;

        // Clamp to max per bucket
        let bankroll = self.config.max_total_exposure;
        let size = (kelly * bankroll).max(0.0).min(self.config.max_per_bucket);

        // Don't exceed remaining exposure
        let remaining = self.config.max_total_exposure - self.total_exposure;
        size.min(remaining)
    }

    /// Save trade log to strategy_trades.json
    fn save_trade_log(&self) -> Result<()> {
        // Load existing trades
        let mut all_trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        // Append new trades
        for trade in &self.trades {
            all_trades.push(WeatherTrade {
                timestamp: trade.timestamp.clone(),
                market_question: trade.market_question.clone(),
                bucket_label: trade.bucket_label.clone(),
                city: trade.city.clone(),
                our_probability: trade.our_probability,
                market_price: trade.market_price,
                edge: trade.edge,
                side: trade.side.clone(),
                shares: trade.shares,
                price: trade.price,
                cost: trade.cost,
                dry_run: trade.dry_run,
            });
        }

        let json = serde_json::to_string_pretty(&all_trades)?;
        std::fs::write("strategy_trades.json", json)?;

        Ok(())
    }

    /// Run in a loop (with configurable interval)
    pub async fn run_loop(&mut self) -> Result<()> {
        let mode = if self.dry_run { "DRY RUN" } else { "LIVE" };
        println!("\n== Weather Arbitrage Strategy - {} ==", mode);
        println!("   Scan interval: {}s", self.config.scan_interval_secs);
        println!("   Min edge: {:.0}%", self.config.min_edge * 100.0);
        println!("   Max per bucket: ${:.0}", self.config.max_per_bucket);
        println!("   Max total exposure: ${:.0}", self.config.max_total_exposure);
        println!("   Kelly fraction: {:.0}%\n", self.config.kelly_fraction * 100.0);

        let startup_msg = format!(
            "Weather Strategy Started ({})\nInterval: {}s | Edge: {:.0}% | Max: ${:.0}",
            mode, self.config.scan_interval_secs,
            self.config.min_edge * 100.0, self.config.max_total_exposure
        );
        self.notifier.send(&startup_msg).await;

        loop {
            match self.run_once().await {
                Ok(n) => {
                    if n > 0 {
                        println!("  Weather: {} trades placed this cycle", n);
                    }
                }
                Err(e) => {
                    error!("Weather scan error: {}", e);
                    println!("Weather scan error: {}. Retrying...", e);
                }
            }

            tokio::time::sleep(std::time::Duration::from_secs(self.config.scan_interval_secs)).await;
        }
    }
}
