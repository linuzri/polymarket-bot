# 🎯 Polymarket Trading Bot

Automated prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Focused on **risk-free sniper trading** — buying near-certain outcomes at 95-99.9¢ and collecting $1.00 on resolution.

## 🔴 Live Trading Status

- **Balance:** ~$1.64 USDC cash + ~$87 in positions
- **Initial Deposit:** $100.27
- **Strategy:** 4-strategy bot (arb + multi-arb + sniper + hybrid take-profit)
- **Process:** 1 (`polymarket-arb` PM2 id:13)
- **Telegram notifications:** Active (trades, hourly portfolio summary, errors)

## Architecture

```
polymarket-arb (PM2 id:13)
├── 2-Outcome Arbitrage — YES+NO < $0.985 spread detection
├── Multi-Outcome Arbitrage — 3-30 outcome events, sum of YES asks < $1.00
├── Resolved-Market Sniper — buy 90-99.9¢ near-certain outcomes
│   ├── Fast-resolving focus (30-day max resolution)
│   ├── 3 market fetches (top volume, 24h volume, soonest-ending)
│   ├── Tick-size-aware pricing (0.001 and 0.0001 tick markets)
│   ├── Dynamic exposure limit (from balance)
│   ├── Duplicate tracking (by condition_id)
│   └── Score: profit_pct / days_to_resolve
├── Hybrid Take-Profit — sell sniper positions at 99¢+ bid
├── Weather Arbitrage — temperature forecast vs market price edge detection
│   ├── NOAA forecasts for US cities (NYC, Chicago, Miami, Atlanta, Seattle, Dallas)
│   ├── Open-Meteo ensemble forecasts for international cities (London, Seoul, Paris, Toronto)
│   ├── Normal distribution probability model with forecast uncertainty
│   ├── Kelly criterion position sizing (fractional, 25% default)
│   ├── Min 15% edge threshold, max $10/bucket, $50 total exposure
│   └── Automatic weather market discovery from Gamma API
└── Hourly Portfolio Summary → Telegram
```

## Features

### Active (Risk-Free Focus)
- **Resolved-Market Sniper** — Buys obvious outcomes at 90-99.9¢, holds to resolution at $1.00
- **Hybrid Take-Profit** — Sells sniper positions early at 99¢+ to free capital faster
- **Multi-Outcome Arbitrage** — Buys all YES outcomes in events where sum < $1.00 (guaranteed profit)
- **2-Outcome Arbitrage** — Scans for YES+NO price gaps where both sides sum < $0.985
- **Fast-Resolving Focus** — Only targets markets resolving within 30 days
- **Tick-Size-Aware Pricing** — Fetches each market's `minimum_tick_size` from CLOB API
- **Dynamic Exposure Management** — Fetches real balance each cycle, adjusts limits
- **Hourly Portfolio Summary** — Automated Telegram updates with positions, P/L, and stats
- **Telegram Alerts** — Real-time notifications for every trade placed

### Available (Paused)
- **Two-Tier AI Evaluator** — Haiku screens → Sonnet deep-evaluates (paused to focus on risk-free)
- **Contrarian Bet Support** — Sonnet-confirmed signals at $0.03+ prices
- **Portfolio Tracking** — Open/resolved positions, auto-sell (TP/SL), edge re-evaluation
- **Paper Trading** — Practice with virtual balance

## Sniper Strategy

The Anjun-inspired strategy:
1. Scan ~400 fast-resolving markets every 30 seconds (30-day max resolution)
2. Find outcomes priced 90-99.9% certain (near-resolved)
3. Buy the winning side at market ask price
4. Hold to resolution → collect $1.00 per share
5. **OR** sell early at 99c+ if bid surges (hybrid take-profit)
6. Profit = $1.00 - buy price (0.1% to 10% per trade)

**Target markets:** Sports outcomes, crypto daily prices, near-term politics, upcoming events. Filtered to resolve within 30 days.

**Tick size matters:** Political markets use 0.001 tick (3 decimal prices = $0.999 possible). Sports use 0.01 tick (max $0.99).

### Risk Profile
- **Near risk-free** — buying outcomes with 90-99.9% implied probability
- **Black swan risk** — tiny chance the "impossible" happens
- **Capital lockup** — mitigated by 30-day max + hybrid take-profit at 99c+
- **Best at scale** — Anjun made $1M with $200K positions; at $92, returns are pennies

## Weather Strategy

Exploits mispriced temperature buckets on Polymarket weather markets by comparing forecast probabilities against market prices.

### How it works
1. **Discover** weather markets from Gamma API (tag "weather", question contains "temperature")
2. **Fetch forecasts** — NOAA (US cities) and Open-Meteo ensemble (international cities)
3. **Build probability distribution** — Normal distribution centered on forecast high, σ based on forecast horizon (2-3°F day-1, 4-5°F day-2)
4. **Compare** our probability vs market price for each temperature bucket
5. **Trade** when edge ≥ 15% — Kelly criterion sizing, max $10/bucket, $50 total exposure

### Run
```bash
# Weather strategy (standalone, loops every 30 min)
./target/release/polymarket-bot weather

# Single scan (no loop)
./target/release/polymarket-bot weather --once

# Dry run
./target/release/polymarket-bot weather --dry-run
```

### Config (config.toml)
```toml
[weather]
enabled = true
scan_interval_secs = 1800
min_edge = 0.15
max_per_bucket = 10.0
max_total_exposure = 50.0
kelly_fraction = 0.25
cities_us = ["nyc", "chicago", "miami", "atlanta", "seattle", "dallas"]
cities_intl = ["london", "seoul", "paris", "toronto"]
```

## Quick Start

### Prerequisites
- [Rust](https://rustup.rs/) (1.75+)
- Polymarket account with funds deposited

### Setup
```bash
cp .env.example .env
# Edit .env with your wallet keys and API credentials
cargo build --release
```

### Run
```bash
# Arb + Sniper + Multi-Arb + Take-Profit scanner (primary)
./target/release/polymarket-bot arb

# AI strategy bot (paused, available if needed)
./target/release/polymarket-bot run

# View portfolio
cmd /c ./target/release/polymarket-bot portfolio

# Paper trading mode
./target/release/polymarket-bot paper
```

### PM2 (Production)
```bash
pm2 start ecosystem.config.js --only polymarket-arb
```

## Configuration

### Sniper Constants (src/arbitrage/mod.rs)
| Constant | Value | Description |
|----------|-------|-------------|
| SNIPER_MIN_PRICE | 0.90 | Minimum price (90% certainty) |
| SNIPER_MAX_PRICE | 0.999 | Maximum price (99.9% for 0.001 tick) |
| SNIPER_MAX_SIZE | $25 | Max USD per trade |
| SNIPER_MIN_VOLUME | $50K | Min market volume |
| SNIPER_MAX_DAYS_TO_RESOLVE | 30 | Skip markets > 30 days out |
| TAKE_PROFIT_MIN_BID | 0.99 | Sell early if bid >= 99c |
| MULTI_ARB_MIN_PROFIT_PCT | 0.5% | Min profit for multi-outcome arb |
| MULTI_ARB_MAX_SIZE | $25 | Max total investment per multi-arb |

### Strategy Config (strategy_config.json)
AI evaluator settings (when enabled): scan interval, max trade size, Kelly fraction, confidence thresholds.

## Key Files
| File | Purpose |
|------|---------|
| `src/arbitrage/mod.rs` | Arb scanner + sniper logic |
| `src/orders/mod.rs` | Order building, tick-size-aware signing |
| `src/api/client.rs` | CLOB API client (orders, books, tick sizes) |
| `src/notifications/mod.rs` | Telegram notifications |
| `src/portfolio/mod.rs` | Position tracking |
| `src/strategy/` | AI evaluator (paused) |
| `ecosystem.config.js` | PM2 process config |
| `portfolio_state.json` | Persisted portfolio state |
| `strategy_config.json` | AI strategy config |

## Wallet Setup
- **EOA Wallet:** Signs transactions (POLY_WALLET_ADDRESS)
- **Proxy Wallet:** Holds funds, is maker (POLY_PROXY_WALLET)
- **Auth:** EIP-712 signatures, signature_type=1 for proxy wallets
- **CLOB API keys:** Deterministically derived from private key (cannot be rotated without new wallet)

## Commit History (Recent)
- `e4cc065` — Hybrid take-profit (sell sniper at 99c+)
- `c814edb` — Multi-outcome arbitrage scanner
- `ebe502d` — Fast-resolving market focus (30-day max)
- `94df988` — Hourly portfolio summary to Telegram
- `5b1dcc5` — Tick-size-aware pricing (unlock 99.9¢)
- `16baebd` — Resolved-market sniper
- `6a0dfe4` — Arbitrage scanner
- `fa2cb47` — Two-tier AI evaluator + contrarian filter

## License
Private repository.
