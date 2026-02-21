# 🌤️ Polymarket Weather Arbitrage Bot

Automated weather prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Uses **NOAA + Open-Meteo weather forecasts** to find mispriced temperature markets and places limit orders at calculated fair value.

## 🔴 Live Trading Status (Feb 21, 2026)

- **Portfolio:** ~$118 | Cash: ~$85 USDC | All-time P/L: **+$18.22**
- **Initial Deposit:** $100.27
- **Open Positions:** Seoul 14°C (41.5 shares, +266%), Atlanta 59°F (37 shares), Ankara 10°C (42.7 shares)
- **Strategy:** 100% Weather Arbitrage (all other strategies on backlog)
- **PM2:** `polymarket-bot` **ONLINE** — continuous `weather` run_loop, scans every 30 min
- **Cities:** 13 (6 US + 7 international) — all with coordinates + forecast sources
- **Forecast Models:** 5 for US (NOAA + 4× Open-Meteo), 4 for international (Open-Meteo ensemble)
- **First Live Trades:** Feb 16, 2026 — Miami 81°F, Seoul 7°C
- **Best Trade:** Seoul Feb 21 — +$25.46 (266% return, still open)
- **Config:** 15% min edge, 40% Kelly, $20/bucket, $60 max exposure (3 concurrent positions), $100 Kelly bankroll (single position limit)

## How It Works

```
Every 30 minutes (PM2 run_loop):
1. Discover weather markets → 26+ markets across 13 cities (today + tomorrow)
2. Fetch forecasts → NOAA (US) + Open-Meteo (international)
3. Calculate probabilities → Normal distribution per temperature bucket
4. Find edges → Our probability vs market price (min 15% edge + forecast buffer)
5. Size positions → Kelly criterion (40% fraction, $20 max per bucket)
6. Place limit orders → BUY YES at 85% of fair value (maker, zero fees)
7. Wait for fills → Orders sit until someone takes the other side
8. Resolution → 1-2 days, winning buckets pay $1.00 per share
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
│   │   ├── mod.rs            # City configs, WeatherConfig, TempUnit
│   │   ├── noaa.rs           # NOAA API (api.weather.gov) — US cities
│   │   ├── open_meteo.rs     # Open-Meteo ensemble — international cities
│   │   ├── forecast.rs       # Normal distribution probability per bucket
│   │   ├── markets.rs        # Market discovery via slug patterns
│   │   └── strategy.rs       # Edge detection, Kelly sizing, execution
│   ├── api/client.rs         # Polymarket API (Gamma + CLOB)
│   ├── auth/mod.rs           # L2 HMAC + EIP-712 signing
│   ├── orders/mod.rs         # Tick-size-aware order placement
│   ├── notifications/mod.rs  # Telegram alerts
│   └── main.rs               # CLI entry point
├── weather_multi_source.py   # Python multi-source forecasting (5 models + bias correction)
├── check_balance.py          # Portfolio balance checker
├── check_exposure.py         # Current exposure calculator
├── config.toml               # Strategy configuration
└── .env                      # Wallet keys (never committed)
```

## Cities Tracked

### US (Fahrenheit) — NOAA
NYC • Chicago • Miami • Atlanta • Seattle • Dallas

### International (Celsius) — Open-Meteo
London • Seoul • Paris • Toronto • Buenos Aires • Ankara • Wellington

## Configuration

```toml
[weather]
min_edge = 0.15              # 15% minimum edge to trade
max_per_bucket = 20.0        # $20 max per temperature bucket
max_total_exposure = 60.0    # $60 total (up to 3 concurrent positions)
kelly_fraction = 0.40        # 40% Kelly for position sizing
kelly_bankroll = 100.0       # Actual capital for Kelly calculation
noaa_warm_bias_f = 1.0       # NOAA warm bias correction (°F)
forecast_buffer_f = 3.0      # °F buffer from bucket threshold
forecast_buffer_c = 2.0      # °C buffer from bucket threshold
```

## Safety Features

- **Per-position deduplication** — won't re-enter the same market+bucket across scans
- **Crash-safe trade logging** — saves to `strategy_trades.json` after each trade, not at end of cycle
- **Resolved position tracking** — queries Gamma API for closed markets, frees exposure automatically
- **Telegram notifications** — trade alerts + startup messages
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
