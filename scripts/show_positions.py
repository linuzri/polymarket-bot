import json
ps = json.load(open('portfolio_state.json'))
total = 0
for key, p in ps['positions'].items():
    side = p['side']
    q = p['market_question'][:50]
    shares = p['shares']
    cost = p['cost_basis']
    avg = p['avg_entry_price']
    total += cost
    print(f"  {side:3} | {q:50} | {shares:>10.1f} shares | cost ${cost:>8.2f} | avg ${avg:.4f}")
print(f"\n  TOTAL INVESTED: ${total:.2f}")
