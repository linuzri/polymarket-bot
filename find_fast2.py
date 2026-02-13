import requests, json
from datetime import datetime, timezone, timedelta

now = datetime.now(timezone.utc)

# Get events (parent containers for markets)
print("=== Events ending within 7 days ===\n")

r = requests.get('https://gamma-api.polymarket.com/events', params={
    'limit': 100,
    'active': 'true',
    'closed': 'false',
    'order': 'volume24hr',
    'ascending': 'false'
})
events = r.json()

fast = []
for e in events:
    end = e.get('endDate', '')
    if not end:
        continue
    try:
        end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
        dl = (end_dt - now).total_seconds() / 86400
        if 0 < dl < 7:
            vol = float(e.get('volume', 0) or 0)
            title = e.get('title', '?')[:80]
            slug = e.get('slug', '?')
            n_markets = len(e.get('markets', []))
            fast.append((dl, vol, title, slug, n_markets))
    except:
        continue

fast.sort(key=lambda x: x[0])
for dl, vol, title, slug, n in fast:
    print(f"  {dl:.1f}d | Vol: ${vol:,.0f} | {n} markets | {title}")
    print(f"    -> {slug}")

print(f"\nTotal fast events: {len(fast)}")

# Also check for specific market types
print("\n=== Top volume markets resolving < 3 days ===\n")
r = requests.get('https://gamma-api.polymarket.com/markets', params={
    'limit': 500,
    'active': 'true',
    'closed': 'false',
    'order': 'volume24hr',
    'ascending': 'false'
})
markets = r.json()
fast_markets = []
for m in markets:
    end = m.get('endDate', '')
    if not end:
        continue
    try:
        end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
        dl = (end_dt - now).total_seconds() / 86400
        if 0 < dl < 3:
            vol = float(m.get('volume', 0) or 0)
            q = m.get('question', '?')[:80]
            slug = m.get('slug', '?')
            op = m.get('outcomePrices', '[]')
            try:
                prices = json.loads(op)
                yes = float(prices[0]) * 100
            except:
                yes = 0
            fast_markets.append((dl, vol, yes, q, slug))
    except:
        continue

fast_markets.sort(key=lambda x: -x[1])
for dl, vol, yes, q, slug in fast_markets[:15]:
    print(f"  {dl:.1f}d | Vol: ${vol:,.0f} | YES: {yes:.0f}% | {q}")
    print(f"    -> {slug}")
