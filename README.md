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
1. Scan 300+ active markets every 30 seconds
2. Find outcomes priced 95-99.9% certain (near-resolved)
3. Buy the winning side at market ask price
4. Wait for resolution → collect $1.00 per share
5. Profit = $1.00 - buy price (0.1% to 5% per trade)

**Target markets:** 2028 presidential candidates, Fed nominees, expired event deadlines, sports longshots, absurd outcomes.

**Tick size matters:** Political markets use 0.001 tick (3 decimal prices = $0.999 possible). Sports use 0.01 tick (max $0.99).

### Risk Profile
- **Near risk-free** — buying outcomes with 95-99.9% implied probability
- **Black swan risk** — tiny chance the "impossible" happens
- **Capital lockup** — some markets don't resolve for months/years
- **Best at scale** — Anjun made $1M with $200K positions; at $92, returns are pennies

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
# Arb + Sniper scanner (primary)
./target/release/polymarket-bot arb

# AI strategy bot (paused, available if needed)
./target/release/polymarket-bot run
```

### PM2 (Production)
```bash
pm2 start ecosystem.config.js --only polymarket-arb
```

## Configuration

### Sniper Constants (src/arbitrage/mod.rs)
| Constant | Value | Description |
|----------|-------|-------------|
| SNIPER_MIN_PRICE | 0.95 | Minimum price (95% certainty) |
| SNIPER_MAX_PRICE | 0.999 | Maximum price (99.9% for 0.001 tick) |
| SNIPER_MAX_SIZE | $25 | Max USD per trade |
| SNIPER_MIN_VOLUME | $100K | Min market volume |
| MAX_SNIPER_EXPOSURE | $70 | Total committed limit |

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
- `94df988` — Hourly portfolio summary to Telegram
- `5b1dcc5` — Tick-size-aware pricing (unlock 99.9¢)
- `16baebd` — Resolved-market sniper
- `6a0dfe4` — Arbitrage scanner
- `fa2cb47` — Two-tier AI evaluator + contrarian filter
- `55dfcbd` — Security: scrub git history of leaked keys

## License
Private repository.
