# CLAUDE.md - Polymarket Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **Pivoted to weather arbitrage strategy** — using NOAA + Open-Meteo forecasts to find mispriced temperature markets and placing limit orders at fair value.

## Current Status (Feb 16, 2026)
- **Balance:** ~$26.56 USDC cash + ~$86 in positions (2 Fed + 2 weather)
- **Strategy:** 100% WEATHER FOCUS — all other strategies on backlog
- **Process:** Weather runs via OpenClaw cron every 3 hours (`weather --once`)
- **Dashboard sync:** `scripts/sync_dashboard.py` runs every 3h via cron → pushes to Supabase
- **polymarket-arb:** STOPPED (sniper/arb strategies paused)
- **polymarket-bot:** STOPPED (AI strategy paused)
- **Telegram:** Trade alerts on weather orders

### Recent Changes (Feb 16)
- **Weather hardening** (commit `5c39318`): Forecast buffer (3°F/2°C), higher std_dev, min_edge 15%
- **Dashboard sync script** added: `scripts/sync_dashboard.py` — fetches live prices, updates Supabase
- Miami/Seoul bets lost due to borderline forecasts — hardening prevents future borderline bets

## Strategy: Weather Arbitrage (ACTIVE — Primary Focus)
- Scans 26+ weather markets across 13 cities (today + tomorrow)
- Compares NOAA/Open-Meteo forecasts against Polymarket bucket prices
- Normal distribution probability model (configurable std dev per source)
- Places LIMIT BUY orders at 85% of fair value (maker, not taker)
- Zero maker fees on Polymarket
- Kelly criterion sizing: 40% fraction, $20 max/bucket, $100 total exposure
- Min edge: 15% (raised from 10% after Miami/Seoul losses)
- Resolution: 1-2 days (fast capital recycling)
- Cron: every 3 hours via OpenClaw
- **Multi-source verification:** `weather_multi_source.py` cross-checks 4 Open-Meteo models (best_match, gfs_seamless, icon_seamless, ecmwf_ifs025) with station bias correction. Only flags trades when 3+/4 models agree on the same bucket. Run with `python weather_multi_source.py --date YYYY-MM-DD`

### Why Weather Works
- Informational edge: weather forecasts are reliable (not just sentiment)
- Wide bid-ask spreads = mispricing opportunities
- Fast resolution = quick compounding

### Forecast Buffer (Added Feb 16)
- Skip bets where forecast is within 3°F (US) / 2°C (intl) of bucket threshold
- Prevents borderline bets killed by small forecast shifts
- Higher std_dev: NOAA 3.5+2.0x/day, Open-Meteo 2.0+1.0x/day (°C)

### Cities
- **US (°F):** NYC, Chicago, Miami, Atlanta, Seattle, Dallas (NOAA)
- **International (°C):** London, Seoul, Paris, Toronto, Buenos Aires, Ankara, Wellington (Open-Meteo)

### Market Discovery
- Slug-based: `highest-temperature-in-{city}-on-{month}-{day}-{year}`
- Gamma API: `GET https://gamma-api.polymarket.com/events?slug={slug}`
- Tag/category search does NOT work for weather markets

## Architecture
```
polymarket-bot/
├── src/
│   ├── api/
│   │   ├── client.rs      # API client (Gamma + CLOB + tick size fetching)
│   │   └── endpoints.rs   # URL constants
│   ├── weather/            # WEATHER ARBITRAGE (PRIMARY STRATEGY)
│   │   ├── mod.rs          # Module defs, city configs, WeatherConfig, TempUnit
│   │   ├── noaa.rs         # NOAA API client (api.weather.gov) for US cities
│   │   ├── open_meteo.rs   # Open-Meteo ensemble forecasts for international cities
│   │   ├── forecast.rs     # Normal distribution probability calculation per bucket
│   │   ├── markets.rs      # Gamma API weather market discovery + temp bucket parsing
│   │   └── strategy.rs     # Edge detection, Kelly sizing, trade execution, logging
│   ├── arbitrage/          # Sniper + arb (BACKLOG — stopped)
│   ├── auth/               # L2 HMAC auth + EIP-712 signing
│   ├── models/             # Market, OrderBook structs
│   ├── orders/             # Tick-size-aware order signing
│   ├── notifications/      # Telegram notifications
│   ├── portfolio/          # Position tracking
│   ├── strategy/           # AI evaluator (PAUSED)
│   ├── btc5min/            # BTC 5-min (DISABLED)
│   └── main.rs             # CLI: weather, arb, portfolio, paper
├── config.toml             # Weather config (primary)
├── ecosystem.config.js     # PM2 config (arb stopped)
├── .env                    # Credentials (NEVER commit)
└── scripts/                # Helper scripts
```

## Key Concepts

### Order Book Reality
- Weather markets have MASSIVE bid-ask spreads (e.g., 63¢ on Miami)
- Gamma API mid-prices are synthetic, NOT tradeable
- Must place LIMIT orders at fair value and wait for fills
- We are MAKERS (zero fees), not takers

### Tick Size System
- CLOB API: each market has `minimum_tick_size` (0.1, 0.01, 0.001, or 0.0001)
- Weather markets: typically 0.01
- Order amounts rounded per ROUNDING_CONFIG

## Backlog Strategies (Not Active)
- **Sniper:** Buy 90-99.9% certain outcomes, hold to resolution
- **Multi-outcome arb:** Buy all YES if sum < $1.00
- **2-outcome arb:** Buy YES+NO if sum < $0.985
- **Hybrid take-profit:** Sell sniper positions at 99¢+ bid
- **AI Evaluator:** Two-tier Claude evaluation (cost ~$1.50/day)

## Critical Rules
- **NEVER commit .env or hardcoded keys**
- **Unicode:** No Unicode special chars in log messages (Windows cp1252)
- **CLOB price constraint:** Price must be >0 and <1
- **Addresses must be checksummed** for CLOB API
- **signature_type=1** for proxy wallet orders
- **PM2 release build:** Must stop ALL processes sharing the exe before `cargo build --release`

## Wallet
- **EOA (signer):** 0x7ec329D34D2c94456c015B236EBEc41d2a7B3Bce
- **Proxy (funder/maker):** 0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853
- **Contract:** Neg risk exchange on Polygon (chain 137)

## Commands
```bash
# Weather (primary)
polymarket-bot.exe weather --once        # Single scan + trade
polymarket-bot.exe weather --dry-run     # Test without trading
polymarket-bot.exe weather               # Continuous loop

# Legacy (stopped)
pm2 stop polymarket-arb                  # Arb bot stopped
pm2 logs polymarket-arb                  # View old logs
```
