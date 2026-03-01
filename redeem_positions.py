#!/usr/bin/env python3
"""
Auto-redeem resolved Polymarket positions via proxy wallet.

Runs as a standalone PM2 process on a 30-minute cron.
Reads strategy_trades.json, finds resolved positions, calls redeemPositions()
on the CTF contract through the proxy wallet, and updates the trade log.

Phase 1: Direct on-chain via proxy wallet (pays MATIC gas).
Phase 2: Will swap to poly-web3 relayer (gas-free) when Builder API is approved.
"""

import json
import os
import sys
import time
import traceback
from datetime import datetime, timezone, timedelta
from pathlib import Path

import requests
from dotenv import load_dotenv
from web3 import Web3
from eth_account import Account

# ─── Config ───────────────────────────────────────────────────────────────────

load_dotenv()

# Polygon RPCs (fallback chain)
RPC_URLS = [
    "https://polygon-mainnet.g.alchemy.com/v2/demo",
    "https://rpc-mainnet.maticvigil.com",
    "https://polygon-rpc.com",
    "https://rpc.ankr.com/polygon",
]

# Contract addresses (Polygon mainnet)
CTF_ADDRESS = Web3.to_checksum_address("0x4D97DCd97eC945f40cF65F87097ACe5EA0476045")
USDC_ADDRESS = Web3.to_checksum_address("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174")

# Wallet config from .env
EOA_PRIVATE_KEY = os.getenv("POLY_PRIVATE_KEY", "")
EOA_ADDRESS = os.getenv("POLY_WALLET_ADDRESS", "")
PROXY_ADDRESS = os.getenv("POLY_PROXY_WALLET", "")

# Telegram
TELEGRAM_BOT_TOKEN = os.getenv("TELEGRAM_BOT_TOKEN", "")
TELEGRAM_CHAT_ID = os.getenv("TELEGRAM_CHAT_ID", "")

# Paths
SCRIPT_DIR = Path(__file__).parent
TRADES_FILE = SCRIPT_DIR / "strategy_trades.json"
OUTCOMES_FILE = SCRIPT_DIR / "trade_outcomes.jsonl"

# Minimum age before attempting redemption (hours)
MIN_AGE_HOURS = 12

# ─── ABIs ─────────────────────────────────────────────────────────────────────

CTF_ABI = [
    {
        "name": "redeemPositions",
        "type": "function",
        "inputs": [
            {"name": "collateralToken", "type": "address"},
            {"name": "parentCollectionId", "type": "bytes32"},
            {"name": "conditionId", "type": "bytes32"},
            {"name": "indexSets", "type": "uint256[]"},
        ],
        "outputs": [],
    },
    {
        "name": "payoutNumerators",
        "type": "function",
        "inputs": [
            {"name": "", "type": "bytes32"},
            {"name": "", "type": "uint256"},
        ],
        "outputs": [{"name": "", "type": "uint256"}],
    },
]

# Polymarket ProxyWallet ABI — proxy() function takes array of ProxyCall tuples
# ProxyCall = (address to, uint256 value, bytes data, bool isDelegateCall)
PROXY_ABI = [
    {
        "name": "proxy",
        "type": "function",
        "inputs": [
            {
                "name": "calls",
                "type": "tuple[]",
                "components": [
                    {"name": "to", "type": "address"},
                    {"name": "value", "type": "uint256"},
                    {"name": "data", "type": "bytes"},
                    {"name": "isDelegateCall", "type": "bool"},
                ],
            }
        ],
        "outputs": [{"name": "returnValues", "type": "bytes[]"}],
    }
]

# USDC Transfer event for parsing payout amount
USDC_TRANSFER_TOPIC = Web3.keccak(text="Transfer(address,address,uint256)")


# ─── Helpers ──────────────────────────────────────────────────────────────────

def get_web3() -> Web3:
    """Connect to Polygon with fallback RPCs."""
    for rpc in RPC_URLS:
        try:
            w3 = Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 10}))
            if w3.is_connected():
                print(f"  Connected to RPC: {rpc}")
                return w3
        except Exception:
            continue
    raise ConnectionError("All Polygon RPCs failed")


def send_telegram(message: str):
    """Send Telegram notification."""
    if not TELEGRAM_BOT_TOKEN or not TELEGRAM_CHAT_ID:
        print("  Telegram not configured, skipping notification")
        return
    try:
        url = f"https://api.telegram.org/bot{TELEGRAM_BOT_TOKEN}/sendMessage"
        requests.post(
            url,
            json={"chat_id": TELEGRAM_CHAT_ID, "text": message, "parse_mode": "HTML"},
            timeout=10,
        )
    except Exception as e:
        print(f"  Telegram send failed: {e}")


def log_outcome(trade: dict, tx_hash: str, amount_returned: float):
    """Append redemption outcome to trade_outcomes.jsonl."""
    outcome = {
        "event": "redemption",
        "market_question": trade.get("market_question", ""),
        "bucket_label": trade.get("bucket_label", ""),
        "city": trade.get("city", ""),
        "trade_date": trade.get("timestamp", ""),
        "resolution_date": datetime.now(timezone.utc).isoformat(),
        "amount_spent": trade.get("cost", 0),
        "amount_returned": amount_returned,
        "pnl": amount_returned - trade.get("cost", 0),
        "redeemed_tx": tx_hash,
        "redeemed_at": datetime.now(timezone.utc).isoformat(),
        "condition_id": trade.get("condition_id", ""),
    }
    with open(OUTCOMES_FILE, "a") as f:
        f.write(json.dumps(outcome) + "\n")
    print(f"  Logged outcome to {OUTCOMES_FILE}")


# ─── Resolution Check ────────────────────────────────────────────────────────

def is_market_resolved(condition_id: str) -> bool:
    """Check Gamma API for market resolution status."""
    try:
        url = f"https://gamma-api.polymarket.com/markets?condition_id={condition_id}"
        resp = requests.get(url, timeout=10)
        if resp.status_code == 200:
            markets = resp.json()
            if markets and len(markets) > 0:
                market = markets[0]
                # Check outcomePrices — resolved markets have [1,0] or [0,1]
                outcome_prices = market.get("outcomePrices", "")
                if outcome_prices:
                    try:
                        prices = json.loads(outcome_prices) if isinstance(outcome_prices, str) else outcome_prices
                        if len(prices) >= 2:
                            p0, p1 = float(prices[0]), float(prices[1])
                            if (p0 >= 0.99 and p1 <= 0.01) or (p0 <= 0.01 and p1 >= 0.99):
                                return True
                    except (ValueError, TypeError):
                        pass
                # Also check resolved flag
                if market.get("resolved", False):
                    return True
    except Exception as e:
        print(f"  Error checking resolution for {condition_id}: {e}")
    return False


def check_ctf_resolved(w3: Web3, condition_id: str) -> bool:
    """Check CTF contract directly for payout numerators (backup method)."""
    try:
        ctf = w3.eth.contract(address=CTF_ADDRESS, abi=CTF_ABI)
        cid_bytes = bytes.fromhex(condition_id.replace("0x", ""))
        # If payoutNumerators returns non-zero for either index, market is resolved
        p0 = ctf.functions.payoutNumerators(cid_bytes, 0).call()
        p1 = ctf.functions.payoutNumerators(cid_bytes, 1).call()
        return (p0 + p1) > 0
    except Exception as e:
        print(f"  CTF resolution check failed: {e}")
        return False


# ─── Redemption ───────────────────────────────────────────────────────────────

def redeem_position(w3: Web3, condition_id: str) -> tuple[bool, str, float]:
    """
    Call redeemPositions() on CTF through the proxy wallet.
    Returns (success, tx_hash, usdc_returned).
    """
    ctf = w3.eth.contract(address=CTF_ADDRESS, abi=CTF_ABI)
    proxy = w3.eth.contract(
        address=Web3.to_checksum_address(PROXY_ADDRESS), abi=PROXY_ABI
    )

    # Encode redeemPositions calldata
    cid_bytes = bytes.fromhex(condition_id.replace("0x", ""))
    parent_collection = b"\x00" * 32
    index_sets = [1, 2]  # Both YES and NO — only winning side pays out

    inner_calldata = ctf.encode_abi(
        "redeemPositions",
        args=[USDC_ADDRESS, parent_collection, cid_bytes, index_sets],
    )

    # Build ProxyCall tuple: (to, value, data, isDelegateCall)
    proxy_call = (CTF_ADDRESS, 0, bytes.fromhex(inner_calldata[2:]), False)

    # Build the proxy() transaction
    eoa = Account.from_key(EOA_PRIVATE_KEY)
    nonce = w3.eth.get_transaction_count(eoa.address)

    tx = proxy.functions.proxy([proxy_call]).build_transaction(
        {
            "from": eoa.address,
            "nonce": nonce,
            "gas": 300_000,
            "maxFeePerGas": w3.eth.gas_price * 2,
            "maxPriorityFeePerGas": w3.to_wei(30, "gwei"),
            "chainId": 137,
        }
    )

    # Sign and send
    signed = w3.eth.account.sign_transaction(tx, EOA_PRIVATE_KEY)
    tx_hash = w3.eth.send_raw_transaction(signed.raw_transaction)
    tx_hash_hex = tx_hash.hex()
    print(f"  TX sent: {tx_hash_hex}")

    # Wait for receipt
    receipt = w3.eth.wait_for_transaction_receipt(tx_hash, timeout=120)

    if receipt["status"] != 1:
        print(f"  ❌ TX reverted: {tx_hash_hex}")
        return False, tx_hash_hex, 0.0

    # Parse USDC transfer amount from logs
    usdc_returned = 0.0
    proxy_addr_lower = PROXY_ADDRESS.lower()
    for log in receipt["logs"]:
        if (
            log["address"].lower() == USDC_ADDRESS.lower()
            and len(log["topics"]) >= 3
            and log["topics"][0] == USDC_TRANSFER_TOPIC
        ):
            # Transfer TO our proxy wallet
            to_addr = "0x" + log["topics"][2].hex()[-40:]
            if to_addr.lower() == proxy_addr_lower:
                # USDC has 6 decimals
                amount_raw = int(log["data"].hex(), 16)
                usdc_returned = amount_raw / 1e6
                break

    print(f"  ✅ Redemption successful! USDC returned: ${usdc_returned:.2f}")
    return True, tx_hash_hex, usdc_returned


# ─── Main Loop ────────────────────────────────────────────────────────────────

def main():
    print(f"\n{'='*60}")
    print(f"Polymarket Auto-Redeem — {datetime.now(timezone.utc).isoformat()}")
    print(f"{'='*60}")

    # Validate config
    if not EOA_PRIVATE_KEY:
        print("ERROR: POLY_PRIVATE_KEY not set in .env")
        sys.exit(1)
    if not PROXY_ADDRESS:
        print("ERROR: POLY_PROXY_WALLET not set in .env")
        sys.exit(1)

    # Load trades
    if not TRADES_FILE.exists():
        print("No strategy_trades.json found, nothing to redeem.")
        return

    trades = json.loads(TRADES_FILE.read_text())
    if not isinstance(trades, list):
        print("strategy_trades.json is not a list, skipping.")
        return

    # Filter candidates: non-dry-run, not redeemed, has condition_id, >12h old
    cutoff = datetime.now(timezone.utc) - timedelta(hours=MIN_AGE_HOURS)
    candidates = []
    for i, t in enumerate(trades):
        if t.get("dry_run", False):
            continue
        if t.get("redeemed", False):
            continue
        if not t.get("condition_id"):
            continue
        # Check age
        try:
            ts = datetime.fromisoformat(t["timestamp"].replace("Z", "+00:00"))
            if ts.tzinfo is None:
                ts = ts.replace(tzinfo=timezone.utc)
            if ts > cutoff:
                continue  # Too recent
        except (ValueError, KeyError):
            continue
        candidates.append((i, t))

    if not candidates:
        print("No candidates for redemption.")
        return

    print(f"Found {len(candidates)} candidate(s) to check for resolution.\n")

    # Connect to Polygon
    try:
        w3 = get_web3()
    except ConnectionError as e:
        print(f"ERROR: {e}")
        return

    # Check EOA MATIC balance
    eoa_balance = w3.eth.get_balance(Web3.to_checksum_address(EOA_ADDRESS))
    matic_balance = float(w3.from_wei(eoa_balance, "ether"))
    print(f"EOA MATIC balance: {matic_balance:.4f} MATIC")
    if matic_balance < 0.01:
        print("WARNING: Low MATIC balance! Need at least 0.01 MATIC for gas.")
        send_telegram(
            "⚠️ <b>Auto-Redeem: Low MATIC</b>\n"
            f"EOA balance: {matic_balance:.4f} MATIC\n"
            "Need to bridge MATIC to continue redemptions."
        )
        return

    redeemed_count = 0
    total_returned = 0.0
    modified = False

    for idx, trade in candidates:
        cid = trade["condition_id"]
        print(f"\nChecking: {trade.get('market_question', '?')} | {trade.get('bucket_label', '?')}")
        print(f"  Condition ID: {cid}")

        # Check resolution via Gamma API first, then CTF as backup
        resolved = is_market_resolved(cid)
        if not resolved:
            resolved = check_ctf_resolved(w3, cid)
        if not resolved:
            print("  Not resolved yet, skipping.")
            continue

        print("  ✓ Market is RESOLVED — attempting redemption...")

        try:
            success, tx_hash, usdc_returned = redeem_position(w3, cid)
        except Exception as e:
            print(f"  ❌ Redemption failed: {e}")
            traceback.print_exc()
            continue

        if success:
            # Update trade in list
            trades[idx]["redeemed"] = True
            modified = True
            redeemed_count += 1
            total_returned += usdc_returned

            # Log outcome
            log_outcome(trade, tx_hash, usdc_returned)

            # Telegram notification
            pnl = usdc_returned - trade.get("cost", 0)
            pnl_emoji = "📈" if pnl >= 0 else "📉"
            send_telegram(
                f"✅ <b>Position Redeemed</b>\n\n"
                f"Market: {trade.get('market_question', '?')}\n"
                f"Bucket: {trade.get('bucket_label', '?')}\n"
                f"Cost: ${trade.get('cost', 0):.2f}\n"
                f"Returned: ${usdc_returned:.2f}\n"
                f"{pnl_emoji} P/L: ${pnl:+.2f}\n\n"
                f"<a href='https://polygonscan.com/tx/{tx_hash}'>View TX</a>"
            )

            # Small delay between redemptions to avoid nonce issues
            time.sleep(2)

    # Save updated trades
    if modified:
        TRADES_FILE.write_text(json.dumps(trades, indent=2))
        print(f"\nUpdated strategy_trades.json ({redeemed_count} trades marked redeemed)")

    print(f"\n{'='*60}")
    print(f"Summary: {redeemed_count} redeemed, ${total_returned:.2f} USDC returned")
    print(f"{'='*60}\n")


if __name__ == "__main__":
    main()
