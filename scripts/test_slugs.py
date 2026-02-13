import json, requests

trades = json.load(open('strategy_trades.json'))['trades']
for t in trades:
    if t.get('dry_run', True):
        continue
    slug = t.get('market_slug', '')
    q = t.get('market_question', '')[:60]
    print("Slug:", slug)
    print("  Q:", q)
    try:
        r = requests.get("https://gamma-api.polymarket.com/markets?slug=" + slug, timeout=10)
        data = r.json()
        if data:
            print("  Found:", str(data[0].get("question", "?"))[:60])
            print("  Closed:", data[0].get("closed", "?"))
        else:
            print("  NOT FOUND")
    except Exception as e:
        print("  ERROR:", e)
    print()
