# Agent Instructions: Polymarket Weather Bot — Bug Fixes & Improvements

> Based on full code review of `strategy.rs`, `forecast.rs`, and `config.toml`  
> Fix in order of priority. Do not skip ahead — Bug 1 and Bug 2 must be fixed before touching config.

---

## 🔴 BUG 1 (CRITICAL): Add Per-Position Deduplication in `strategy.rs`

**Problem:** Before placing any order, the code only checks `total_exposure >= max_total_exposure`. It never checks whether a trade for this exact `market_question + bucket_label` combination already exists in `strategy_trades.json`. This is why Seoul 14°C got bought 3 times — each scan cycle saw remaining exposure headroom and re-entered the same position.

**Fix:** In `WeatherStrategy`, add a `placed_this_session: HashSet<String>` field, AND check `strategy_trades.json` for existing open positions before placing any order.

**In the struct definition, add:**
```rust
pub struct WeatherStrategy {
    // ... existing fields ...
    placed_this_session: std::collections::HashSet<String>,
}
```

**In `new()`, initialize it:**
```rust
placed_this_session: std::collections::HashSet::new(),
```

**Add a helper method to load already-traded positions:**
```rust
fn load_open_position_keys() -> std::collections::HashSet<String> {
    let trades: Vec<WeatherTrade> = match std::fs::read_to_string("strategy_trades.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => return std::collections::HashSet::new(),
    };

    trades.iter()
        .filter(|t| !t.dry_run)
        .filter(|t| {
            // Only consider trades from last 4 days (weather markets are 1-2 days out)
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&t.timestamp) {
                let days_ago = (Utc::now() - ts.with_timezone(&Utc)).num_days();
                days_ago <= 4
            } else {
                false
            }
        })
        .map(|t| format!("{}|{}", t.market_question, t.bucket_label))
        .collect()
}
```

**In `new()`, load existing open keys into placed_this_session:**
```rust
placed_this_session: Self::load_open_position_keys(),
```

**In `run_once()`, inside the bucket loop, BEFORE the edge check, add:**
```rust
let position_key = format!("{}|{}", market.question, bucket.label);
if self.placed_this_session.contains(&position_key) {
    debug!("SKIP: Already have position in {} | {}", market.question, bucket.label);
    continue;
}
```

**After a successful order placement (both live and dry_run), register the key:**
```rust
self.placed_this_session.insert(position_key);
```

---

## 🔴 BUG 2 (CRITICAL): Save Trade Log Per-Trade, Not Once at End in `strategy.rs`

**Problem:** `save_trade_log()` is called once at the very end of `run_once()`. If the process crashes or is restarted mid-cycle after placing orders but before the save completes, those trades are lost from the log. The next startup's `load_existing_exposure()` won't count them, causing the bot to re-enter the same positions.

**Fix:** Call `save_trade_log()` immediately after each successful order placement. Change the `trades: Vec<WeatherTrade>` approach — append and save atomically per trade.

**Replace the current pattern** (where you push to `self.trades` and save at end) with an immediate save after each trade. After the line `self.trades.push(trade);`, add:

```rust
self.trades.push(trade);
// Save immediately after each trade to prevent data loss on crash
if let Err(e) = self.save_trade_log() {
    error!("Failed to save trade log: {}", e);
}
```

This way, even if the bot crashes 10 seconds later, the trade is persisted and the next run will correctly load it and skip the position.

---

## 🟠 BUG 3 (MEDIUM): Fix `load_existing_exposure()` Window from 2 Days to 4 Days in `strategy.rs`

**Problem:** `days_ago <= 2` is used to identify potentially-open trades. But a trade placed on day 0 for a market resolving on day 2 will fall out of this window before resolution if the bot restarts on day 3. This causes the bot to stop counting that exposure and potentially re-enter.

**Fix:** Change the window to 4 days to safely cover the full weather market lifecycle.

Find this line in `load_existing_exposure()`:
```rust
days_ago <= 2 // trades from last 2 days could be unresolved
```

Change to:
```rust
days_ago <= 4 // weather markets can be up to 2 days out; 4-day window is safe
```

---

## 🟠 BUG 4 (MEDIUM): Fix Kelly Bankroll to Use Actual Balance in `strategy.rs`

**Problem:** `calculate_kelly_size()` uses `self.config.max_total_exposure` ($20) as the bankroll for Kelly calculations. Kelly should be calculated on your actual available capital, not the config cap. With $84 cash available but a $20 bankroll in Kelly math, you're undersizing every position.

**Fix:** Add an `account_balance: f64` field to `WeatherStrategy` and pass the actual USDC balance fetched at startup. For now, a simpler interim fix: make the Kelly bankroll the larger of `max_total_exposure` and a configurable `kelly_bankroll` config value.

**In `config.toml`, add under `[weather]`:**
```toml
kelly_bankroll = 100.0   # Your actual capital for Kelly sizing purposes
```

**In `WeatherConfig` struct (in `mod.rs` or wherever it's defined), add:**
```rust
pub kelly_bankroll: f64,
```

**In `calculate_kelly_size()`, change:**
```rust
let bankroll = self.config.max_total_exposure;
```
to:
```rust
let bankroll = self.config.kelly_bankroll;
```

This keeps position cap limits separate from Kelly bankroll calculation.

---

## 🟡 IMPROVEMENT 1: Raise `max_total_exposure` in `config.toml`

**Problem:** `max_total_exposure = 20.0` = `max_per_bucket = 20.0`. The bot can hold exactly ONE position at a time. With $84 cash available, 76% of capital is always idle.

**Fix:** After Bug 1 and Bug 2 are fixed (so you won't over-expose), update `config.toml`:

```toml
[weather]
min_edge = 0.15              # Keep this — it's working well
max_per_bucket = 20.0        # Keep single position cap at $20
max_total_exposure = 60.0    # Allow up to 3 concurrent positions across different cities
kelly_fraction = 0.40
kelly_bankroll = 100.0       # Add this (from Bug 4 fix)
forecast_buffer_f = 3.0
forecast_buffer_c = 2.0
```

With this change, the bot can hold Seoul + Paris + London simultaneously if all three have 15%+ edge on the same day.

---

## 🟡 IMPROVEMENT 2: Expand City Coverage in `config.toml`

**Problem:** 13 cities × 2 days = 26 potential markets per cycle. At 15% min edge, you're finding ~1 trade per 2-3 days. More cities = more at-bats for the same edge quality.

**Fix:** Add to `config.toml`:

```toml
cities_us = ["nyc", "chicago", "miami", "atlanta", "seattle", "dallas", 
             "los-angeles", "denver", "phoenix", "houston", "boston", "minneapolis"]

cities_intl = ["london", "seoul", "paris", "toronto", "buenos-aires", "ankara", 
               "wellington", "tokyo", "sydney", "singapore", "dubai", "berlin", "mumbai"]
```

**Before adding each city**, verify:
1. Open-Meteo has ensemble data for it (test with `weather_multi_source.py`)
2. Polymarket actually lists that city in their weather markets (spot-check on polymarket.com)

Do not add cities that aren't on Polymarket — the market discovery slug pattern must match.

---

## 🟡 IMPROVEMENT 3: Make NOAA Warm Bias Configurable in `strategy.rs` + `config.toml`

**Problem:** `biased_temp = noaa_temp + 1.0` — this is hardcoded with no explanation of where +1.0 came from. If it's wrong for certain cities or seasons, every US trade is miscalibrated.

**Fix:** Add to `config.toml`:
```toml
noaa_warm_bias_f = 1.0   # Empirical warm bias correction for NOAA forecasts
```

In `strategy.rs`, replace:
```rust
let biased_temp = noaa_temp + 1.0;
```
with:
```rust
let biased_temp = noaa_temp + self.config.noaa_warm_bias_f;
```

---

## 🟢 IMPROVEMENT 4: Auto-Redeem After Resolution

**Problem:** Paris 10°C had to be manually redeemed. Capital sits idle until someone does it.

**Add a new method to `WeatherStrategy`:**
```rust
pub async fn redeem_resolved_positions(&self, client: &PolymarketClient) -> Result<u32> {
    // 1. Read strategy_trades.json for non-dry-run trades older than 24h
    // 2. For each, query Polymarket CLOB for the position's current token value
    // 3. If token value is 1.0 (resolved YES) or 0.0 (resolved NO), redeem it
    // 4. Mark trade as resolved in strategy_trades.json
    // Returns number of positions redeemed
}
```

Call this at the start of `run_once()` before the main scan loop. This frees up capital automatically and keeps `total_exposure` accurate.

---

## Summary: Tell Your Agent Exactly This

```
Work through the following tasks IN ORDER. Do not skip ahead. 
Each task references specific files and line numbers from the codebase.

TASK 1 — strategy.rs:
  Add a HashSet<String> field `placed_this_session` to WeatherStrategy.
  In new(), populate it by calling a new helper function load_open_position_keys()
  that reads strategy_trades.json and returns a set of "market_question|bucket_label"
  keys for all non-dry-run trades from the last 4 days.
  In run_once(), before the edge check for each bucket, build the position_key
  string and skip (continue) if it exists in placed_this_session.
  After a successful order placement, insert the key into placed_this_session.

TASK 2 — strategy.rs:
  Move save_trade_log() to be called immediately after each successful order
  placement (both live and dry_run branches), instead of once at the end of run_once().

TASK 3 — strategy.rs:
  In load_existing_exposure(), change `days_ago <= 2` to `days_ago <= 4`.

TASK 4 — config.toml + strategy.rs + WeatherConfig struct:
  Add `kelly_bankroll = 100.0` to config.toml [weather] section.
  Add `kelly_bankroll: f64` to WeatherConfig struct.
  In calculate_kelly_size(), change `let bankroll = self.config.max_total_exposure`
  to `let bankroll = self.config.kelly_bankroll`.

TASK 5 — config.toml only (after Tasks 1-4 are complete and tested):
  Change max_total_exposure from 20.0 to 60.0.
  Add kelly_bankroll = 100.0.
  Add noaa_warm_bias_f = 1.0.
  Expand cities_us to include: los-angeles, denver, phoenix, houston, boston, minneapolis.
  Expand cities_intl to include: tokyo, sydney, singapore, dubai, berlin.
  Only add cities that actually appear in Polymarket weather markets.

TASK 6 — strategy.rs:
  Replace the hardcoded `noaa_temp + 1.0` with `noaa_temp + self.config.noaa_warm_bias_f`.
  Add the corresponding field to WeatherConfig.

After all tasks: run with --dry-run --once and verify in the logs that:
  - The same market/bucket does NOT appear twice in one scan
  - Trade log is written immediately after each order attempt
  - Kelly sizes are calculated on $100 bankroll, not $20
```

---

## What NOT to Change

- **`min_edge = 0.15`** — keep it. Your two big wins (Paris +144%, Seoul +266%) both came through this filter. Do not lower it.
- **`kelly_fraction = 0.40`** — keep it. Aggressive but appropriate for 15%+ edge weather trades.
- **`forecast_buffer_f = 3.0` / `forecast_buffer_c = 2.0`** — keep it. This filter correctly skips borderline bets.
- **`forecast.rs` consensus logic** — it's well-designed. The multi-model blending (0.3 base + 0.7 consensus) is sophisticated and correct. Do not rewrite it.
- **The 85% limit order price** (`our_prob * 0.85`) — this is working. It's why you get fills on thinly-traded weather books without paying full mid.

---

*Generated Feb 21, 2026 — based on full code review of strategy.rs, forecast.rs, config.toml*
