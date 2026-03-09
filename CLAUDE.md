# CLAUDE.md - Polymarket Weather Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **100% weather arbitrage** â€" uses NOAA + Open-Meteo forecasts + ensemble probabilities to find mispriced temperature markets and places limit orders at fair value.

## Current Status (Mar 9, 2026)
- **Portfolio:** ~$94 | Cash: ~$72 | All-time P/L: **+$0.18** (breakeven)
- **Weather P&L:** **+$8.59** (9 markets won, 34 lost
- **Non-weather P&L:** -$8.41 (manual trades, US/Iran biggest drag)
- **PM2:** `polymarket-bot` ONLINE — Phase A deployed, schedule-aware scanning
- **PM2:** `polymarket-redeem` ONLINE — auto-redeem every 30 min via Builder relayer (gas-free)
- **Telegram:** Trade alerts + weekly P&L summary (Sundays midnight UTC)
- **polymarket-arb:** STOPPED (sniper/arb strategies paused)
- **Max Exposure:** $80 (raised from $60 after batch sell recovered funds)
- **Laddering:** DISABLED (34 orders placed, 0 fills)
- **Wellington:** REMOVED — worst city (-$14.02 on $17.32, 0 wins)
- **v9:** DEPLOYED Mar 9 — fill tracking reconciliation with Polymarket activity API (PR #15)
- **v8:** DEPLOYED Mar 7 — 8 bug fixes from full weather module code review (PR #12)
- **v7:** DEPLOYED Mar 5 — per-bucket hard cap ($4), SH seasonal bias correction, auto-exit position monitor
- **v7.1:** DEPLOYED Mar 5 — position monitor uses actual hours-to-resolution instead of position age
- **Phase A:** DEPLOYED Feb 28 — config tuning, bucket-type sizing, order book pricing
- **Phase B (backlog):** BUY_NO support + cross-bucket mispricing — after 3 days of Phase A data (Mar 3)
- **Auto-Redeem:** LIVE Mar 1 — Phase 2 Builder relayer (gas-free), replaces manual UI claiming
- **Station Codes Verified:** Chicago=KORD (O'Hare) ✅, Dallas=KDAL (Love Field) ✅ — confirmed against Polymarket resolution sources


### Mar 9 — v9: Fill Tracking Reconciliation (PR #15)

**Problem:** strategy_trades.json showed 0 fills and 0 wins across 62 trades. Actual Polymarket account data (from data-api) showed 39 fills and +$8.59 weather P&L.

**Root Cause (3 bugs):**
1. `check_fill_status()` skipped `resolved` trades, but trades were marked resolved before fills were checked
2. CLOB API is ephemeral — orders disappear after settlement, so fill checks returned "unfilled" for settled trades
3. No reconciliation with ground-truth account activity data

**Fix 1: Remove resolved skip in check_fill_status()**
- Changed from `if trade.dry_run || trade.resolved { continue; }` to `if trade.dry_run { continue; }`
- Trades now get fill-checked regardless of resolved status

**Fix 2: Add reconcile_with_api() function**
- New async function queries `data-api.polymarket.com/activity?user={proxy_wallet}` (paginates up to 500 activities)
- Filters for temperature-related TRADE/REDEEM activities
- Groups by eventSlug to calculate per-event P&L (sell + redeem - buy)
- Matches local trades by condition_id/token_id with cost similarity check (within 50%)
- Updates fill_status, fill_confirmed, pnl (proportional to event P&L), and outcome (WIN/LOSS)
- Marks stale unmatched trades (>48h) as NO_FILL
- Runs once per scan cycle after check_fill_status()

**Historical backfill:** Separate PowerShell script reconciled all 62 existing trades against 249 API activities.
Before: 0 fills, 0 P&L, 0 outcomes. After: 39 fills, 39 P&L, 46 outcomes (12 WIN, 27 LOSS, 4 NO_FILL, 3 MANUAL_CLOSE).

**Account analysis (from Polymarket API):**
- 249 total activities (206 trades + 43 redemptions) across 55 markets
- Weather: +$8.59 P&L (165 trades). Non-weather: -$8.41 P&L (41 trades)
- Probability model insight: winning markets avg prob 0.462 vs losing avg prob 0.410 — model doesn't separate winners from losers
- Worst cities by P&L: Buenos Aires (-$37), Dallas (-$23), Seoul (-$11) — candidates for removal
- Best cities: NYC (+$8.71), Ankara (+$2.19), Chicago (+$6.43)
### Mar 7 — v8: Bug Fixes from Full Code Review (PR #12)

**8 correctness fixes, no new features:**

**Task 1: 5-share guard replaced with 1-share sanity guard**
- Both the $1.00 dollar floor (v5) AND the old `shares < 5.0` guard were running simultaneously
- Toronto case: $1.34 cost at $0.30/share = 4.46 shares → passed dollar floor, blocked by share guard
- Fixed in both main trade pass and laddering pass in `run_once()`

**Task 2: Ensemble bias correction**
- `parse_multi_model` applied `bias_f`/`bias_c`; `fetch_ensemble` did not
- With current config (bias=0.0) no live impact, but a systematic offset risk for future config changes
- Fixed in `open_meteo.rs` `fetch_ensemble()`

**Task 3: Position monitor uses local city timezone**
- Resolution window was hardcoded to `23:59 UTC`; weather markets resolve at end-of-local-day
- Seoul (UTC+9) local midnight = 14:59 UTC — exit window was opening 9h late
- Added `local_end_of_day_utc(date, city_name)` helper using `chrono-tz` crate
- Added `chrono-tz = "0.9"` to `Cargo.toml`

**Task 4: Buffer check uses `adjusted_forecast.high_temp`**
- Same-day observation adjustments modified `adjusted_forecast` but buffer check still read raw `forecast.high_temp`
- One-line fix in `run_once()`

**Task 5: Rename `markets_skipped_disagreement` → `markets_high_disagreement`**
- Markets have not been skipped on disagreement since v6 introduced CV-Kelly
- Counter still incremented but label said "skipped" — actively misleading in scan summary and weekly Telegram
- Renamed field, updated scan log format string, updated weekly summary label

**Task 6: Align `WEATHER_CITIES` with default config (hybrid)**
- `WEATHER_CITIES` in `markets.rs` had buenos-aires, ankara, wellington but `default_cities_intl()` only had 4 cities
- Every scan discovered markets for 3 cities with no forecasts → guaranteed `NO FORECAST` warnings
- Hybrid: added buenos-aires + ankara to `default_cities_intl()`; removed wellington from `WEATHER_CITIES` entirely

**Task 7: Delete dead `extreme_edge_size_factor()` function**
- Replaced by CV-Kelly in v6, never called — confirmed via grep

**Task 8: Telegram alert on zero markets discovered**
- Empty market scan returned silently with only a local `info!` log
- Now fires `warn!` + `notifier.send()` for immediate operator notification

**Task 9 (deferred):** Move `compute_edge_cv` before `CalibrationEntry` so eval rows get real `edge_cv` instead of `-1.0`. No trading impact — deferred to standalone pass.

### Mar 5 — v7: Position Controls (PR #10) + v7.1 Patch

**Three fixes for three real losses:**

**Task 1: Per-Bucket Hard Cap ($4)**
- `max_per_bucket_hard_cap = 4.0` in config, `kelly_size.min(config.max_per_bucket_hard_cap)` after CV-Kelly
- Prevents CV-Kelly from concentrating $10+ in a single bucket (Ankara $11.19 incident)
- Normal $1-3 bets unaffected, only caps when ensemble agreement drives cv_factor near 1.0
- Log: `BUCKET CAP APPLIED: {bucket} | cv-kelly=${before} -> capped at ${after}`

**Task 2: Southern Hemisphere Seasonal Bias Correction**
- `southern_hemisphere: bool` added to `City` struct (Buenos Aires, Wellington, Sydney, Sao Paulo = true)
- `southern_hemisphere_seasonal_correction()` — 0.75x probability multiplier for cool bets in SH cities Dec-Mar
- Hard skip: SH summer cool bets with edge < 0.25 after correction are blocked entirely
- Warm-outcome bets and Northern Hemisphere cities completely unaffected
- Logs: `SH SEASONAL CORRECTION` and `SH SUMMER COOL SKIP`

**Task 3: Automated Position Exit on Price Deterioration**
- New file: `src/weather/position_monitor.rs`
- Runs once per scan cycle before market evaluation
- Sells if: current price < 50% of cost basis AND resolution within 14 hours AND recoverable value >= $0.50
- Uses `market_date` field (v7.1) to compute actual hours-to-resolution (end-of-local-day per city timezone, v8)
- Places SELL via `orders::place_order()` with `Side::Sell` at current market price
- Marks trade as `auto_exited: true`, `outcome: "AUTO_EXIT"` in strategy_trades.json
- Telegram notification via `notify_sell()` with exit reason
- Old trades without `market_date` safely skipped

**v7.1 Patch (same day):**
- Changed position monitor from position-age proxy (`MIN_HOURS_OLD = 10h`) to actual resolution timing
- `market_date: Option<String>` added to `WeatherTrade`, populated at both constructor sites
- Resolution time = `market_date` at 23:59 local city time (converted to UTC via chrono-tz, v8 Task 3). Exits only when `hours_to_resolution < 14.0`

**New fields on WeatherTrade:**
- `neg_risk: bool` — whether market uses neg-risk exchange contract
- `auto_exited: bool` — whether position was auto-exited by monitor
- `market_date: Option<String>` — market resolution date (YYYY-MM-DD)

### Mar 5 — v6: CV-Adjusted Kelly Sizing + Calibration Logging (PR #7)

- CV-Kelly formula: `f_emp = base_kelly * (1 - CV).max(0.10)` — reduces position when ensemble members disagree
- `CalibrationEntry` logged per bucket evaluation to `calibration_log.jsonl`
- Fields: city, date, bucket, model probability, market price, edge, ensemble stats, outcome prediction

### Mar 1 — Market Discovery Reliability + Diagnostic Logging (PR #4)

**Problem:** Market discovery intermittently dropping cities (26 → 10 markets between scans). "No-forecast" markets not logged by name.

**Task 2 — No-forecast diagnostic logging:**
- Upgraded from `debug!` to `warn!` with full diagnostic info
- Logs: market name, city, date, and available forecast dates for that city
- Enables immediate diagnosis of timezone mismatches vs actual bugs
- Check with: `pm2 logs polymarket-bot --lines 300 | Select-String "NO FORECAST"`

**Task 1 — Discovery retry logic + rate limit handling:**
- 3-attempt retry with exponential backoff per API call
- Explicit 15s timeout on all Polymarket API requests
- 429 rate limit detection with 5s×attempt backoff
- 200ms delay between city/date lookups to prevent cascading timeouts
- Warning when market count drops below expected minimum (1 per city)
- Logs which specific cities are missing from discovery
- Check with: `pm2 logs polymarket-bot --lines 300 | Select-String "MISSING|Rate limited|LOW MARKET"`

**Task 3 — Station code verification:**
- Chicago: KORD (O'Hare) confirmed correct — Polymarket resolves on "Chicago O'Hare Intl Airport Station"
- Dallas: KDAL (Love Field) confirmed correct — Polymarket resolves on "Dallas Love Field Station"
- No code change needed

### Mar 1 — Auto-Redemption System (3 PRs)

**Problem:** Resolved positions required manual claiming on Polymarket UI. Capital sat idle until manually redeemed.

**Solution:** Standalone Python redemption script + Rust bot changes, running as separate PM2 process.

**PR #1 — Rust: WeatherTrade fields for redemption**
- Added `condition_id: Option<String>` — CTF condition ID needed for `redeemPositions()` call
- Added `redeemed: bool` — tracks whether USDC has been claimed on-chain
- Both use `#[serde(default)]` for backward compatibility with existing trades
- `load_existing_exposure()` excludes redeemed positions → frees capital
- `load_open_position_keys()` excludes redeemed positions → frees market slots

**PR #2 — Phase 1: Direct on-chain redemption (superseded by PR #3)**
- `redeem_positions.py` calling `redeemPositions()` via proxy wallet's `proxy()` function
- Required MATIC for gas on EOA — blocker since EOA had 0 MATIC

**PR #3 — Phase 2: Builder relayer upgrade (LIVE)**
- Replaced Phase 1 with `poly-web3` Builder relayer (gas-free, no MATIC needed)
- Uses `service.redeem(condition_id)` for per-trade redemption (Approach A)
- Builder API credentials (key/secret/passphrase) in `.env`
- Correct P/L: `shares × $1.00` for wins, `$0` for losses (not cost-based)
- Relayer health check at startup
- Rate limit cap: 5 redemptions per cycle
- Phase 1 code preserved as commented fallback

**Key architecture decisions:**
- Separate PM2 process (`polymarket-redeem`), NOT subprocess of Rust bot
- 30-min cron cycle via `--cron-restart "*/30 * * * *"` + `--no-autorestart`
- Reads `strategy_trades.json` for candidates: non-dry-run, not redeemed, has condition_id, >12h old
- Resolution check via Gamma API (outcomePrices + resolved flag)
- Updates `strategy_trades.json` with `redeemed: true` after successful redemption
- Logs to `trade_outcomes.jsonl` with per-trade P/L
- Telegram notification on each redemption

**Important distinctions:**
- `resolved` = market settled (set by Rust bot's `check_and_mark_resolved()`)
- `redeemed` = USDC claimed back (set by Python redemption script)
- A trade can be resolved but not yet redeemed — that's the window the script closes
- Both fields exclude from exposure, but only `redeemed` triggers P/L logging

**Proxy wallet details:**
- Type: Polymarket ProxyWallet (Solidity ^0.5.0, Magic/email type, signature_type=1)
- Address: 0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853
- Owner: EOA (0x7ec329D34D2c94456c015B236EBEc41d2a7B3Bce)
- Function: `proxy(ProxyCall[] memory calls) public payable onlyOwner`
- Factory: 0xaB45c5A4B0c941a2F231C04C3f49182e1A254052

**Note:** Old trades (pre-Mar 1) have `condition_id: null` — redemption script skips them. Only new trades going forward are eligible for auto-redemption.

### Feb 28 — Phase A: Data-Driven Strategy Overhaul

**Deep analysis of 197 on-chain transactions (Feb 12-28) revealed:**
- Wide "or higher" buckets: +$32.24 (86% ROI) — **only profitable category**
- Exact temperature bets: -$28.51 (0% resolved win rate) — **worst category**
- Seoul alone = 61% of weather gross winnings (+$30.48)
- Wellington = worst city (-$14.02, 0 wins)
- Order pricing at 85% fair value → 0% fill rate on ladders

**Phase A changes (commit 1498cbf):**
1. **Config tuning:** min_edge 0.15→0.12, min_market_price 0.05→0.02, min_our_probability 0.60→0.45, forecast_buffer 3/2→2/1°F/°C, min_edge_narrow 0.25→0.45, max_per_bucket 30→15, kelly_bankroll 120→90. Wellington REMOVED.
2. **Per-bucket file read fix:** load_open_position_keys() called ONCE before market loop (was 224x per scan)
3. **Bucket-type position sizing:** wide directional 1.5x, narrow/exact 0.2x, medium 0.5x
4. **Order-book-aware pricing:** taker for >25% edge with depth, near-ask for >15% edge, maker fallback
5. **Outcomes temp unit fix:** Open-Meteo archive returns Celsius; now converts for US cities

**Station code fixes (commit 16b0b89):**
- Dallas: KDFW → KDAL (Polymarket resolves on Love Field, not DFW — 20+ miles apart)
- Chicago: coordinates updated from city center to O'Hare airport

### Feb 27 — Config Changes + Critical Bug Identified

**Timezone bug discovered AND FIXED (all 7 tasks implemented):**
Open-Meteo Ensemble API was using UTC days for daily max temperature aggregation, NOT local timezone. For Chicago Feb 28, this meant the "daily max" included overnight UTC hours instead of actual daytime. Result: 119/119 ensemble members showed >42°F (mean=51.5°F) when reality was ~38°F. With `&timezone=America/Chicago`, 112/119 members are BELOW 42°F (mean=39.0°F). This caused a $30 bet at 100% model confidence on a ~6% actual probability event.

**All 7 safety tasks implemented:**
1. ✅ **TASK 1:** `timezone: String` on `City` struct, IANA timezones for all cities, `&timezone=` on ALL Open-Meteo API URLs
2. ✅ **TASK 2:** `cross_validate_with_noaa()` — caps ensemble prob when NOAA disagrees on threshold side
3. ✅ **TASK 3:** `models_disagree_too_much()` — skips market if Open-Meteo vs NOAA gap >8°F/4.5°C
4. ✅ **TASK 4:** `clamp_probability()` — all probs capped to [0.02, 0.95]
5. ✅ **TASK 5:** Large edge warning — reduces position size by up to 50% for edges >30%
6. ✅ **TASK 6:** `narrow_bucket_insufficient_ensemble()` — requires ≥5 ensemble members and ≥25% edge for ≤5°F range
7. ✅ **TASK 7:** Diagnostic fields on WeatherTrade (model_disagreement, etc.)

**Config changes applied:**
- `max_total_exposure`: $120 → $60 (conservative while validating fixes)
- `enable_laddering`: true → false (zero fills in 34 attempts)

### Feb 26 Upgrades (v3 â€" Competitive Edge)

Based on competitive analysis of 7+ active weather bots, 5 top traders ($2M+ combined profit), and 2 commercial platforms.

| Task | Detail |
|------|--------|
| **Model-release scan timing** | Replaced fixed 30-min cycle with schedule-aware scanning. Targets 15 min after GFS (00/06/12/18Z) and ECMWF (00/12Z) model releases â€" 8 windows/day. Fallback interval: 120 min. Config: `[weather.scan_schedule]` with `model_release_hours`, `fallback_interval_minutes`, `post_release_delay_minutes`. This is Hans323's edge ($1.1M profit from latency arbitrage on fresh forecasts). |
| **Micro-position laddering** | Second pass AFTER main edge detection. Spreads $2 bets across up to 5 adjacent cheap buckets (â‰¤15Â¢) where model_prob â‰¥5% and model_prob > market_price. Sorted by edge descending. Uses taker pricing for speed on cheap buckets. Trades logged as `LADDER_BUY_YES`. Config: `enable_laddering`, `ladder_amount_per_bucket`, `ladder_max_buckets`, `ladder_min_model_prob`, `ladder_max_market_price`. This is gopfan2/neobrother's strategy â€" diversification via law of large numbers. |

### Backlog (v3 Tasks 3-6 â€" remind Nazri March 5)
- Task 3: NWS data release awareness (avoid getting sniped by DSM/6HR bots)
- Task 4: Cross-bucket mispricing detection (0xf2e346ab's 73% win rate strategy)
- Task 5: Forecast change detection (delta between model runs)
- Task 6: Expand city coverage to 33+ (currently 13)
- Full instructions: `AGENT_INSTRUCTIONS_V3.md`

### Feb 22 Upgrades (v2 â€" Major)

| Task | Detail |
|------|--------|
| **Ensemble probabilities** | 119 members from 3 ensemble systems (ECMWF 51 + GFS 31 + ICON 40) via Open-Meteo Ensemble API. Non-parametric: each member votes for a bucket. Falls back to normal distribution if <20 members. |
| **Configurable Open-Meteo bias** | Removed hardcoded +1.0Â°F/+0.5Â°C warm bias. Now `open_meteo_bias_f` and `open_meteo_bias_c` in config.toml (default 0.0). |
| **Min market price filter** | `min_market_price = 0.05` â€" skips buckets priced below 5Â¢ where model is unreliable in tails. |
| **Quarter-Kelly** | `kelly_fraction` 0.40 â†' 0.25. Industry standard for prediction markets. |
| **Real-time observations** | `observations.rs` â€" fetches current temperature for same-day markets. Adjusts forecast upward if current temp > forecast high. |
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

### Feb 23 Upgrades (8 changes in one day)

| Task | Detail |
|------|--------|
| **Narrow bucket filter** | `min_edge_narrow = 0.25` â€" single-temp buckets (e.g. "18Â°C") require higher edge. Ensemble overestimates tight ranges. |
| **Min probability filter** | `min_our_probability = 0.60` â€" skip trades where model says <60%. Every winning trade had >0.75, every loss <0.60. |
| **Outcome tracking** | `fill_confirmed`, `outcome` (WIN/LOSS/NO_FILL), `pnl`, `resolution_temp`, `token_id` on WeatherTrade. Queries CLOB trades API for fill confirmation. |
| **Weekly Telegram summary** | `weekly_summary()` runs Sunday midnight UTC. Trades, W/L, win rate, P&L, best/worst, avg our_prob on wins vs losses. |
| **Supabase key** | Updated to new `sb_secret_` format (old JWT keys deprecated by Supabase). |

### Known Limitations
- Legacy trades (pre-Mar 1) have no `condition_id` — cannot be auto-redeemed, must claim manually or wait for 4-day exposure window expiry
- Legacy trades (pre-Feb 22) in strategy_trades.json have no `market_slug` — resolution falls back to substring matching
- `resolution_temp` placeholder — needs Weather Underground API key for actual lookup

## Strategy: Weather Arbitrage
- Scans 30+ weather markets across 13 cities (today + tomorrow + day_after_tomorrow)
- Fetches **119 ensemble members** from Open-Meteo Ensemble API (ECMWF + GFS + ICON)
- Also fetches NOAA (US) + Open-Meteo multi-model point forecasts as fallback
- **Ensemble probabilities** (preferred): each member votes for a bucket â€" non-parametric
- **Normal distribution** (fallback): when <20 ensemble members available
- Places LIMIT BUY orders at 85% of fair value (maker, zero fees)
- Kelly criterion sizing: 25% fraction, $90 bankroll, $15 max/bucket, $80 total exposure
- Bucket-type multiplier: wide directional 1.5x, exact/narrow 0.2x, medium 0.5x
- Order pricing: taker (>25% edge + depth), near-ask (>15% edge), maker (fallback)
- Min edge: 12% (wide) / 45% (narrow) | Min market price: 2Â¢ | Min probability: 45%
- Forecast buffer: 2Â°F / 1Â°C
- Same-day markets: real-time observation adjustment when current temp > forecast
- Resolution: 1-2 days

### Cities
- **US (Â°F, NOAA + Open-Meteo + Ensemble):** NYC (KLGA), Chicago (KORD), Miami (KMIA), Atlanta (KATL), Seattle (KSEA), Dallas (KDAL)
- **International (Â°C, Open-Meteo + Ensemble):** London (EGLC), Seoul (RKSS), Paris (LFPG), Toronto (CYYZ), Buenos Aires (SAEZ), Ankara (LTAC)
- **Removed:** Wellington (NZWN) â€" worst city by P&L (-$14.02 on $17.32, 0 wins)
- Station codes in parentheses = Weather Underground resolution stations

### Market Discovery
- Slug-based: `highest-temperature-in-{city}-on-{month}-{day}-{year}`
- Gamma API: `GET https://gamma-api.polymarket.com/events?slug={slug}`
- 3 dates checked: today, tomorrow, day_after_tomorrow
- `WEATHER_CITIES` in `markets.rs` must match `cities_us`/`cities_intl` in config.toml

## Architecture
```
polymarket-bot/
â"œâ"€â"€ src/
â"'   â"œâ"€â"€ weather/                # PRIMARY STRATEGY
â"'   â"'   â"œâ"€â"€ mod.rs              # WeatherConfig, City (with station codes), CityForecast (with ensemble_members)
â"'   â"'   â"œâ"€â"€ strategy.rs         # WeatherStrategy: run_once(), check_and_mark_resolved(), Kelly sizing
â"'   â"'   â"œâ"€â"€ forecast.rs         # calculate_probabilities() + calculate_probabilities_ensemble()
â"'   â"'   â"œâ"€â"€ markets.rs          # WEATHER_CITIES list, slug generation, 3-day Gamma API discovery
â"'   â"'   â"œâ"€â"€ noaa.rs             # NOAA API (api.weather.gov) â€" US cities
â"'   â"'   â"œâ"€â"€ open_meteo.rs       # Open-Meteo multi-model + fetch_ensemble() (119 members)
â"'   â"'   â""â"€â"€ observations.rs     # Real-time METAR observations for same-day markets
â"'   â"œâ"€â"€ api/client.rs           # PolymarketClient (Gamma + CLOB)
â"'   â"œâ"€â"€ auth/mod.rs             # L2 HMAC + EIP-712 signing
â"'   â"œâ"€â"€ orders/mod.rs           # place_order() â†' returns JSON with orderID
â"'   â"œâ"€â"€ notifications/mod.rs    # Telegram alerts
â"'   â""â"€â"€ main.rs                 # CLI entry point
â"œâ"€â"€ config.toml                 # Strategy configuration
â"œâ"€â"€ ecosystem.config.js         # PM2 config (polymarket-bot â†' weather)
â"œâ"€â"€ strategy_trades.json        # Trade log (crash-safe, per-trade writes)
â"œâ"€â"€ redeem_positions.py         # Auto-redeem via Builder relayer (PM2 cron, 30 min)
â"œâ"€â"€ trade_outcomes.jsonl        # Redemption P/L log (appended by redeem script)
â"œâ"€â"€ weather_multi_source.py     # Python multi-source forecasting (5 models + bias correction)
â""â"€â"€ .env                        # Wallet keys + Builder API creds + Telegram token (NEVER commit)
```

## Key Patterns

### WeatherTrade struct (strategy.rs)
```rust
pub struct WeatherTrade {
    timestamp, market_question, bucket_label, city,
    our_probability, market_price, edge, side,
    shares, price, cost, dry_run,
    resolved: bool,              // true when market closed (Gamma API)
    filled: bool,                // legacy field (always false)
    order_id: Option<String>,    // from CLOB response
    market_slug: Option<String>, // for reliable slug-based resolution
    fill_confirmed: bool,        // CLOB trades API confirmation
    outcome: Option<String>,     // "WIN", "LOSS", "NO_FILL", "UNKNOWN"
    pnl: Option<f64>,            // profit/loss in USDC
    resolution_temp: Option<f64>,// actual high temp (placeholder)
    token_id: Option<String>,    // for CLOB fill checking
    condition_id: Option<String>,// CTF condition ID for on-chain redemption (added Mar 1)
    redeemed: bool,              // USDC claimed on-chain via redeem script (added Mar 1)
    neg_risk: bool,              // market uses neg-risk exchange contract (added Mar 5)
    auto_exited: bool,           // position auto-exited by monitor (added Mar 5)
    market_date: Option<String>, // market resolution date YYYY-MM-DD (added Mar 5)
}
```

### run_once() flow
1. `position_monitor::check_and_exit_deteriorated_positions()` — auto-exit positions with price < 50% cost basis within 14h of resolution
2. `check_and_mark_resolved()` â€" queries Gamma API by slug for closed markets, frees exposure
2. Discover 30+ weather markets via slug patterns (3 dates Ã- 13 cities)
3. Fetch forecasts (NOAA + Open-Meteo + Ensemble) for 13 cities Ã- 3 days
4. For each market:
   a. Same-day? â†' fetch current observation, adjust forecast if current > forecast high
   b. Log resolution station (WUnderground code)
   c. Use ensemble probabilities (119 members) or fall back to normal distribution
5. For each bucket: min price check (â‰¥2Â¢) â†' dedup â†' buffer check (2Â°F/1Â°C) â†' NOAA cross-validation â†' min probability (â‰¥0.45) â†' edge check (wide â‰¥0.12, narrow â‰¥0.45) â†' Kelly sizing Ã— bucket-type multiplier â†' order-book-aware pricing â†' order
6. **Laddering pass** (if enabled): second scan for cheap buckets (â‰¤15Â¢) with model_prob > market_price. $2/bucket, up to 5 per market, sorted by edge. Logged as `LADDER_BUY_YES`.
7. `save_trade_log()` after each successful trade (with market_slug)

### Deduplication
- `placed_this_session: HashSet<String>` â€" keys are `"question|bucket"`
- Loaded from `strategy_trades.json` (non-dry-run, non-resolved, last 4 days) on startup
- Inserted after each successful order placement

### Exposure Tracking
- `load_existing_exposure()` sums cost of non-dry-run, non-resolved trades from last 4 days
- Decremented in-memory when `check_and_mark_resolved()` resolves a position
- `max_total_exposure=60` caps concurrent positions

## Critical Rules
- **NEVER commit .env** â€" wallet keys + Telegram token
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
# Weather bot (primary â€" PM2 managed)
pm2 start ecosystem.config.js --only polymarket-bot
pm2 logs polymarket-bot --lines 20
pm2 restart polymarket-bot

# Auto-redeem (PM2 cron, 30 min cycle)
pm2 start redeem_positions.py --name polymarket-redeem --interpreter python --cron-restart "*/30 * * * *" --no-autorestart
pm2 logs polymarket-redeem --lines 20

# Manual runs
polymarket-bot.exe weather --once          # Single live scan
polymarket-bot.exe weather --dry-run --once # Test without orders
python redeem_positions.py                  # Manual redemption run
```

## Workflow Orchestration

### 1. Plan Mode Default
- Enter plan mode for ANY non-trivial task (3+ steps or architectural decisions)
- If something goes sideways, STOP and re-plan immediately - don't keep pushing
- Use plan mode for verification steps, not just building
- Write detailed specs upfront to reduce ambiguity

### 2. Subagent Strategy
- Use subagents liberally to keep main context window clean
- Offload research, exploration, and parallel analysis to subagents
- For complex problems, throw more compute at it via subagents
- One task per subagent for focused execution

### 3. Self-Improvement Loop
- After ANY correction from the user: update `tasks/lessons.md` with the pattern
- Write rules for yourself that prevent the same mistake
- Ruthlessly iterate on these lessons until mistake rate drops
- Review lessons at session start for relevant project

### 4. Verification Before Done
- Never mark a task complete without proving it works
- Diff behavior between main and your changes when relevant
- Ask yourself: "Would a staff engineer approve this?"
- Run tests, check logs, demonstrate correctness

### 5. Demand Elegance (Balanced)
- For non-trivial changes: pause and ask "is there a more elegant way?"
- If a fix feels hacky: "Knowing everything I know now, implement the elegant solution"
- Skip this for simple, obvious fixes - don't over-engineer
- Challenge your own work before presenting it

### 6. Autonomous Bug Fixing
- When given a bug report: just fix it. Don't ask for hand-holding
- Point at logs, errors, failing tests - then resolve them
- Zero context switching required from the user
- Go fix failing CI tests without being told how

## Task Management

1. **Plan First:** Write plan to `tasks/todo.md` with checkable items
2. **Verify Plans:** Check in before starting implementation
3. **Track Progress:** Mark items complete as you go
4. **Explain Changes:** High-level summary at each step
5. **Document Results:** Add review section to `tasks/todo.md`
6. **Capture Lessons:** Update `tasks/lessons.md` after corrections

## Core Principles

- **Simplicity First:** Make every change as simple as possible. Impact minimal code.
- **No Laziness:** Find root causes. No temporary fixes. Senior developer standards.
- **Minimal Impact:** Changes should only touch what's necessary. Avoid introducing bugs.

## Security - CRITICAL

- NEVER commit tokens/keys/secrets to git. This has caused GitHub alerts TWICE on mt5-trading (Feb 19 + Feb 24).
- ALWAYS use env vars or `.env` (gitignored) for credentials
- NEVER use `git add -A` - always `git add <specific files>` and review staged files
- One-off scripts with credentials belong in gitignored folders, not the repo
- API keys, wallet private keys, Supabase keys → `.env` ONLY
