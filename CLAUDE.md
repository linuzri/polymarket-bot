# CLAUDE.md - Polymarket Weather Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **100% weather arbitrage** — uses NOAA + Open-Meteo forecasts + ensemble probabilities to find mispriced temperature markets and places limit orders at fair value.

## Current Status (Feb 22, 2026)
- **Portfolio:** $119.29 USDC | All-time P/L: +$18.22 (on $100.27 deposit)
- **Open Positions:** NONE — all weather resolved, Fed positions manually closed
- **PM2:** `polymarket-bot` ONLINE — continuous `weather` run_loop, scans every 30 min
- **Telegram:** Trade alerts enabled (chat_id: 3588682)
- **polymarket-arb:** STOPPED (sniper/arb strategies paused)

### Feb 22 Upgrades (v2 — Major)

| Task | Detail |
|------|--------|
| **Ensemble probabilities** | 119 members from 3 ensemble systems (ECMWF 51 + GFS 31 + ICON 40) via Open-Meteo Ensemble API. Non-parametric: each member votes for a bucket. Falls back to normal distribution if <20 members. |
| **Configurable Open-Meteo bias** | Removed hardcoded +1.0°F/+0.5°C warm bias. Now `open_meteo_bias_f` and `open_meteo_bias_c` in config.toml (default 0.0). |
| **Min market price filter** | `min_market_price = 0.05` — skips buckets priced below 5¢ where model is unreliable in tails. |
| **Quarter-Kelly** | `kelly_fraction` 0.40 → 0.25. Industry standard for prediction markets. |
| **Real-time observations** | `observations.rs` — fetches current temperature for same-day markets. Adjusts forecast upward if current temp > forecast high. |
| **WUnderground stations** | `wunderground_station` on City struct. Logged per trade for resolution tracking. |
| **3-day discovery** | Markets discovered for today + tomorrow + day_after_tomorrow. `forecast_days=3`. |
| **Slug-based resolution** | `check_and_mark_resolved()` queries Gamma API by slug instead of brittle question substring matching. |
| **market_slug in trade log** | `WeatherTrade` includes `market_slug` field for reliable resolution. |

### Previous Overhaul (Feb 21, 2026)

| Fix | Detail |
|-----|--------|
| **Per-position dedup** | `placed_this_session: HashSet<String>` prevents re-entering same market+bucket |
| **Crash-safe logging** | `save_trade_log()` called per-trade, only appends last entry (no duplicates) |
| **Resolved tracking** | `resolved: bool` on WeatherTrade, Gamma API checks for closed markets |
| **Exposure management** | `load_existing_exposure()` filters resolved trades, 4-day window, mid-session decrement |
| **Kelly bankroll** | Separate `kelly_bankroll=100` from `max_total_exposure=60` |
| **NOAA bias configurable** | `noaa_warm_bias_f` in config.toml (was hardcoded +1.0) |
| **3 missing cities** | buenos-aires, ankara, wellington added to `intl_city()` coordinates |
| **Telegram** | Enabled in config.toml |

### Known Limitations
- `filled` field always false — no CLOB fill-confirmation endpoint
- `order_id` captured from CLOB response but not used for fill tracking
- No auto-redeem — PolymarketClient has no redeem/settle/merge methods
- Legacy trades (pre-Feb 22) in strategy_trades.json have no `market_slug` — resolution falls back to substring matching

## Strategy: Weather Arbitrage
- Scans 30+ weather markets across 13 cities (today + tomorrow + day_after_tomorrow)
- Fetches **119 ensemble members** from Open-Meteo Ensemble API (ECMWF + GFS + ICON)
- Also fetches NOAA (US) + Open-Meteo multi-model point forecasts as fallback
- **Ensemble probabilities** (preferred): each member votes for a bucket — non-parametric
- **Normal distribution** (fallback): when <20 ensemble members available
- Places LIMIT BUY orders at 85% of fair value (maker, zero fees)
- Kelly criterion sizing: 25% fraction, $100 bankroll, $20 max/bucket, $60 total exposure
- Min edge: 15% | Min market price: 5¢ | Forecast buffer: 3°F / 2°C
- Same-day markets: real-time observation adjustment when current temp > forecast
- Resolution: 1-2 days

### Cities
- **US (°F, NOAA + Open-Meteo + Ensemble):** NYC (KLGA), Chicago (KORD), Miami (KMIA), Atlanta (KATL), Seattle (KSEA), Dallas (KDFW)
- **International (°C, Open-Meteo + Ensemble):** London (EGLC), Seoul (RKSS), Paris (LFPG), Toronto (CYYZ), Buenos Aires (SAEZ), Ankara (LTAC), Wellington (NZWN)
- Station codes in parentheses = Weather Underground resolution stations

### Market Discovery
- Slug-based: `highest-temperature-in-{city}-on-{month}-{day}-{year}`
- Gamma API: `GET https://gamma-api.polymarket.com/events?slug={slug}`
- 3 dates checked: today, tomorrow, day_after_tomorrow
- `WEATHER_CITIES` in `markets.rs` must match `cities_us`/`cities_intl` in config.toml

## Architecture
```
polymarket-bot/
├── src/
│   ├── weather/                # PRIMARY STRATEGY
│   │   ├── mod.rs              # WeatherConfig, City (with station codes), CityForecast (with ensemble_members)
│   │   ├── strategy.rs         # WeatherStrategy: run_once(), check_and_mark_resolved(), Kelly sizing
│   │   ├── forecast.rs         # calculate_probabilities() + calculate_probabilities_ensemble()
│   │   ├── markets.rs          # WEATHER_CITIES list, slug generation, 3-day Gamma API discovery
│   │   ├── noaa.rs             # NOAA API (api.weather.gov) — US cities
│   │   ├── open_meteo.rs       # Open-Meteo multi-model + fetch_ensemble() (119 members)
│   │   └── observations.rs     # Real-time METAR observations for same-day markets
│   ├── api/client.rs           # PolymarketClient (Gamma + CLOB)
│   ├── auth/mod.rs             # L2 HMAC + EIP-712 signing
│   ├── orders/mod.rs           # place_order() → returns JSON with orderID
│   ├── notifications/mod.rs    # Telegram alerts
│   └── main.rs                 # CLI entry point
├── config.toml                 # Strategy configuration
├── ecosystem.config.js         # PM2 config (polymarket-bot → weather)
├── strategy_trades.json        # Trade log (crash-safe, per-trade writes)
├── weather_multi_source.py     # Python multi-source forecasting (5 models + bias correction)
└── .env                        # Wallet keys + Telegram token (NEVER commit)
```

## Key Patterns

### WeatherTrade struct (strategy.rs)
```rust
pub struct WeatherTrade {
    timestamp, market_question, bucket_label, city,
    our_probability, market_price, edge, side,
    shares, price, cost, dry_run,
    resolved: bool,              // true when market closed (Gamma API)
    filled: bool,                // always false (no fill confirmation)
    order_id: Option<String>,    // from CLOB response
    market_slug: Option<String>, // for reliable slug-based resolution
}
```

### run_once() flow
1. `check_and_mark_resolved()` — queries Gamma API by slug for closed markets, frees exposure
2. Discover 30+ weather markets via slug patterns (3 dates × 13 cities)
3. Fetch forecasts (NOAA + Open-Meteo + Ensemble) for 13 cities × 3 days
4. For each market:
   a. Same-day? → fetch current observation, adjust forecast if current > forecast high
   b. Log resolution station (WUnderground code)
   c. Use ensemble probabilities (119 members) or fall back to normal distribution
5. For each bucket: min price check → dedup → buffer check → edge check → Kelly sizing → order
6. `save_trade_log()` after each successful trade (with market_slug)

### Deduplication
- `placed_this_session: HashSet<String>` — keys are `"question|bucket"`
- Loaded from `strategy_trades.json` (non-dry-run, non-resolved, last 4 days) on startup
- Inserted after each successful order placement

### Exposure Tracking
- `load_existing_exposure()` sums cost of non-dry-run, non-resolved trades from last 4 days
- Decremented in-memory when `check_and_mark_resolved()` resolves a position
- `max_total_exposure=60` caps concurrent positions

## Critical Rules
- **NEVER commit .env** — wallet keys + Telegram token
- **PM2 release build:** Stop `polymarket-bot` before `cargo build --release`
- **Unicode:** No special chars in log messages (Windows cp1252)
- **CLOB prices:** Must be >0 and <1
- **Checksummed addresses** for CLOB API
- **signature_type=1** for proxy wallet orders
- **Adding cities:** Must update BOTH `WEATHER_CITIES` in markets.rs AND config.toml + coordinate lookup in mod.rs

## Wallet
- **EOA (signer):** 0x7ec329D34D2c94456c015B236EBEc41d2a7B3Bce
- **Proxy (funder/maker):** 0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853

## Commands
```bash
# Weather (primary — PM2 managed)
pm2 start ecosystem.config.js --only polymarket-bot
pm2 logs polymarket-bot --lines 20
pm2 restart polymarket-bot

# Manual runs
polymarket-bot.exe weather --once          # Single live scan
polymarket-bot.exe weather --dry-run --once # Test without orders
polymarket-bot.exe weather                  # Continuous loop (use PM2 instead)
```
