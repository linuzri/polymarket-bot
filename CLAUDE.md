# CLAUDE.md - Polymarket Weather Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **100% weather arbitrage** â€" uses NOAA + Open-Meteo forecasts + ensemble probabilities to find mispriced temperature markets and places limit orders at fair value.

## Current Status (Feb 27, 2026)
- **Portfolio:** ~$119 USDC | All-time P&L: +$18.22
- **Open Positions:** Chicago 42°F bet ($30 at risk — NOAA says 38°F, model said 100% wrong due to timezone bug)
- **PM2:** `polymarket-bot` ONLINE — schedule-aware scanning aligned to model releases
- **Telegram:** Trade alerts + weekly P&L summary (Sundays midnight UTC)
- **polymarket-arb:** STOPPED (sniper/arb strategies paused)
- **Max Exposure:** $60 (reduced from $120 on Feb 27)
- **Laddering:** DISABLED (34 orders placed, 0 fills, 0 profit — fair-value limits never matched in illiquid markets)
- **Roadmap:** WATCH & WAIT — monitoring v3 changes for 1 week, then Tasks 3-6

### Feb 27 — Config Changes + Critical Bug Identified

**Timezone bug discovered (ROOT CAUSE — NOT YET FIXED):**
Open-Meteo Ensemble API uses UTC days for daily max temperature aggregation, NOT local timezone. For Chicago Feb 28, this means the "daily max" includes overnight UTC hours (evening local time) instead of actual daytime. Result: 119/119 ensemble members showed >42°F (mean=51.5°F) when reality was ~38°F. With `&timezone=America/Chicago`, 112/119 members are BELOW 42°F (mean=39.0°F). This caused a $30 bet at 100% model confidence on a ~6% actual probability event.

**7 fix tasks identified (pending implementation via coding agent):**
1. **TASK 1 (EMERGENCY):** Add `timezone: String` to `City` struct, populate IANA timezones for all 24 US + 13 intl cities, add `&timezone={city.timezone}` to ALL Open-Meteo API URLs with `daily=`
2. **TASK 2:** Cross-validate with NOAA — cap ensemble prob when NOAA disagrees on threshold side
3. **TASK 3:** Model disagreement filter — skip market if Open-Meteo vs NOAA gap >8°F/4.5°C
4. **TASK 4:** Probability clamping — all probs capped to [0.02, 0.95]
5. **TASK 5:** Large edge warning — reduce position size by up to 50% for edges >30%
6. **TASK 6:** Narrow bucket filter — require ≥5 ensemble members and ≥25% edge for ≤5°F range buckets
7. **TASK 7:** Diagnostic fields on WeatherTrade (open_meteo_mean, noaa_temp, ensemble stats)

**Config changes applied:**
- `max_total_exposure`: $120 → $60 (reduced risk while timezone bug unfixed)
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
- **⚠️ TIMEZONE BUG (CRITICAL):** Open-Meteo Ensemble API returns UTC-aggregated daily max temps. Must add `&timezone={iana_tz}` to all API calls with `daily=` parameter. Without this, model probabilities are WRONG for non-UTC cities. $30 loss on Chicago Feb 28 bet caused by this.
- No auto-redeem — PolymarketClient has no redeem/settle/merge methods
- Legacy trades (pre-Feb 22) in strategy_trades.json have no `market_slug` — resolution falls back to substring matching
- `resolution_temp` placeholder — needs Weather Underground API key for actual lookup

## Strategy: Weather Arbitrage
- Scans 30+ weather markets across 13 cities (today + tomorrow + day_after_tomorrow)
- Fetches **119 ensemble members** from Open-Meteo Ensemble API (ECMWF + GFS + ICON)
- Also fetches NOAA (US) + Open-Meteo multi-model point forecasts as fallback
- **Ensemble probabilities** (preferred): each member votes for a bucket â€" non-parametric
- **Normal distribution** (fallback): when <20 ensemble members available
- Places LIMIT BUY orders at 85% of fair value (maker, zero fees)
- Kelly criterion sizing: 25% fraction, $100 bankroll, $20 max/bucket, $60 total exposure
- Min edge: 15% | Min market price: 5Â¢ | Forecast buffer: 3Â°F / 2Â°C
- Same-day markets: real-time observation adjustment when current temp > forecast
- Resolution: 1-2 days

### Cities
- **US (Â°F, NOAA + Open-Meteo + Ensemble):** NYC (KLGA), Chicago (KORD), Miami (KMIA), Atlanta (KATL), Seattle (KSEA), Dallas (KDFW)
- **International (Â°C, Open-Meteo + Ensemble):** London (EGLC), Seoul (RKSS), Paris (LFPG), Toronto (CYYZ), Buenos Aires (SAEZ), Ankara (LTAC), Wellington (NZWN)
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
â"œâ"€â"€ weather_multi_source.py     # Python multi-source forecasting (5 models + bias correction)
â""â"€â"€ .env                        # Wallet keys + Telegram token (NEVER commit)
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
}
```

### run_once() flow
1. `check_and_mark_resolved()` â€" queries Gamma API by slug for closed markets, frees exposure
2. Discover 30+ weather markets via slug patterns (3 dates Ã- 13 cities)
3. Fetch forecasts (NOAA + Open-Meteo + Ensemble) for 13 cities Ã- 3 days
4. For each market:
   a. Same-day? â†' fetch current observation, adjust forecast if current > forecast high
   b. Log resolution station (WUnderground code)
   c. Use ensemble probabilities (119 members) or fall back to normal distribution
5. For each bucket: min price check â†' min probability (â‰¥0.60) â†' dedup â†' buffer check â†' edge check (narrow buckets need 0.25) â†' Kelly sizing â†' order
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
# Weather (primary â€" PM2 managed)
pm2 start ecosystem.config.js --only polymarket-bot
pm2 logs polymarket-bot --lines 20
pm2 restart polymarket-bot

# Manual runs
polymarket-bot.exe weather --once          # Single live scan
polymarket-bot.exe weather --dry-run --once # Test without orders
polymarket-bot.exe weather                  # Continuous loop (use PM2 instead)
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
