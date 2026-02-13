# CLAUDE.md - Polymarket Bot

## Project Overview
Automated Polymarket prediction market trading bot built in Rust.

## Architecture
```
polymarket-bot/
├── src/
│   ├── api/
│   │   ├── client.rs      # API client (Gamma + CLOB)
│   │   └── endpoints.rs   # URL constants
│   ├── auth/
│   │   └── mod.rs          # L2 HMAC auth + EIP-712 signing support
│   ├── models/
│   │   └── market.rs       # Market, OrderBook, GammaMarket structs
│   ├── orders/
│   │   └── mod.rs          # EIP-712 order signing + placement
│   ├── paper/
│   │   └── mod.rs          # Paper trading engine ($1000 virtual)
│   ├── strategy/
│   │   ├── mod.rs          # Strategy module
│   │   ├── scanner.rs      # Market scanner
│   │   ├── evaluator.rs    # Probability evaluator (heuristic)
│   │   ├── risk.rs         # Risk manager (Kelly criterion)
│   │   ├── engine.rs       # Main strategy loop
│   │   └── logger.rs       # Trade logging
│   ├── signals/
│   │   └── mod.rs          # Signal types
│   └── main.rs             # CLI entry point
├── strategy_config.json    # Strategy configuration
├── paper_account.json      # Paper trading state
├── .env                    # Credentials (NEVER commit)
├── Cargo.toml
└── README.md
```

## Key Concepts

### Authentication
- **L1 Auth**: EIP-712 signed message (for deriving API keys)
- **L2 Auth**: HMAC-SHA256 signed headers (for API requests)
- **Order Signing**: EIP-712 typed data signature for CTF Exchange

### Wallet Architecture
- **EOA Wallet** (`POLY_WALLET_ADDRESS`): Signs transactions, owns the proxy
- **Proxy Wallet** (`POLY_PROXY_WALLET`): Holds funds, is the `maker` in orders
- **Signature Type**: 1 (POLY_PROXY) — EOA signs, proxy funds

### Contract Addresses (Polygon Mainnet)
- Normal Exchange: `0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E`
- Neg Risk Exchange: `0xC5d563A36AE78145C45a50134d48A1215220f80a`
- USDC (PoS): `0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174`
- CTF Tokens: `0x4D97DCd97eC945f40cF65F87097ACe5EA0476045`

### APIs
- **Gamma API**: `https://gamma-api.polymarket.com` — Market data, search
- **CLOB API**: `https://clob.polymarket.com` — Auth, orders, balance, trades

### Order Flow
1. Fetch market from Gamma API (get token_id, neg_risk)
2. Fetch order book from CLOB API (get best price)
3. Get fee rate for maker address
4. Build order (calculate maker/taker amounts)
5. Sign order (EIP-712 with domain separator)
6. POST to /order with L2 HMAC auth headers

## Environment Variables (.env)
```
POLY_WALLET_ADDRESS=<EOA checksummed address>
POLY_PROXY_WALLET=<Proxy wallet address>
POLY_PRIVATE_KEY=<Private key with 0x prefix>
POLY_API_KEY=<CLOB API key>
POLY_API_SECRET=<CLOB API secret (base64)>
POLY_PASSPHRASE=<CLOB passphrase>
```

## CLI Commands
```bash
cargo run -- markets [-q query] [-l limit]   # List markets
cargo run -- market <slug>                    # Market details
cargo run -- book <token_id>                  # Order book
cargo run -- account                          # Balance + positions
cargo run -- buy <slug> <yes/no> <$amount> [--dry-run]
cargo run -- sell <slug> <yes/no> <shares> [--dry-run]
cargo run -- run [--dry-run]                  # Start strategy engine
cargo run -- paper buy/sell/portfolio/history/reset
```

## Common Issues
- **401 Unauthorized**: Use EOA address (not proxy) for POLY_ADDRESS header
- **Gzip garbled response**: Don't send Accept-Encoding: gzip (reqwest handles it)
- **"Invalid order payload"**: Addresses must be checksummed, salt must be number not string
- **"price must be < 1"**: CLOB rejects prices >= 1.0, round carefully
- **"min size: $1"**: Minimum order value is $1 USDC
- **balance-allowance returns 0**: Use `asset_type=COLLATERAL` and `signature_type=1` (not 2)
- **PowerShell git push "errors"**: stderr output from git is normal, push succeeds

## Strategy Engine
- **Value Betting**: Heuristic-based probability estimation vs market price
- **Risk Management**: Kelly criterion, max $5/trade, max 10 positions, max $20 exposure
- **Config**: `strategy_config.json` — start with `dry_run: true`
- **Scan interval**: 5 minutes

## Development Notes
- Always test with `--dry-run` before real trades
- Never commit .env file
- Use `cargo run -- account` to verify balance before trading
- The py_clob_client Python package is useful for debugging auth issues
- Salt generation: `timestamp * random()` (matches Python client)
