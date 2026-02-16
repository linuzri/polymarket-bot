"""
Update portfolio_state.json with sniper positions found during reconciliation.
"""
import json

data = json.load(open('reconciliation_data.json'))
ps = json.load(open('portfolio_state.json'))

trades = data['trades']

# Group by asset_id to find open positions
by_token = {}
for t in trades:
    token = t.get('asset_id', 'unknown')
    if token not in by_token:
        by_token[token] = {'buys': [], 'sells': [], 'market': t.get('market', '')}
    side = t.get('side', '')
    price = float(t.get('price', 0))
    size = float(t.get('size', 0))
    outcome = t.get('outcome', '')
    entry = {'price': price, 'size': size, 'cost': price * size, 'time': t.get('match_time', ''), 'outcome': outcome}
    if side == 'BUY':
        by_token[token]['buys'].append(entry)
    else:
        by_token[token]['sells'].append(entry)

# Find open sniper positions not in portfolio_state
existing_tokens = set(ps.get('positions', {}).keys())
print(f"Existing positions in portfolio_state: {len(existing_tokens)}")

new_positions = 0
for token, td in by_token.items():
    buy_shares = sum(b['size'] for b in td['buys'])
    sell_shares = sum(s['size'] for s in td['sells'])
    net_shares = buy_shares - sell_shares
    
    if net_shares < 0.5:
        continue  # Closed position
    
    avg_price = sum(b['cost'] for b in td['buys']) / buy_shares if buy_shares > 0 else 0
    
    if avg_price < 0.90:
        continue  # Not a sniper position (already tracked as strategy)
    
    # Check if already in portfolio_state by condition_id match
    already_tracked = False
    for existing_key in existing_tokens:
        if token in existing_key:
            already_tracked = True
            break
    
    if not already_tracked:
        net_cost = sum(b['cost'] for b in td['buys']) - sum(s['cost'] for s in td['sells'])
        # Determine side from outcome field
        outcome = td['buys'][0].get('outcome', 'unknown')
        side = 'YES' if 'Yes' in outcome else 'NO' if 'No' in outcome else 'YES'
        
        new_pos = {
            "condition_id": token,
            "token_id": token,
            "market_slug": "sniper-position",
            "market_question": f"Sniper position (avg ${avg_price:.4f})",
            "side": side,
            "shares": net_shares,
            "cost_basis": net_cost,
            "avg_entry_price": avg_price,
            "current_price": avg_price,
            "opened_at": td['buys'][0]['time']
        }
        
        key = f"{token}_{side.lower()}"
        ps['positions'][key] = new_pos
        new_positions += 1
        print(f"  Added: {net_shares:.1f} shares @ ${avg_price:.4f} = ${net_cost:.2f}")

if new_positions > 0:
    with open('portfolio_state.json', 'w') as f:
        json.dump(ps, f, indent=2)
    print(f"\nAdded {new_positions} sniper positions to portfolio_state.json")
else:
    print("\nNo new positions to add")

print(f"\nTotal positions now: {len(ps['positions'])}")
total_invested = sum(p.get('cost_basis', 0) for p in ps['positions'].values())
print(f"Total invested: ${total_invested:.2f}")
print(f"Exchange balance: $25.63")
print(f"Accounted: ${total_invested + 25.63:.2f} / $100.27 deposit")
