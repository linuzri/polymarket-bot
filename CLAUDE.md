# CLAUDE.md - Polymarket Weather Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **100% weather arbitrage** — uses NOAA + Open-Meteo forecasts to find mispriced temperature markets and places limit orders at fair value.

## Current Status (Feb 21, 2026)
- **Portfolio:** ~$118 | Cash: ~$85 USDC | All-time P/L: +$18.22
- **Open Positions:** Seoul 14°C (+266%), Dallas 59°F ($0.22), Ankara 10°C ($0.27)
- **PM2:** `polymarket-bot` ONLINE — continuous `weather` run_loop, scans every 30 min
- **Telegram:** Trade alerts enabled (chat_id: 3588682)
- **polymarket-arb:** STOPPED (sniper/arb strategies paused)

### Complete Overhaul (Feb 21, 2026)
All fixes implemented and verified this session:

| Fix | Detail |
|-----|--------|
| **Per-position dedup** | `placed_this_session: HashSet<String>` prevents re-entering same market+bucket |
| **Crash-safe logging** | `save_trade_log()` called per-trade, only appends last entry (no duplicates) |
| **Resolved tracking** | `resolved: bool` on WeatherTrade, Gamma API checks for closed markets |
| **Exposure management** | `load_existing_exposure()` filters resolved trades, 4-day window, mid-session decrement |
| **Kelly bankroll** | Separate `kelly_bankroll=100` from `max_total_exposure=60` |
| **NOAA bias configurable** | `noaa_warm_bias_f` in config.toml (was hardcoded +1.0) |
| **3 missing cities** | buenos-aires, ankara, wellington added to `intl_city()` coordinates |
| **forecast_days** | 4→2 in Open-Meteo URLs (only need today+tomorrow) |
| **Telegram** | Enabled in config.toml |
| **Edge-at-order-price** | Threshold 0.05→0.04 (floating point tolerance) |
| **False-positive fix** | Match length 20→50 chars in resolved detection |

### Known Limitations
- `filled` field always false — no CLOB fill-confirmation endpoint
- `order_id` captured from CLOB response but not used for fill tracking
- No auto-redeem — PolymarketClient has no redeem/settle/merge methods
- Positions resolve via Gamma API closed-market check only

## Strategy: Weather Arbitrage
- Scans 26 weather markets across 13 cities (today + tomorrow)
- Compares NOAA (US) + Open-Meteo ensemble (international) forecasts against market prices
- Normal distribution probability model per temperature bucket
- Places LIMIT BUY orders at 85% of fair value (maker, zero fees)
- Kelly criterion sizing: 40% fraction, $100 bankroll, $20 max/bucket, $60 total exposure
- Min edge: 15% | Forecast buffer: 3°F / 2°C
- Resolution: 1-2 days

### Cities
- **US (°F, NOAA + Open-Meteo):** NYC, Chicago, Miami, Atlanta, Seattle, Dallas
- **International (°C, Open-Meteo only):** London, Seoul, Paris, Toronto, Buenos Aires, Ankara, Wellington
- Checked Feb 21: no other cities have Polymarket weather markets

### Market Discovery
- Slug-based: `highest-temperature-in-{city}-on-{month}-{day}-{year}`
- Gamma API: `GET https://gamma-api.polymarket.com/events?slug={slug}`
- `WEATHER_CITIES` in `markets.rs` must match `cities_us`/`cities_intl` in config.toml

## Architecture
```
polymarket-bot/
├── src/
│   ├── weather/                # PRIMARY STRATEGY
│   │   ├── mod.rs              # WeatherConfig struct, City, get_cities(), us_city(), intl_city()
│   │   ├── strategy.rs         # WeatherStrategy: run_once(), check_and_mark_resolved(), Kelly sizing
│   │   ├── forecast.rs         # Normal distribution probability per bucket
│   │   ├── markets.rs          # WEATHER_CITIES list, slug generation, Gamma API discovery
│   │   ├── noaa.rs             # NOAA API (api.weather.gov) — US cities
│   │   └── open_meteo.rs       # Open-Meteo 3-model ensemble — all cities
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
    resolved: bool,     // true when market closed (Gamma API)
    filled: bool,       // always false (no fill confirmation)
    order_id: Option<String>,  // from CLOB response
}
```

### run_once() flow
1. `check_and_mark_resolved()` — queries Gamma API for closed markets, frees exposure
2. Discover 26 weather markets via slug patterns
3. Fetch forecasts (NOAA + Open-Meteo) for 13 cities × 2 days
4. For each bucket: dedup check → buffer check → edge check → Kelly sizing → order
5. `save_trade_log()` after each successful trade

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
