# 🎯 Polymarket Trading Bot

Automated prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust. Uses **Claude 3.5 Haiku** as an AI evaluator to find edge against market prices.

## 🔴 Live Trading Status

- **Balance:** ~$99 USDC deposited (real money)
- **Trades placed:** 4 confirmed
- **Telegram notifications:** Active (signals, trades, errors)

## Features

- **AI Evaluator** — Claude 3.5 Haiku evaluates each market's true probability vs market price to find edge
- **Market Scanner** — Fetches 198 markets (top volume + fast-resolving sorted by 24h volume)
- **Fast-Resolving Priority** — Sports, crypto daily, esports markets evaluated first (resolve quickly = faster feedback)
- **Live Trading** — Real money orders with EIP-712 signed authentication
- **Paper Trading** — Practice with $1,000 virtual balance
- **Telegram Alerts** — Notifications for signals, executed trades, and errors
- **Risk Management** — Kelly criterion sizing with conservative limits
- **Portfolio Tracking** — View balance, positions, and trade history

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

# Buy shares (dry run first!)
cargo run -- buy <slug> yes 5 --dry-run
cargo run -- buy <slug> yes 5  # real trade

# Sell shares
cargo run -- sell <slug> yes 10 --dry-run

# Run automated strategy engine
cargo run -- run --dry-run    # paper mode
cargo run -- run              # live trading

# Paper trading
cargo run -- paper buy <slug> yes 10
cargo run -- paper portfolio
cargo run -- paper history
```

## Strategy Engine

The bot uses an **AI-powered value betting strategy**:

1. **Scan** — Fetches 198 active markets (top volume + fast-resolving by 24h volume)
2. **Prioritize** — Fast-resolving markets first (sports, crypto daily, esports)
3. **AI Evaluate** — Claude 3.5 Haiku estimates true probability for each market
4. **Signal** — Identifies markets where AI estimate diverges from market price by ≥ min edge
5. **Risk Check** — Applies Kelly criterion sizing with conservative limits
6. **Execute** — Places orders and sends Telegram notification
7. **Notify** — Telegram alerts for signals, trades, and errors

### Strategy Configuration
| Parameter | Value |
|-----------|-------|
| Max trade size | **$5** |
| Max open positions | 10 |
| Max total exposure | **$50** |
| Minimum edge | **10%** |
| Kelly fraction | **0.25** (quarter Kelly) |
| Min market volume | $10,000 |
| Min hours to close | 24h |

Configure in `strategy_config.json`.

## Architecture

```
Scanner ─→ AI Evaluator (Claude 3.5 Haiku) ─→ Signal Generator ─→ Risk Check ─→ Execution
   ↑                                                                               ↓
   └──────────── Position Monitor ←── Trade Logger ←── Telegram Notifier ←─────────┘
```

### Auth Flow
- **L2 HMAC**: API request authentication (balance, orders, trades)
- **EIP-712**: Order signing for the CTF Exchange smart contract
- **Proxy Wallet**: Funds held in Polymarket proxy wallet, signed by EOA

## Tech Stack
- **Rust** — Core bot logic
- **Claude 3.5 Haiku** — AI market evaluation (via Anthropic API)
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
| AI Evaluator (Claude) | ✅ Working |
| Strategy engine | ✅ Live |
| Telegram notifications | ✅ Working |
| Fast-resolving scanner | ✅ Working |
| Live trading | ✅ **4 trades placed** |

## License
Private — not for redistribution.

## Links
- [Polymarket](https://polymarket.com)
- [Polymarket CLOB Docs](https://docs.polymarket.com)
