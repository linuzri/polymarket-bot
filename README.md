# 🎯 Polymarket Trading Bot

Automated prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Uses a **two-tier AI evaluator** (Claude 3.5 Haiku + Claude Sonnet 4) and an **arbitrage scanner** to find profitable trades.

## 🔴 Live Trading Status

- **Balance:** ~$94.71 USDC
- **Initial Deposit:** $100.27
- **Processes:** 2 (AI strategy + arb scanner)
- **Telegram notifications:** Active (signals, trades, arbs, errors)

## Features

- **Two-Tier AI Evaluator** — Haiku screens 20 markets/cycle, Sonnet deep-evaluates flagged candidates for higher accuracy
- **Arbitrage Scanner** — Separate process scanning every 30s for YES+NO price gaps (risk-free profit)
- **Market Scanner** — Fetches 300+ markets (top volume + fast-resolving by 24h volume)
- **Fast-Resolving Priority** — Sports, crypto daily, esports markets evaluated first
- **Contrarian Bet Support** — Sonnet-confirmed signals can trade at prices as low as $0.03
- **Live Trading** — Real money orders with EIP-712 signed authentication
- **Paper Trading** — Practice with $1,000 virtual balance
- **Telegram Alerts** — Notifications for signals, executed trades, arbs, and errors
- **Risk Management** — Kelly criterion sizing with conservative limits
- **Portfolio Tracking** — Open positions, resolved positions, P/L, auto-sell (TP/SL)
- **AI Edge Re-Evaluation** — Re-evaluates open positions >24h old, sells if edge is gone

## Quick Start

### Prerequisites
- [Rust](https://rustup.rs/) (1.75+)
- Polymarket account with funds deposited
- API credentials (derived via `py_clob_client`)
- Anthropic API key (for Claude evaluator)

### Setup

1. Clone the repo:
```bash
git clone https://github.com/linuzri/polymarket-bot.git
cd polymarket-bot
```

2. Create `.env` file:
```env
POLY_WALLET_ADDRESS=<your-eoa-wallet-address>
POLY_PROXY_WALLET=<your-proxy-wallet-address>
POLY_PRIVATE_KEY=<your-private-key>
POLY_API_KEY=<clob-api-key>
POLY_API_SECRET=<clob-api-secret>
POLY_PASSPHRASE=<clob-passphrase>
ANTHROPIC_API_KEY=<claude-api-key>
TELEGRAM_BOT_TOKEN=<telegram-bot-token>
TELEGRAM_CHAT_ID=<your-chat-id>
```

3. Derive API credentials (one-time):
```bash
pip install py-clob-client
python -c "
from py_clob_client.client import ClobClient
c = ClobClient('https://clob.polymarket.com', key='YOUR_PRIVATE_KEY', chain_id=137)
creds = c.create_or_derive_api_creds()
print(creds)
"
```

### Usage

```bash
# Browse markets
cargo run -- markets -q "trump" -l 10

# View market details
cargo run -- market <market-slug>

# Check account balance & positions
cargo run -- account

# Buy/Sell shares
cargo run -- buy <slug> yes 5 --dry-run
cargo run -- buy <slug> yes 5
cargo run -- sell <slug> yes 10

# Run automated strategy engine (AI evaluator)
cargo run -- run --dry-run    # paper mode
cargo run -- run              # live trading

# Run arbitrage scanner
cargo run -- arb --dry-run    # paper mode
cargo run -- arb              # live scanning

# View portfolio
cargo run -- portfolio

# Paper trading
cargo run -- paper buy <slug> yes 10
cargo run -- paper portfolio
```

## Strategy Engine

### AI Strategy (Two-Tier)

1. **Scan** — Fetches 300+ active markets
2. **Tier 1 (Haiku)** — Fast screening of up to 20 markets per cycle
3. **Tier 2 (Sonnet)** — Deep evaluation of Haiku-flagged candidates (more accurate)
4. **Signal** — Identifies markets where AI estimate diverges from market price
5. **Risk Check** — Kelly criterion sizing, min price filters (relaxed for Sonnet-confirmed)
6. **Execute** — Places orders and sends Telegram notification

### Arbitrage Scanner

Separate process running every 30 seconds:
1. Fetches all active markets from Gamma API
2. Pre-filters by mid-price spread (YES + NO < $0.99)
3. Checks actual CLOB order books for real ask prices
4. Executes when YES + NO < $0.985 (1.5%+ guaranteed profit)
5. Buys both sides simultaneously — risk-free

### Configuration
| Parameter | Value |
|-----------|-------|
| Max trade size | **$5** |
| Max open positions | **15** |
| Max total exposure | **$50** |
| Minimum edge | **15%** |
| Kelly fraction | **0.25** (quarter Kelly) |
| AI Tier 1 | Claude 3.5 Haiku (fast screen) |
| AI Tier 2 | Claude Sonnet 4 (deep eval) |
| Arb min spread | **1.5%** |
| Arb scan interval | **30 seconds** |
| Arb max size | **$10/side** |

Configure in `strategy_config.json`.

## Architecture

```
                    ┌─── AI Strategy Bot (PM2: polymarket-bot) ───┐
                    │                                              │
Scanner ──→ Tier 1 (Haiku) ──→ Tier 2 (Sonnet) ──→ Risk ──→ Execute
                    │                                              │
                    └──── Portfolio Monitor ←── Auto-Sell (TP/SL) ─┘

                    ┌─── Arb Scanner (PM2: polymarket-arb) ───────┐
                    │                                              │
Gamma API ──→ Pre-filter ──→ Order Books ──→ Spread Check ──→ Execute Both Sides
                    │                                              │
                    └──── Telegram Alerts ─────────────────────────┘
```

### Auth Flow
- **L2 HMAC**: API request authentication (balance, orders, trades)
- **EIP-712**: Order signing for the CTF Exchange smart contract
- **Proxy Wallet**: Funds held in Polymarket proxy wallet, signed by EOA

## PM2 Processes

| Process | PM2 Name | Command | Interval |
|---------|----------|---------|----------|
| AI Strategy | `polymarket-bot` | `run` | 5 min |
| Arb Scanner | `polymarket-arb` | `arb` | 30 sec |

## Tech Stack
- **Rust** — Core bot logic
- **Claude 3.5 Haiku + Sonnet 4** — Two-tier AI market evaluation
- **alloy** — Ethereum primitives, EIP-712 signing
- **reqwest** — HTTP client
- **serde** — JSON serialization
- **clap** — CLI argument parsing
- **tokio** — Async runtime

## Project Status

| Component | Status |
|-----------|--------|
| Market browser | ✅ Working |
| Order book viewer | ✅ Working |
| Auth (L2 HMAC + EIP-712) | ✅ Working |
| Buy/Sell orders | ✅ Working |
| Paper trading | ✅ Working |
| Two-Tier AI Evaluator | ✅ Live |
| Arbitrage Scanner | ✅ Live |
| Strategy engine | ✅ Live |
| Portfolio tracker | ✅ Working |
| Auto-sell (TP/SL) | ✅ Working |
| AI Edge re-evaluation | ✅ Built (opt-in) |
| Telegram notifications | ✅ Working |
| Fast-resolving scanner | ✅ Working |
| Contrarian bet filter | ✅ Working |

## License
Private — not for redistribution.

## Links
- [Polymarket](https://polymarket.com)
- [Polymarket CLOB Docs](https://docs.polymarket.com)
