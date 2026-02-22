# 🌤️ Polymarket Weather Arbitrage Bot

Automated weather prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Uses **NOAA + Open-Meteo forecasts + ensemble member probabilities** to find mispriced temperature markets and places limit orders at calculated fair value.

## 🔴 Live Trading Status (Feb 22, 2026)

- **Portfolio:** $119.29 USDC | All-time P/L: **+$18.22** (on $100.27 deposit)
- **Initial Deposit:** $100.27
- **Open Positions:** None — fully liquid
- **Strategy:** 100% Weather Arbitrage (all other strategies on backlog)
- **PM2:** `polymarket-bot` **ONLINE** — continuous `weather` run_loop, scans every 30 min
- **Cities:** 13 (6 US + 7 international) — all with coordinates, forecast sources + Weather Underground station codes
- **Forecast Models:** 119 ensemble members (ECMWF 51 + GFS 31 + ICON 40) + NOAA for US cities
- **First Live Trades:** Feb 16, 2026 — Miami 81°F, Seoul 7°C
- **Best Trade:** Seoul Feb 21 — +$31.87 (266% return)
- **Config:** 15% min edge, 25% Kelly, $20/bucket, $60 max exposure, 5¢ min market price

## How It Works

```
Every 30 minutes (PM2 run_loop):
1. Discover weather markets → 30+ markets across 13 cities (today + tomorrow + day after)
2. Fetch forecasts → NOAA (US) + Open-Meteo (all) + 119 ensemble members
3. Same-day markets → fetch real-time observations, adjust forecast if needed
4. Calculate probabilities → Ensemble voting (preferred) or normal distribution (fallback)
5. Find edges → Our probability vs market price (min 15% edge + forecast buffer)
6. Filter → Skip buckets below 5¢ (model unreliable in tails)
7. Size positions → Kelly criterion (25% fraction, $20 max per bucket)
8. Place limit orders → BUY YES at 85% of fair value (maker, zero fees)
9. Resolution → 1-2 days, slug-based Gamma API detection
```

## Why Weather Markets?

| Factor | Weather | Politics | Sports |
|--------|---------|----------|--------|
| **Edge Source** | Forecasts (reliable) | Sentiment (noisy) | Stats (competitive) |
| **Resolution** | 1-2 days | Weeks/months | Hours |
| **Order Books** | Wide spreads (opportunity!) | Tight | Tight |
| **Maker Fees** | 0% | 0% | 0% |
| **ROI Potential** | 200-700% per trade | 5-10% | 1-10% |

## Architecture

```
polymarket-bot/
├── src/
│   ├── weather/              # PRIMARY STRATEGY
│   │   ├── mod.rs            # City configs (with WUnderground stations), CityForecast (with ensemble_members)
│   │   ├── noaa.rs           # NOAA API (api.weather.gov) — US cities
│   │   ├── open_meteo.rs     # Open-Meteo multi-model + fetch_ensemble() (119 members)
│   │   ├── observations.rs   # Real-time METAR observations for same-day markets
│   │   ├── forecast.rs       # Normal distribution + ensemble member voting probabilities
│   │   ├── markets.rs        # Market discovery via slug patterns (3 dates)
│   │   └── strategy.rs       # Edge detection, Kelly sizing, execution, slug-based resolution
│   ├── api/client.rs         # Polymarket API (Gamma + CLOB)
│   ├── auth/mod.rs           # L2 HMAC + EIP-712 signing
│   ├── orders/mod.rs         # Tick-size-aware order placement
│   ├── notifications/mod.rs  # Telegram alerts
│   └── main.rs               # CLI entry point
├── weather_multi_source.py   # Python multi-source forecasting (5 models + bias correction)
├── config.toml               # Strategy configuration
└── .env                      # Wallet keys (never committed)
```

## Cities Tracked

### US (Fahrenheit) — NOAA + Open-Meteo + Ensemble
| City | Station | Coords |
|------|---------|--------|
| NYC | KLGA | 40.71, -74.01 |
| Chicago | KORD | 41.88, -87.63 |
| Miami | KMIA | 25.76, -80.19 |
| Atlanta | KATL | 33.75, -84.39 |
| Seattle | KSEA | 47.61, -122.33 |
| Dallas | KDFW | 32.78, -96.80 |

### International (Celsius) — Open-Meteo + Ensemble
| City | Station | Coords |
|------|---------|--------|
| London | EGLC | 51.51, -0.13 |
| Seoul | RKSS | 37.57, 126.98 |
| Paris | LFPG | 48.86, 2.35 |
| Toronto | CYYZ | 43.65, -79.38 |
| Buenos Aires | SAEZ | -34.60, -58.38 |
| Ankara | LTAC | 39.93, 32.86 |
| Wellington | NZWN | -41.29, 174.78 |

## Configuration

```toml
[weather]
min_edge = 0.15              # 15% minimum edge to trade
min_market_price = 0.05      # Skip buckets priced below 5¢
max_per_bucket = 20.0        # $20 max per temperature bucket
max_total_exposure = 60.0    # $60 total (up to 3 concurrent positions)
kelly_fraction = 0.25        # Quarter-Kelly (industry standard for prediction markets)
kelly_bankroll = 100.0       # Actual capital for Kelly calculation
noaa_warm_bias_f = 1.0       # NOAA warm bias correction (°F)
open_meteo_bias_f = 0.0      # Open-Meteo bias correction (°F) — 0 = raw model output
open_meteo_bias_c = 0.0      # Open-Meteo bias correction (°C)
forecast_buffer_f = 3.0      # °F buffer from bucket threshold
forecast_buffer_c = 2.0      # °C buffer from bucket threshold
```

## Probability Models

### Ensemble (Primary) — 119 members
Fetches individual member trajectories from Open-Meteo Ensemble API:
- ECMWF IFS (51 members) + GFS/GEFS (31 members) + ICON-EPS (40 members)
- Each member "votes" for a temperature bucket
- Fraction of members in each bucket = probability estimate
- Captures flow-dependent uncertainty (some days predictable, others not)

### Normal Distribution (Fallback)
- Used when <20 ensemble members available
- Fits Gaussian to mean + spread of 4 point forecasts
- Consensus weighting: 3+ models must agree for strong signal

### Same-Day Observations
- For markets resolving today, fetches real-time temperature
- If current temp > forecast high → adjusts forecast upward with tighter uncertainty

## Safety Features

- **Min market price filter** — won't bet on buckets priced below 5¢ (model unreliable in tails)
- **Per-position deduplication** — won't re-enter the same market+bucket across scans
- **Crash-safe trade logging** — saves to `strategy_trades.json` after each trade
- **Slug-based resolution** — reliable market closure detection via Gamma API slug query
- **Forecast buffer** — skips borderline bets where forecast is within 3°F/2°C of threshold
- **Telegram notifications** — trade alerts + startup messages + heartbeat
- **Exposure tracking** — loads unresolved trades from last 4 days on startup

## Quick Start

```bash
# Build
cargo build --release

# Dry run (no real orders)
polymarket-bot.exe weather --dry-run --once

# Single live scan
polymarket-bot.exe weather --once

# Continuous loop (PM2 managed)
pm2 start ecosystem.config.js --only polymarket-bot
pm2 save

# Check status
pm2 logs polymarket-bot --lines 20
```

## Key Insight: Be a Maker, Not a Taker

Weather markets have **massive bid-ask spreads** (30-60¢). The Gamma API mid-price is synthetic — real order books are thin. We place limit orders at our fair value and wait for fills, earning **zero fees** as makers.

## Backlog Strategies

These strategies are built but paused — weather is the focus:
- Sniper (buy 90-99.9% certain outcomes)
- Multi-outcome arbitrage
- 2-outcome arbitrage
- Hybrid take-profit
- AI evaluator (Claude-powered)

## Links

- [Polymarket](https://polymarket.com)
- [Trading Bot HQ Dashboard](https://trade-bot-hq.vercel.app/)
- [NOAA Weather API](https://api.weather.gov)
- [Open-Meteo](https://open-meteo.com)
- [Open-Meteo Ensemble API](https://ensemble-api.open-meteo.com)
