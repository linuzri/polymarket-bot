import json

log = json.load(open('strategy_trades.json'))
op = log.get('open_positions', {})
print('=== open_positions in trade log ===')
for k, v in op.items():
    print("  " + k[:40] + "... = $" + f"{v:.2f}")
print("\nTotal tracked exposure: $" + f"{sum(op.values()):.2f}")
print("Position count: " + str(len(op)))

trades = log.get('trades', [])
live = [t for t in trades if not t.get('dry_run', True)]
print("\nTotal trades: " + str(len(trades)))
print("Live trades: " + str(len(live)))
total_spent = sum(t.get('size_usd', 0) for t in live)
print("Total spent (all live trades): $" + f"{total_spent:.2f}")

# Per market breakdown
per_market = {}
for t in live:
    cid = t.get('condition_id', '')
    q = t.get('market_question', '')[:40]
    key = cid[:20] + " " + q
    if key not in per_market:
        per_market[key] = {'count': 0, 'total': 0, 'q': q}
    per_market[key]['count'] += 1
    per_market[key]['total'] += t.get('size_usd', 0)

print("\n=== Per-market spending ===")
for k, v in sorted(per_market.items(), key=lambda x: -x[1]['total']):
    print(f"  {v['q']}: {v['count']} trades = ${v['total']:.2f}")
