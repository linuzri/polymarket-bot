#!/usr/bin/env python3
"""
Auto-redeem resolved Polymarket positions via Builder relayer (Phase 2).

Runs as a standalone PM2 process on a 30-minute cron.
Reads strategy_trades.json, finds resolved positions, redeems via poly-web3
Builder relayer (gas-free), and updates the trade log.

Phase 2: Gas-free via Builder relayer + poly-web3.
Phase 1 fallback: Commented out below — uncomment if relayer is down.
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

# ─── Phase 2 Imports (Builder Relayer) ────────────────────────────────────────

from py_clob_client.client import ClobClient
from py_builder_relayer_client.client import RelayClient
from py_builder_signing_sdk.sdk_types import BuilderApiKeyCreds
from py_builder_signing_sdk.config import BuilderConfig
from poly_web3 import PolyWeb3Service, RELAYER_URL

# ─── Config ───────────────────────────────────────────────────────────────────

load_dotenv()

# CLOB API
CLOB_HOST = "https://clob.polymarket.com"
CHAIN_ID = 137  # Polygon

# Wallet
EOA_PRIVATE_KEY = os.getenv("POLY_PRIVATE_KEY", "")
EOA_ADDRESS = os.getenv("POLY_WALLET_ADDRESS", "")
PROXY_ADDRESS = os.getenv("POLY_PROXY_WALLET", "")

# Builder API credentials
BUILDER_API_KEY = os.getenv("POLY_BUILDER_API_KEY", "")
BUILDER_SECRET = os.getenv("POLY_BUILDER_SECRET", "")
BUILDER_PASSPHRASE = os.getenv("POLY_BUILDER_PASSPHRASE", "")

# Telegram
TELEGRAM_BOT_TOKEN = os.getenv("TELEGRAM_BOT_TOKEN", "")
TELEGRAM_CHAT_ID = os.getenv("TELEGRAM_CHAT_ID", "")

# Paths
SCRIPT_DIR = Path(__file__).parent
TRADES_FILE = SCRIPT_DIR / "strategy_trades.json"
OUTCOMES_FILE = SCRIPT_DIR / "trade_outcomes.jsonl"

# Limits
MIN_AGE_HOURS = 12
MAX_REDEMPTIONS_PER_CYCLE = 5  # Conservative — avoid relayer rate limits


# ─── Service Initialization ──────────────────────────────────────────────────

def create_service() -> "PolyWeb3Service":
    """Initialize ProxyWeb3Service with Builder relayer credentials."""
    # Builder credentials
    builder_creds = BuilderApiKeyCreds(
        key=BUILDER_API_KEY,
        secret=BUILDER_SECRET,
        passphrase=BUILDER_PASSPHRASE,
    )
    builder_config = BuilderConfig(local_builder_creds=builder_creds)

    # Relayer client (gas-free on-chain operations)
    relayer_client = RelayClient(
        relayer_url=RELAYER_URL,
        chain_id=CHAIN_ID,
        private_key=EOA_PRIVATE_KEY,
        builder_config=builder_config,
    )

    # CLOB client (for position queries)
    clob_client = ClobClient(
        host=CLOB_HOST,
        chain_id=CHAIN_ID,
        key=EOA_PRIVATE_KEY,
        signature_type=1,  # Proxy/Magic wallet
        funder=PROXY_ADDRESS,
        builder_config=builder_config,
    )
    clob_client.set_api_creds(clob_client.create_or_derive_api_creds())

    # PolyWeb3Service auto-detects ProxyWeb3Service from wallet type
    service = PolyWeb3Service(
        clob_client=clob_client,
        relayer_client=relayer_client,
    )

    return service


# ─── Helpers ──────────────────────────────────────────────────────────────────

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


def log_outcome(trade: dict, amount_returned: float):
    """Append redemption outcome to trade_outcomes.jsonl."""
    cost = trade.get("cost", 0)
    outcome = {
        "event": "redemption",
        "market_question": trade.get("market_question", ""),
        "bucket_label": trade.get("bucket_label", ""),
        "city": trade.get("city", ""),
        "trade_date": trade.get("timestamp", ""),
        "resolution_date": datetime.now(timezone.utc).isoformat(),
        "amount_spent": cost,
        "shares": trade.get("shares", 0),
        "amount_returned": amount_returned,
        "pnl": amount_returned - cost,
        "redeemed_at": datetime.now(timezone.utc).isoformat(),
        "condition_id": trade.get("condition_id", ""),
        "method": "relayer",
    }
    with open(OUTCOMES_FILE, "a") as f:
        f.write(json.dumps(outcome) + "\n")
    print(f"  Logged outcome to {OUTCOMES_FILE}")


# ─── Resolution Check ────────────────────────────────────────────────────────

def is_market_resolved(condition_id: str) -> dict | None:
    """
    Check Gamma API for market resolution status.
    Returns {"resolved": True, "won": True/False} or None if not resolved.
    """
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
                            if p0 >= 0.99 and p1 <= 0.01:
                                return {"resolved": True, "won_yes": True}
                            if p0 <= 0.01 and p1 >= 0.99:
                                return {"resolved": True, "won_yes": False}
                    except (ValueError, TypeError):
                        pass

                # Fallback: check resolved flag
                if market.get("resolved", False):
                    return {"resolved": True, "won_yes": None}
    except Exception as e:
        print(f"  Error checking resolution for {condition_id}: {e}")
    return None


# ─── Phase 2: Relayer Redemption ─────────────────────────────────────────────

def redeem_single_position(service, condition_id: str) -> bool:
    """
    Redeem a single resolved position via Builder relayer (gas-free).
    Returns True on success, False on failure.
    """
    try:
        result = service.redeem(condition_ids=condition_id, batch_size=1)
        if result:
            print(f"  ✅ Redeemed via relayer: {condition_id[:16]}...")
            return True
        else:
            # Empty result = nothing to redeem (already redeemed or no balance)
            print(f"  ⚠️ Relayer returned empty — position may already be redeemed")
            return True  # Mark as redeemed anyway to free exposure
    except Exception as e:
        print(f"  ❌ Relayer redemption failed: {e}")
        traceback.print_exc()
        return False


def check_relayer_health(service) -> bool:
    """Verify the Builder relayer is responding before attempting redemptions."""
    try:
        # Lightweight check — resolve status of a known condition
        # If this call works, auth + relayer are good
        resolved = service.is_condition_resolved(
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        )
        print("  Relayer health check: OK")
        return True
    except Exception as e:
        # Try a simpler check — just see if the service object is valid
        try:
            service.fetch_positions_by_condition_ids(
                user_address=PROXY_ADDRESS, condition_ids=[]
            )
            print("  Relayer health check: OK (positions endpoint)")
            return True
        except Exception as e2:
            print(f"  ⚠️ Relayer health check failed: {e2}")
            return False


# ─── Main ─────────────────────────────────────────────────────────────────────

def main():
    print(f"\n{'='*60}")
    print(f"Polymarket Auto-Redeem (Phase 2 — Relayer)")
    print(f"{datetime.now(timezone.utc).isoformat()}")
    print(f"{'='*60}")

    # Validate config
    if not EOA_PRIVATE_KEY:
        print("ERROR: POLY_PRIVATE_KEY not set in .env")
        sys.exit(1)
    if not PROXY_ADDRESS:
        print("ERROR: POLY_PROXY_WALLET not set in .env")
        sys.exit(1)
    if not BUILDER_API_KEY or not BUILDER_SECRET or not BUILDER_PASSPHRASE:
        print("ERROR: Builder API credentials not set in .env")
        sys.exit(1)

    # Initialize service
    print("\nInitializing Builder relayer service...")
    try:
        service = create_service()
        print(f"  Service type: {type(service).__name__}")
    except Exception as e:
        print(f"ERROR: Failed to initialize service: {e}")
        traceback.print_exc()
        return

    # Health check
    if not check_relayer_health(service):
        print("Relayer unavailable. Will retry next cycle.")
        return

    # Load trades
    if not TRADES_FILE.exists():
        print("No strategy_trades.json found, nothing to redeem.")
        return

    trades = json.loads(TRADES_FILE.read_text())
    if not isinstance(trades, list):
        print("strategy_trades.json is not a list, skipping.")
        return

    # Filter candidates
    cutoff = datetime.now(timezone.utc) - timedelta(hours=MIN_AGE_HOURS)
    candidates = []
    for i, t in enumerate(trades):
        if t.get("dry_run", False):
            continue
        if t.get("redeemed", False):
            continue
        if not t.get("condition_id"):
            continue
        try:
            ts = datetime.fromisoformat(t["timestamp"].replace("Z", "+00:00"))
            if ts.tzinfo is None:
                ts = ts.replace(tzinfo=timezone.utc)
            if ts > cutoff:
                continue
        except (ValueError, KeyError):
            continue
        candidates.append((i, t))

    if not candidates:
        print("\nNo candidates for redemption.")
        return

    print(f"\nFound {len(candidates)} candidate(s) to check.\n")

    redeemed_count = 0
    total_pnl = 0.0
    modified = False

    for idx, trade in candidates:
        if redeemed_count >= MAX_REDEMPTIONS_PER_CYCLE:
            print(f"\nHit per-cycle limit ({MAX_REDEMPTIONS_PER_CYCLE}). Remaining will be tried next cycle.")
            break

        cid = trade["condition_id"]
        print(f"Checking: {trade.get('market_question', '?')} | {trade.get('bucket_label', '?')}")
        print(f"  Condition ID: {cid[:16]}...")

        # Resolution check via Gamma API
        resolution = is_market_resolved(cid)
        if not resolution:
            print("  Not resolved yet, skipping.")
            continue

        won_yes = resolution.get("won_yes")
        side = trade.get("side", "BUY_YES")
        shares = trade.get("shares", 0)
        cost = trade.get("cost", 0)

        # Calculate expected return
        # Winning shares pay $1 each, losing shares pay $0
        if won_yes is True and "YES" in side.upper():
            amount_returned = shares * 1.0  # Each winning YES share = $1
        elif won_yes is False and "NO" in side.upper():
            amount_returned = shares * 1.0  # Each winning NO share = $1
        else:
            amount_returned = 0.0  # Lost — shares are worthless

        pnl = amount_returned - cost

        print(f"  ✓ RESOLVED — {'WIN' if amount_returned > 0 else 'LOSS'}")
        print(f"    Shares: {shares:.2f} | Cost: ${cost:.2f} | Return: ${amount_returned:.2f} | P/L: ${pnl:+.2f}")

        # Attempt redemption via relayer
        print("  Redeeming via Builder relayer...")
        success = redeem_single_position(service, cid)

        if success:
            trades[idx]["redeemed"] = True
            trades[idx]["redeemed_at"] = datetime.now(timezone.utc).isoformat()
            modified = True
            redeemed_count += 1
            total_pnl += pnl

            # Log outcome
            log_outcome(trade, amount_returned)

            # Telegram notification
            pnl_emoji = "📈" if pnl >= 0 else "📉"
            send_telegram(
                f"✅ <b>Position Redeemed (Relayer)</b>\n\n"
                f"Market: {trade.get('market_question', '?')}\n"
                f"Bucket: {trade.get('bucket_label', '?')}\n"
                f"Shares: {shares:.2f} @ ${trade.get('price', 0):.2f}\n"
                f"Cost: ${cost:.2f}\n"
                f"Return: ${amount_returned:.2f}\n"
                f"{pnl_emoji} P/L: ${pnl:+.2f}"
            )

            # Rate limit courtesy — 2s between redemptions
            time.sleep(2)

    # Save updated trades
    if modified:
        TRADES_FILE.write_text(json.dumps(trades, indent=2))
        print(f"\nUpdated strategy_trades.json ({redeemed_count} trades marked redeemed)")

    print(f"\n{'='*60}")
    print(f"Summary: {redeemed_count} redeemed | Total P/L: ${total_pnl:+.2f}")
    print(f"{'='*60}\n")


if __name__ == "__main__":
    main()


# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 1 FALLBACK (direct on-chain, needs MATIC)
# Uncomment this section and comment out Phase 2 code above if the Builder
# relayer is down or poly-web3 has issues.
# ═══════════════════════════════════════════════════════════════════════════════
#
# from web3 import Web3
# from eth_account import Account
#
# RPC_URLS = [
#     "https://polygon-mainnet.g.alchemy.com/v2/demo",
#     "https://rpc-mainnet.maticvigil.com",
#     "https://polygon-rpc.com",
#     "https://rpc.ankr.com/polygon",
# ]
#
# CTF_ADDRESS = Web3.to_checksum_address("0x4D97DCd97eC945f40cF65F87097ACe5EA0476045")
# USDC_ADDRESS = Web3.to_checksum_address("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174")
#
# CTF_ABI = [
#     {
#         "name": "redeemPositions",
#         "type": "function",
#         "inputs": [
#             {"name": "collateralToken", "type": "address"},
#             {"name": "parentCollectionId", "type": "bytes32"},
#             {"name": "conditionId", "type": "bytes32"},
#             {"name": "indexSets", "type": "uint256[]"},
#         ],
#         "outputs": [],
#     }
# ]
#
# PROXY_ABI = [
#     {
#         "name": "proxy",
#         "type": "function",
#         "inputs": [
#             {
#                 "name": "calls",
#                 "type": "tuple[]",
#                 "components": [
#                     {"name": "to", "type": "address"},
#                     {"name": "value", "type": "uint256"},
#                     {"name": "data", "type": "bytes"},
#                     {"name": "isDelegateCall", "type": "bool"},
#                 ],
#             }
#         ],
#         "outputs": [{"name": "returnValues", "type": "bytes[]"}],
#     }
# ]
#
# USDC_TRANSFER_TOPIC = Web3.keccak(text="Transfer(address,address,uint256)")
#
#
# def get_web3() -> Web3:
#     for rpc in RPC_URLS:
#         try:
#             w3 = Web3(Web3.HTTPProvider(rpc, request_kwargs={"timeout": 10}))
#             if w3.is_connected():
#                 return w3
#         except Exception:
#             continue
#     raise ConnectionError("All Polygon RPCs failed")
#
#
# def redeem_position(w3: Web3, condition_id: str) -> tuple[bool, str, float]:
#     ctf = w3.eth.contract(address=CTF_ADDRESS, abi=CTF_ABI)
#     proxy = w3.eth.contract(
#         address=Web3.to_checksum_address(PROXY_ADDRESS), abi=PROXY_ABI
#     )
#     cid_bytes = bytes.fromhex(condition_id.replace("0x", ""))
#     parent_collection = b"\x00" * 32
#     index_sets = [1, 2]
#     inner_calldata = ctf.encode_abi(
#         "redeemPositions",
#         args=[USDC_ADDRESS, parent_collection, cid_bytes, index_sets],
#     )
#     proxy_call = (CTF_ADDRESS, 0, bytes.fromhex(inner_calldata[2:]), False)
#     eoa = Account.from_key(EOA_PRIVATE_KEY)
#     nonce = w3.eth.get_transaction_count(eoa.address)
#     tx = proxy.functions.proxy([proxy_call]).build_transaction(
#         {
#             "from": eoa.address,
#             "nonce": nonce,
#             "gas": 300_000,
#             "maxFeePerGas": w3.eth.gas_price * 2,
#             "maxPriorityFeePerGas": w3.to_wei(30, "gwei"),
#             "chainId": 137,
#         }
#     )
#     signed = w3.eth.account.sign_transaction(tx, EOA_PRIVATE_KEY)
#     tx_hash = w3.eth.send_raw_transaction(signed.raw_transaction)
#     tx_hash_hex = tx_hash.hex()
#     receipt = w3.eth.wait_for_transaction_receipt(tx_hash, timeout=120)
#     if receipt["status"] != 1:
#         return False, tx_hash_hex, 0.0
#     usdc_returned = 0.0
#     for log in receipt["logs"]:
#         if (
#             log["address"].lower() == USDC_ADDRESS.lower()
#             and len(log["topics"]) >= 3
#             and log["topics"][0] == USDC_TRANSFER_TOPIC
#         ):
#             to_addr = "0x" + log["topics"][2].hex()[-40:]
#             if to_addr.lower() == PROXY_ADDRESS.lower():
#                 amount_raw = int(log["data"].hex(), 16)
#                 usdc_returned = amount_raw / 1e6
#                 break
#     return True, tx_hash_hex, usdc_returned
