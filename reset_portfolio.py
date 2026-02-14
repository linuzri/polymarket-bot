import json

# Reset portfolio from trade log (correct data)
log = json.load(open('strategy_trades.json'))
trades = log.get('trades', [])
live = [t for t in trades if not t.get('dry_run', True)]

positions = {}
synced_ids = []

for t in live:
    cid = t.get('condition_id', '')
    side = t.get('side', '').lower()
    key = cid + "_" + side
    trade_id = t.get('id', '')
    
    if trade_id:
        synced_ids.append(trade_id)
    
    if t.get('closed', False):
        continue
    
    shares = t.get('shares', 0)
    price = t.get('price', 0)
    size_usd = t.get('size_usd', 0)
    
    if key in positions:
        p = positions[key]
        total_cost = p['cost_basis'] + size_usd
        total_shares = p['shares'] + shares
        p['avg_entry_price'] = total_cost / total_shares if total_shares > 0 else 0
        p['shares'] = total_shares
        p['cost_basis'] = total_cost
    else:
        positions[key] = {
            'condition_id': cid,
            'token_id': key,
            'market_slug': t.get('market_slug', ''),
            'market_question': t.get('market_question', ''),
            'side': t.get('side', '').upper(),
            'shares': shares,
            'cost_basis': size_usd,
            'avg_entry_price': price,
            'current_price': price,
            'opened_at': t.get('timestamp', '2026-02-13T00:00:00Z')
        }

# Load existing state for resolved positions
old_state = json.load(open('portfolio_state.json'))

state = {
    'positions': positions,
    'resolved': old_state.get('resolved', []),
    'alerted_resolutions': old_state.get('alerted_resolutions', []),
    'synced_trade_ids': synced_ids,
    'last_updated': '2026-02-14T00:00:00Z'
}

# Remove positions that are in alerted_resolutions
for key in list(state['positions'].keys()):
    if key in state['alerted_resolutions']:
        del state['positions'][key]

json.dump(state, open('portfolio_state.json', 'w'), indent=2)

print("=== RESET PORTFOLIO ===")
total = 0
for k, p in state['positions'].items():
    print(f"  {p['market_question'][:50]}")
    print(f"    {p['side']} {p['shares']:.2f} shares @ ${p['avg_entry_price']:.4f} | Cost: ${p['cost_basis']:.2f}")
    total += p['cost_basis']
print(f"\nTotal invested: ${total:.2f}")
print(f"Resolved: {len(state['resolved'])}")
print(f"Synced trade IDs: {len(synced_ids)}")
