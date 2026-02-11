# 🎯 Polymarket Bot

Automated trading bot for [Polymarket](https://polymarket.com) prediction markets, built in Rust.

## Features

- 📊 Browse and search live markets
- 📖 View order books and pricing
- 🔄 WebSocket streaming for real-time prices (coming soon)
- 🤖 Automated trading strategies (coming soon)
- 📱 Telegram notifications (coming soon)

## Quick Start

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
cargo build --release

# List hot markets
cargo run -- markets

# Search for BTC markets
cargo run -- markets -q crypto

# View a specific market
cargo run -- market <slug>

# View order book
cargo run -- book <token_id>
```

## Architecture

```
src/
├── main.rs          # CLI entry point
├── api/             # Polymarket REST + WebSocket client
│   ├── client.rs    # HTTP client for Gamma + CLOB APIs
│   └── endpoints.rs # API endpoint constants
├── models/          # Data structures
│   └── market.rs    # Market, OrderBook, etc.
├── strategy/        # Trading strategies (Phase 2)
└── signals/         # News feeds, sentiment (Phase 2)
```

## APIs Used

- **Gamma API** (`gamma-api.polymarket.com`) — Market discovery, metadata
- **CLOB API** (`clob.polymarket.com`) — Order book, trading, auth
- **Data API** (`data-api.polymarket.com`) — Historical data

## Roadmap

### Phase 1: Data ✅
- [x] Market listing and search
- [x] Order book fetching
- [ ] WebSocket real-time prices
- [ ] Historical data collection

### Phase 2: Trading
- [ ] L1/L2 authentication
- [ ] Order placement (limit, market)
- [ ] Position tracking
- [ ] P/L calculation

### Phase 3: Strategies
- [ ] News arbitrage (react to breaking news)
- [ ] Cross-market arbitrage
- [ ] Liquidity provision
- [ ] Sentiment-based trading

### Phase 4: Operations
- [ ] Telegram alerts
- [ ] Dashboard integration
- [ ] Risk management
- [ ] Auto-rebalancing

## Configuration

Copy `.env.example` to `.env` and fill in your credentials:

```bash
cp .env.example .env
```

Edit `config.toml` for trading parameters.

## ⚠️ Disclaimer

This is experimental software for educational purposes. Trading on prediction markets involves risk. Use at your own discretion.
