"""Sync portfolio_state.json to Supabase polymarket_portfolio for dashboard display.
Run periodically via cron to keep dashboard fresh.

Weather markets use event-level slugs (highest-temperature-in-{city}-on-{month}-{day}-{year})
queried via /events endpoint. Regular markets use /markets?slug= endpoint."""
import json
import requests
from datetime import datetime, timezone

import os
SUPABASE_URL = os.environ.get("SUPABASE_URL", "https://cxpablqwnwvacuvhcjen.supabase.co")
SUPABASE_KEY = os.environ.get("SUPABASE_SERVICE_KEY", "")
PORTFOLIO_PATH = r"C:\Users\Nazri Hussain\projects\polymarket-bot\portfolio_state.json"

def fetch_market_price(slug, side):
    """Fetch current price from Polymarket Gamma API (regular markets)."""
    try:
        resp = requests.get(f"https://gamma-api.polymarket.com/markets?slug={slug}", timeout=10)
        if resp.ok and resp.json():
            m = resp.json()[0]
            prices = json.loads(m.get("outcomePrices", "[]"))
            if side == "YES" and len(prices) > 0:
                return float(prices[0])
            elif side == "NO" and len(prices) > 1:
                return float(prices[1])
    except:
        pass
    return None

def fetch_weather_event_prices(event_slug):
    """Fetch prices for all markets within a weather event."""
    try:
        resp = requests.get(f"https://gamma-api.polymarket.com/events?slug={event_slug}", timeout=10)
        if resp.ok and resp.json():
            event = resp.json()[0]
            markets = event.get("markets", [])
            result = {}
            for m in markets:
                q = m.get("question", "")
                prices = json.loads(m.get("outcomePrices", "[]"))
                if len(prices) >= 1:
                    result[q] = {"yes": float(prices[0]), "no": float(prices[1]) if len(prices) > 1 else 1.0 - float(prices[0])}
            return result
    except:
        pass
    return None

def main():
    with open(PORTFOLIO_PATH) as f:
        state = json.load(f)

    positions = state.get("positions", {})
    resolved = state.get("resolved", [])

    # Update live prices
    for key, pos in positions.items():
        slug = pos.get("market_slug", "")
        if not slug:
            continue

        # Try regular market endpoint first
        price = fetch_market_price(slug, pos["side"])
        if price is not None:
            pos["current_price"] = price
            print(f"  [market] {pos['market_question'][:50]}: {pos['side']} @ ${price:.4f}")
        else:
            # Try as weather event slug
            event_slug = pos.get("event_slug", slug)
            event_prices = fetch_weather_event_prices(event_slug)
            if event_prices:
                # Match by question text
                for q, p in event_prices.items():
                    if pos["market_question"][:30].lower() in q.lower() or q[:30].lower() in pos["market_question"].lower():
                        price = p["yes"] if pos["side"] == "YES" else p["no"]
                        pos["current_price"] = price
                        print(f"  [event] {pos['market_question'][:50]}: {pos['side']} @ ${price:.4f}")
                        break
            if price is None:
                print(f"  [skip] {pos['market_question'][:50]}: no price found, keeping ${pos['current_price']:.4f}")

    # Save updated state back
    state["last_updated"] = datetime.now(timezone.utc).isoformat()
    with open(PORTFOLIO_PATH, "w") as f:
        json.dump(state, f, indent=2)

    # Calculate totals
    total_invested = sum(p["cost_basis"] for p in positions.values())
    unrealized_pnl = 0
    for p in positions.values():
        if p["side"] == "YES":
            unrealized_pnl += (p["current_price"] - p["avg_entry_price"]) * p["shares"]
        else:
            unrealized_pnl += (p["avg_entry_price"] - p["current_price"]) * p["shares"]

    realized_pnl = sum(r.get("realized_pnl", 0) for r in resolved)
    total_value = sum(p["current_price"] * p["shares"] for p in positions.values())

    print(f"\nInvested: ${total_invested:.2f}")
    print(f"Current value: ${total_value:.2f}")
    print(f"Unrealized P/L: ${unrealized_pnl:.2f}")
    print(f"Realized P/L: ${realized_pnl:.2f}")

    # Upsert to Supabase
    payload = {
        "id": 1,
        "wallet_balance": round(total_value, 2),
        "initial_deposit": 100.27,
        "total_invested": round(total_invested, 2),
        "unrealized_pnl": round(unrealized_pnl, 2),
        "realized_pnl": round(realized_pnl, 2),
        "positions": positions,
        "resolved": resolved,
        "updated_at": datetime.now(timezone.utc).isoformat()
    }

    headers = {
        "apikey": SUPABASE_KEY,
        "Authorization": f"Bearer {SUPABASE_KEY}",
        "Content-Type": "application/json",
        "Prefer": "resolution=merge-duplicates"
    }

    resp = requests.post(
        f"{SUPABASE_URL}/rest/v1/polymarket_portfolio",
        headers=headers,
        json=payload
    )

    if resp.status_code in (200, 201):
        print("Dashboard synced OK")
    else:
        print(f"Sync failed: {resp.status_code} {resp.text}")

if __name__ == "__main__":
    main()
