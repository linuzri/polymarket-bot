# 🎯 Polymarket Trading Bot

Automated prediction market trading bot for [Polymarket](https://polymarket.com), built in Rust.

## Features

- **Market Browser** — Search and browse active prediction markets
- **Order Book** — View real-time L2 order book data
- **Real Trading** — Place buy/sell orders with EIP-712 signed authentication
- **Paper Trading** — Practice with $1,000 virtual balance
- **Strategy Engine** — Automated value betting with risk management
- **Portfolio Tracking** — View balance, positions, and trade history

## Quick Start

### Prerequisites
- [Rust](https://rustup.rs/) (1.75+)
- Polymarket account with funds deposited
- API credentials (derived via `py_clob_client`)

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

# Check account balance
cargo run -- account

# Buy shares (dry run first!)
cargo run -- buy <slug> yes 5 --dry-run
cargo run -- buy <slug> yes 5  # real trade

# Sell shares
cargo run -- sell <slug> yes 10 --dry-run

# Run strategy engine (dry run)
cargo run -- run --dry-run

# Paper trading
cargo run -- paper buy <slug> yes 10
cargo run -- paper portfolio
cargo run -- paper history
```

## Strategy Engine

The bot uses a **value betting strategy**:

1. **Scan** — Fetches active markets with good volume (>$10K)
2. **Evaluate** — Estimates true probability using heuristics
3. **Signal** — Identifies markets where our estimate diverges from market price
4. **Risk Check** — Applies Kelly criterion sizing with conservative limits
5. **Execute** — Places orders on identified opportunities

### Risk Management
| Parameter | Default |
|-----------|---------|
| Max trade size | $5 |
| Max open positions | 10 |
| Max total exposure | $20 |
| Minimum edge | 10% |
| Kelly fraction | 0.25 (quarter Kelly) |
| Min market volume | $10,000 |
| Min hours to close | 24h |

Configure in `strategy_config.json`.

## Architecture

```
Scanner ─→ Evaluator ─→ Signal Generator ─→ Risk Check ─→ Execution
   ↑                                                          ↓
   └──────────── Position Monitor ←── Trade Logger ←──────────┘
```

### Auth Flow
- **L2 HMAC**: API request authentication (balance, orders, trades)
- **EIP-712**: Order signing for the CTF Exchange smart contract
- **Proxy Wallet**: Funds held in Polymarket proxy wallet, signed by EOA

## Tech Stack
- **Rust** — Core bot logic
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
| Buy/Sell orders | ✅ Working (first trade executed!) |
| Paper trading | ✅ Working |
| Strategy engine | 🔨 Building |
| Telegram notifications | 📋 Planned |
| News-driven signals | 📋 Planned |

## License
Private — not for redistribution.

## Links
- [Polymarket](https://polymarket.com)
- [Polymarket CLOB Docs](https://docs.polymarket.com)
- [Dashboard](https://trade-bot-hq.vercel.app) (MT5 trading dashboard)
