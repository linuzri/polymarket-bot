# 🌤️ Polymarket Weather Arbitrage Bot

Automated weather prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Uses **NOAA + Open-Meteo forecasts + ensemble member probabilities** to find mispriced temperature markets and places limit orders at calculated fair value.

## 🔴 Live Trading Status (Mar 9, 2026)

- **Portfolio:** ~$94 | Cash: ~$72 | All-time P/L: **+$0.18** (breakeven)
- **Initial Deposit:** $100.27
- **Weather P&L:** **+$8.59** | Non-weather: -$8.41
- **Strategy:** 100% Weather Arbitrage — **Phase A data-driven overhaul deployed**
- **PM2:** `polymarket-bot` **ONLINE** — schedule-aware scanning aligned to weather model releases
- **PM2:** `polymarket-redeem` **ONLINE** — auto-redeem resolved positions every 30 min (gas-free via Builder relayer)
- **Cities:** 12 (6 US + 6 international) — Wellington removed (worst city, -$14 P&L)
- **Forecast Models:** 119 ensemble members (ECMWF 51 + GFS 31 + ICON 40) + NOAA for US cities
- **Scan Timing:** 8 windows/day aligned to GFS/ECMWF model releases (15min post-publish) + 120min fallback
- **Laddering:** ❌ DISABLED (34 orders, 0 fills)
- **Max Exposure:** $80 | **Max/bucket:** $4 hard cap
- **Best Trade:** Seoul Feb 21 — +$31.87 (266% return)
- **Config:** 12% min edge (45% for narrow), 45% min probability, 25% Kelly, $4/bucket cap, $80 max exposure, 2¢ min market price
- **Bucket-type sizing:** Wide directional 1.5x, exact/narrow 0.2x — based on data showing wide bets +$32 vs exact -$28
- **Order pricing:** Taker (>25% edge), near-ask (>15% edge), maker (fallback) — replaces static 85% fair value
- **Auto-Redeem:** Resolved positions auto-claimed via `poly-web3` Builder relayer — gas-free, no manual UI interaction
- **v9 Fill Tracking (Mar 9):** `reconcile_with_api()` queries Polymarket activity API for ground-truth fill data, P&L, and outcomes every scan cycle (PR #15)
- **v8 Bug Fixes (Mar 7):** 8 correctness fixes from full weather module code review — see below
- **v7 Position Controls (Mar 5):** Per-bucket hard cap ($4), SH seasonal bias correction, auto-exit position monitor
- **v7.1:** Position monitor uses actual market resolution date + local city timezone for exit timing
- **CV-Kelly (v6):** Position sizing adjusted by ensemble coefficient of variation
- **Outcome Tracking:** WIN/LOSS/NO_FILL with P&L via CLOB API + temp unit conversion fix
- **Weekly Summary:** Telegram every Sunday midnight UTC


### Mar 9 — v9: Fill Tracking Reconciliation (PR #15)

`strategy_trades.json` showed 0 fills and 0 wins — the bot didn't know its own performance. Root cause: `check_fill_status()` skipped resolved trades, and the CLOB API is ephemeral (orders vanish after settlement).

**Fix:** Added `reconcile_with_api()` that queries `data-api.polymarket.com/activity` for ground-truth trade data every scan cycle. Matches local trades by condition_id/token_id, calculates proportional P&L from event totals, and updates outcomes. Also fixed the resolved-skip bug in `check_fill_status()`.

**Result:** Historical backfill updated all 62 trades. Before: 0 fills, 0 P&L. After: 39 fills, 39 P&L entries, 12 wins, 27 losses. Weather strategy is actually **+$8.59** — the bot just didn't know it.
### Mar 7 — v8: 8 Bug Fixes from Full Code Review (PR #12)

Eight correctness issues identified and fixed across the weather module:

1. **5-share guard removed** — v5 introduced a $1.00 dollar floor, but the old 5-share CLOB minimum was still running simultaneously, blocking valid low-price trades (e.g. Toronto $1.34 at $0.30/share = 4.46 shares). Replaced with 1-share sanity guard only.
2. **Ensemble bias correction** — `fetch_ensemble` was not applying `open_meteo_bias_f`/`open_meteo_bias_c`, while `parse_multi_model` was. Systematic scale offset fixed.
3. **Position monitor timezone fix** — Resolution window was hardcoded to 23:59 UTC. For Seoul (UTC+9), the actual local day ends at 14:59 UTC — window was opening 9h late. Now uses `chrono-tz` to compute end-of-local-day per city.
4. **Buffer check uses adjusted forecast** — Same-day observation adjustments updated `adjusted_forecast.high_temp` but the buffer check was still reading the raw `forecast.high_temp`.
5. **Disagreement counter renamed** — `markets_skipped_disagreement` renamed to `markets_high_disagreement`. Markets have not been skipped since v6 introduced CV-Kelly; the stat was actively misleading in scan summaries and weekly reports.
6. **City/forecast alignment** — `WEATHER_CITIES` included buenos-aires, ankara, and wellington but `default_cities_intl()` only had 4 cities, causing guaranteed `NO FORECAST` spam every cycle. Fixed with hybrid: added buenos-aires + ankara to config, removed wellington from discovery entirely (intentionally retired).
7. **Dead code removed** — `extreme_edge_size_factor()` was replaced by CV-Kelly in v6 and never called. Deleted.
8. **Discovery failure alert** — Zero markets found now fires a Telegram warning immediately instead of silently returning. Covers slug format changes or API timeouts.

### Mar 5 — v7: Position Controls + v7.1 Resolution Timing

Three targeted fixes based on real losses:
1. **Per-bucket hard cap ($4)** — CV-Kelly no longer concentrates $10+ in one bucket. Ankara-style over-sizing impossible.
2. **Southern Hemisphere bias correction** — 0.75x probability for cool bets in BA/Wellington/Sydney during Dec-Mar. Hard skip for weak-edge SH summer cool bets.
3. **Auto-exit position monitor** — Sells deteriorated positions (price < 50% cost, within 14h of resolution). Recovers capital instead of holding to zero.

v7.1 same-day patch: Uses actual `market_date` for resolution timing instead of position age proxy.

### Mar 1 — Market Discovery Reliability + Auto-Redemption

**Discovery reliability (PR #4):** Added 3-attempt retry with exponential backoff to all Polymarket API calls. 15s timeouts, 429 rate limit detection, 200ms inter-city delay to prevent cascading timeouts. Warns when cities are missing from discovery. No-forecast skips now log city, date, and available forecast dates for diagnosis.

**Auto-redemption (PRs #1-3):** Resolved positions now auto-claimed via Builder relayer (gas-free). Standalone Python script runs as PM2 cron every 30 min. Marks trades as `redeemed: true` in strategy_trades.json, frees capital and market slots for new trades. Telegram notification on each redemption.

**Station verification:** Chicago = KORD (O'Hare) ✅, Dallas = KDAL (Love Field) ✅ — confirmed against actual Polymarket resolution sources.

### Feb 28 — Phase A: Data-Driven Strategy Overhaul

Deep analysis of 197 on-chain transactions (Feb 12-28) revealed critical insights:
- **Wide "or higher" bets = only profitable category** (+$32.24, 86% ROI)
- **Exact temperature bets = worst category** (-$28.51, 0% resolved win rate)
- **Seoul = 61% of gross winnings** (+$30.48)
- **Wellington = worst city** (-$14.02 on $17.32) → REMOVED

Phase A changes: (1) config loosening to let bot trade again, (2) per-bucket file read perf fix, (3) bucket-type position sizing (wide 1.5x, exact 0.2x), (4) order-book-aware pricing (taker/near-ask/maker), (5) outcomes temperature unit conversion fix for US cities.

Station code fixes: Dallas KDFW→KDAL (Polymarket resolves on Love Field), Chicago coords to O'Hare airport.

### Feb 27 — Timezone Bug Fix + 7 Safety Tasks (All Implemented ✅)

Open-Meteo Ensemble API was aggregating daily max temperature using **UTC days**, not local timezone. This caused a $30 loss on Chicago Feb 28 — model showed 100% probability (119/119 members above 42°F) when actual was ~6%. Fixed by adding `&timezone={iana_tz}` to all Open-Meteo API calls. Also implemented 6 additional safety layers: NOAA cross-validation, model disagreement filter, probability clamping [0.02, 0.95], large edge position reduction, narrow bucket ensemble check, and diagnostic fields on trades. Laddering disabled (0 fills in 34 attempts).

## How It Works

```
Schedule-aware scan loop (8 windows/day aligned to GFS/ECMWF model releases):
1. Discover weather markets → 24+ markets across 12 cities (today + tomorrow + day after)
2. Fetch forecasts → NOAA (US) + Open-Meteo (all) + 119 ensemble members
3. Same-day markets → fetch real-time observations, adjust forecast if needed
4. Calculate probabilities → Ensemble voting (preferred) or normal distribution (fallback)
5. Find edges → Our probability vs market price (min 12% edge, 45% for narrow + buffer check)
6. Filter → Skip buckets below 2¢, min probability 45%, NOAA cross-validation
7. Size positions → Kelly criterion (25%) × bucket-type multiplier (wide 1.5x, exact 0.2x)
8. Price orders → Fetch order book: taker for >25% edge, near-ask for >15%, maker fallback
9. Place orders → BUY YES with order-book-aware pricing
10. Resolution → 1-2 days, slug-based Gamma API detection
11. Auto-redeem → Separate PM2 process checks every 30 min, redeems via Builder relayer (gas-free)
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
├── redeem_positions.py       # Auto-redeem resolved positions via Builder relayer (PM2 cron)
├── trade_outcomes.jsonl      # Redemption P/L log
├── config.toml               # Strategy configuration
└── .env                      # Wallet keys + Builder API creds (never committed)
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
~~Wellington~~ | ~~NZWN~~ | Removed — worst city (-$14.02, 0 wins)

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

# Laddering — micro-position diversification (v3)
enable_laddering = true
ladder_amount_per_bucket = 2.0   # $2 per bucket in ladder mode
ladder_max_buckets = 5           # Max buckets per market
ladder_min_model_prob = 0.05     # Min 5% model probability
ladder_max_market_price = 0.15   # Only buy buckets ≤15¢

[weather.scan_schedule]
# Aligned to GFS/ECMWF model release availability times (UTC)
model_release_hours = [3, 5, 9, 11, 15, 17, 21, 23]
fallback_interval_minutes = 120
post_release_delay_minutes = 15
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
- **Auto-redemption** — resolved positions auto-claimed via Builder relayer (gas-free, 30-min cycle)
- **Per-bucket hard cap** — $4 max per bucket regardless of Kelly output
- **SH seasonal correction** — reduces probability for cool bets in Southern Hemisphere during summer
- **Auto-exit monitor** — sells positions at >50% loss within 14h of resolution (uses local city timezone)
- **Discovery retry** — 3-attempt retry with backoff on API calls, 200ms inter-city delay, missing city warnings
- **Discovery failure alert** — Telegram notification if zero markets found (slug format change or API timeout)

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

# Auto-redeem (PM2 cron, every 30 min)
pm2 start redeem_positions.py --name polymarket-redeem --interpreter python --cron-restart "*/30 * * * *" --no-autorestart
pm2 save

# Check status
pm2 logs polymarket-bot --lines 20
pm2 logs polymarket-redeem --lines 20
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
