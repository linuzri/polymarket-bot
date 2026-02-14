# CLAUDE.md - Polymarket Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust. **Pivoted to risk-free sniper strategy** — buying near-certain outcomes at 95-99.9¢ and collecting $1.00 on resolution.

## Current Status (Feb 14, 2026)
- **Balance:** ~$92.40 USDC (deposited $100.27)
- **Strategy:** Risk-free sniper + arbitrage scanner
- **Process:** `polymarket-arb` (PM2 id:13) — single process doing both arb + sniper
- **AI strategy bot:** PAUSED (PM2 id:7, stopped)
- **Telegram:** Trade alerts + hourly portfolio summary

## Architecture
```
polymarket-bot/
├── src/
│   ├── api/
│   │   ├── client.rs      # API client (Gamma + CLOB + tick size fetching)
│   │   └── endpoints.rs   # URL constants
│   ├── arbitrage/
│   │   └── mod.rs          # Arb scanner + resolved-market sniper (MAIN BOT)
│   ├── auth/
│   │   └── mod.rs          # L2 HMAC auth + EIP-712 signing
│   ├── models/
│   │   └── market.rs       # Market, OrderBook, GammaMarket structs
│   ├── orders/
│   │   └── mod.rs          # Tick-size-aware order signing (1-4 decimal precision)
│   ├── notifications/
│   │   └── mod.rs          # Telegram notifications
│   ├── portfolio/
│   │   └── mod.rs          # Position tracking, auto-sell, edge re-eval
│   ├── strategy/           # AI evaluator (PAUSED)
│   │   ├── scanner.rs      # Market scanner
│   │   ├── evaluator.rs    # Signal struct + AI evaluation
│   │   ├── ai_evaluator.rs # Two-tier Claude evaluator
│   │   ├── risk.rs         # Kelly criterion sizing
│   │   ├── engine.rs       # Strategy loop
│   │   └── config.rs       # Strategy config
│   ├── btc5min/
│   │   └── mod.rs          # BTC 5-min markets (DISABLED - 17% WR)
│   └── main.rs             # CLI: run, arb, portfolio, paper
├── ecosystem.config.js     # PM2: polymarket-arb (active), polymarket-bot (stopped)
├── strategy_config.json    # AI strategy config (when enabled)
├── portfolio_state.json    # Persisted portfolio state
├── .env                    # Credentials (NEVER commit)
└── scripts/                # Helper scripts (gitignored)
```

## Key Concepts

### Sniper Strategy (Active)
- Scans 300+ markets every 30s via Gamma API
- Finds outcomes priced 95-99.9% certain
- Fetches `minimum_tick_size` from CLOB API per market
- Places buy orders at ask price with correct decimal precision
- Tracks sniped markets by `condition_id` to avoid duplicates
- Exposure limit: $70 max committed, $25 max per trade
- Sends hourly portfolio summary to Telegram

### Tick Size System
- CLOB API: each market has `minimum_tick_size` (0.1, 0.01, 0.001, or 0.0001)
- Political markets: typically 0.001 → enables $0.999 pricing
- Sports markets: typically 0.01 → max $0.99
- Order amounts rounded per ROUNDING_CONFIG: amount_decimals = price_decimals + 2

### Arbitrage Scanner (Active, rarely finds opportunities)
- Checks if YES ask + NO ask < $0.985
- If found: buy both sides for guaranteed profit
- Market makers keep spreads tight — rarely triggers

### AI Evaluator (Paused)
- Two-tier: Haiku screens 20 markets → Sonnet deep-evaluates flagged ones
- Contrarian filter: Sonnet-confirmed signals get $0.03 min price
- Cost: ~$1.50/day when active

## Critical Rules
- **NEVER commit .env or hardcoded keys** — use dotenv
- **Unicode:** No Unicode arrows/special chars in log messages (Windows cp1252 crashes)
- **dotenv:** Must use `dotenvy::dotenv_override()` (system has conflicting ANTHROPIC_API_KEY)
- **CLOB price constraint:** Price must be >0 and <1. Max submittable = tick_size dependent
- **Addresses must be checksummed** for CLOB API
- **signature_type=1** for proxy wallet orders
- **CLOB API keys are deterministic** — derived from wallet private key, cannot be rotated
- **PM2 release build:** Must stop ALL processes sharing the exe before `cargo build --release`
- **Sniper dedup:** Track by `condition_id`, NOT slug or token_id

## Wallet
- **EOA (signer):** 0x7ec329D34D2c94456c015B236EBEc41d2a7B3Bce
- **Proxy (funder/maker):** 0x0585bc93D1a91B0a325d4A1Fa159e080E9D24853
- **Contract:** Neg risk exchange on Polygon (chain 137)

## PM2 Commands
```bash
pm2 restart polymarket-arb   # Restart sniper/arb
pm2 logs polymarket-arb       # View logs
pm2 stop polymarket-bot       # AI bot is stopped
```
