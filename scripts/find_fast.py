import requests, json
from datetime import datetime, timezone, timedelta

now = datetime.now(timezone.utc)

# Try different search approaches for fast-resolving markets
print("=== Searching for fast-resolving markets ===\n")

# Method 1: Browse by tags
for tag in ['nba', 'nfl', 'sports', 'crypto-prices', 'bitcoin', 'tennis', 'soccer']:
    try:
        r = requests.get('https://gamma-api.polymarket.com/markets', params={
            'limit': 10,
            'active': 'true', 
            'closed': 'false',
            'tag': tag,
            'order': 'volume24hr',
            'ascending': 'false'
        })
        markets = r.json()
        if markets:
            print(f"--- {tag.upper()} ({len(markets)} found) ---")
            for m in markets[:3]:
                q = m.get('question', '?')[:90]
                vol = float(m.get('volume', 0) or 0)
                end = m.get('endDate', '')
                slug = m.get('slug', '?')
                days_left = '?'
                if end:
                    try:
                        end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
                        dl = (end_dt - now).total_seconds() / 86400
                        days_left = f"{dl:.1f}d"
                    except:
                        pass
                print(f"  {days_left} | Vol: ${vol:,.0f} | {q}")
                print(f"    -> {slug}")
            print()
    except Exception as e:
        print(f"  Error for {tag}: {e}\n")

# Method 2: Search for today's events
print("--- SEARCHING 'today' ---")
r = requests.get('https://gamma-api.polymarket.com/markets', params={
    'limit': 20,
    'active': 'true',
    'closed': 'false',
    'order': 'volume24hr',
    'ascending': 'false'
})
markets = r.json()
for m in markets:
    end = m.get('endDate', '')
    if not end:
        continue
    try:
        end_dt = datetime.fromisoformat(end.replace('Z', '+00:00'))
        dl = (end_dt - now).total_seconds() / 86400
        if 0 < dl < 3:
            q = m.get('question', '?')[:90]
            vol = float(m.get('volume', 0) or 0)
            slug = m.get('slug', '?')
            print(f"  {dl:.1f}d | Vol: ${vol:,.0f} | {q}")
            print(f"    -> {slug}")
    except:
        continue
