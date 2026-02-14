import json

state = json.load(open('portfolio_state.json'))
print("=== Current Resolved ===")
total = 0
for r in state['resolved']:
    q = r.get('market_question', '')[:45]
    pnl = r.get('realized_pnl', 0)
    cost = r.get('cost_basis', 0)
    outcome = r.get('outcome', '?')
    total += pnl
    print(f"  {q} | {outcome} | cost: ${cost:.2f} | pnl: ${pnl:.2f}")
print(f"Total realized: ${total:.2f}")

# Fix: recalculate based on actual trade amounts
# From CLOB data, actual trades were:
# NBA Over: $1.01 spent, won (~$1 profit)
# Bangladesh NO: $7.00 spent (but actual was $5 from CLOB? let's use trade log)
# Powell YES: $1.28 spent, lost all
# Tennis: from trade log

# Actually let's just recalculate from trade log
log = json.load(open('strategy_trades.json'))
live_trades = [t for t in log['trades'] if not t.get('dry_run', True)]

print("\n=== Actual Trades ===")
total_spent = 0
for t in live_trades:
    q = t.get('market_question', '')[:45]
    cost = t.get('size_usd', 0)
    total_spent += cost
    print(f"  {t['side']} {q} | ${cost:.2f}")
print(f"Total spent: ${total_spent:.2f}")
print(f"Cash now: $94.71")
print(f"Actual total loss: ${100 - 94.71:.2f}")

# Reset resolved with correct amounts
new_resolved = []
for r in state['resolved']:
    q = r.get('market_question', '')
    # Find matching trade in log
    matching = [t for t in live_trades if t.get('condition_id', '') == r.get('condition_id', '')]
    if matching:
        actual_cost = matching[0].get('size_usd', 0)
        r['cost_basis'] = actual_cost
        # Recalculate pnl
        if r['outcome'] in ['LOST', 'WRITTEN-OFF']:
            r['realized_pnl'] = -actual_cost
        elif r['outcome'] == 'MANUAL-CLOSE':
            # These were sold, approximate from $100-$94.71 total loss
            r['realized_pnl'] = (r.get('resolution_price', 0) - r.get('avg_entry_price', 0)) * matching[0].get('shares', 0)
    new_resolved.append(r)

state['resolved'] = new_resolved
json.dump(state, open('portfolio_state.json', 'w'), indent=2)

print("\n=== Fixed Resolved ===")
total2 = 0
for r in state['resolved']:
    q = r.get('market_question', '')[:45]
    pnl = r.get('realized_pnl', 0)
    cost = r.get('cost_basis', 0)
    outcome = r.get('outcome', '?')
    total2 += pnl
    print(f"  {q} | {outcome} | cost: ${cost:.2f} | pnl: ${pnl:.2f}")
print(f"Total realized: ${total2:.2f}")
